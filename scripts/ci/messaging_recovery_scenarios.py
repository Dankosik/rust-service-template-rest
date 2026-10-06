"""Six native recovery situations, driven only through the owned Session.

The oracle is an independent expected-result inventory. Replay reads actual
restored outbox or stream bytes; an oracle copy is never a publication source.
"""

import base64
from datetime import datetime, timezone
import json
from pathlib import Path
import shutil
import time


def header(raw, name):
    """Read first identity value while retaining raw bytes for equality."""
    for line in base64.b64decode(raw.get("hdrs", "")).decode().split("\r\n")[1:]:
        key, separator, value = line.partition(":")
        if separator and key == name:
            return value.strip()
    return None


def record_meaning(raw):
    return {key: raw.get(key, "") for key in ("subject", "seq", "time", "hdrs", "data")}


class Rehearsal:
    def __init__(self, controller, path, artifacts):
        self.c = controller
        self.path = path.absolute()
        self.artifacts = artifacts.resolve()
        self.c.require(not self.path.exists(), "rehearsal_directory_must_be_new")
        for binary in ("messaging_recovery", "migrate"):
            self.c.require((self.artifacts / binary).is_file(), "combined_fixture_artifact_missing")
        started_at = time.time()
        self.deadline = started_at + self.c.SESSION_SECONDS
        admission = self.c.resource_admission(self.path.parent, deadline=self.deadline, cooldown=True)
        self.path.mkdir(mode=0o700)
        self.session = None
        self.oracle = {}
        self.results = {}
        self.manifest = {"version": 1, "started_at": started_at, "deadline": self.deadline,
                         "resource_admission": admission, "sessions": [], "scenarios": {}}
        self.save()

    def save(self):
        self.c.atomic(self.path / "rehearsal.json", self.manifest)

    def fresh(self, name, *, empty=False):
        self.finish_session()
        path = self.path / name
        self.manifest["sessions"].append(str(path))
        self.save()
        try:
            self.c.start(path, self.artifacts, postgres=True, empty_streams=empty,
                         deadline=self.deadline, budget_root=self.path)
        except BaseException:
            if (path / "session.json").exists():
                partial = self.c.Session(path)
                with partial.lock():
                    partial.stop()
            raise
        self.session = self.c.Session(path)
        return self.session

    def finish_session(self):
        if self.session:
            with self.session.lock():
                self.session.stop()
            self.session = None

    def wait(self, reason, predicate, seconds=30):
        until = min(time.monotonic() + seconds, time.monotonic() + self.deadline - time.time())
        while time.monotonic() < until:
            self.session.tick()
            try:
                value = predicate()
            except self.c.Refused as error:
                if str(error) not in {"native_command_failed", "native_command_deadline", "stream_information_unavailable"}:
                    raise
                value = False
            if value:
                return value
            time.sleep(0.2)
        raise self.c.Refused(reason)

    def database(self, name):
        s = self.session
        self.c.require(name in {"producer", "effect"}, "fixture_database")
        s.sql("app", f'CREATE DATABASE "{name}"')
        role = "publisher" if name == "producer" else "consumer"
        environment = s.worker_environment(role, name, "recovery_effect")
        s.client_exec("/artifacts/migrate", timeout=60, environment=environment)
        schema = s.path / "schema.sql"
        if not schema.exists():
            self.c.private_text(schema, (self.c.ROOT / "test/fixtures/messaging_recovery.sql").read_text())
        s.client_exec("psql", "-X", "-q", "-v", "ON_ERROR_STOP=1", "-d", name, "-f", "/session/schema.sql")

    def databases(self):
        self.database("producer")
        self.database("effect")

    def worker_permissions(self):
        s = self.session
        before = self.info()["config"]
        positive = s.request(f"$JS.API.STREAM.UPDATE.{s.data['source_stream']}", before)
        self.c.require("error" not in positive, "permission_probe_admin_control")
        args = ("/artifacts/nats", "--no-context", "--creds", "/session/worker.creds", "request", "--raw", "--no-templates")
        info = json.loads(s.client_exec(*args, f"$JS.API.STREAM.INFO.{s.data['source_stream']}", "{}")[1])
        self.c.require("error" not in info and info["config"] == before, "permission_probe_authenticated_read")
        code, _ = s.client_exec(*args, f"$JS.API.STREAM.UPDATE.{s.data['source_stream']}", json.dumps(before), check=False)
        self.c.require(code != 0 and self.info()["config"] == before, "worker_can_mutate_stream_topology")
        s.evidence("worker-permissions", {"authenticated_read": True, "same_update_admin_control": True,
                                         "worker_stream_update_refused": True, "raw_credentials": "withheld"})

    def produce(self, logical_id, *, counter="orders", delta=1, database="producer"):
        s = self.session
        occurred = "2026-10-06T12:00:00.123456789Z"
        payload = json.dumps({"counter_id": counter, "delta": delta}, separators=(",", ":")).encode()
        expected = {"logical_id": logical_id, "publication_id": logical_id,
                    "subject": s.data["subject"], "event_type": "recovery.counter.incremented",
                    "schema_version": 1, "occurred_at": occurred, "counter_id": counter, "delta": delta,
                    "payload_sha256": self.c.sha(payload), "payload_base64": base64.b64encode(payload).decode()}
        if logical_id in self.oracle:
            self.c.require(self.oracle[logical_id] == expected, "oracle_identity_conflict")
        else:
            self.oracle[logical_id] = expected
            self.c.atomic(self.path / "oracle.json", self.oracle)
        environment = s.path / ("publisher.env" if database == "producer" else "consumer.env")
        code, result = s.client_exec("/artifacts/messaging_recovery", "produce", logical_id, occurred,
                                     counter, str(delta), environment=environment, check=False)
        self.c.require(code == 0 and result.strip() in {b"intent_committed", b"producer_receipt_reconciled"},
                       "producer_commit_unresolved")
        return expected

    def jobs(self, database="producer"):
        return self.session.rows(database, "SELECT id::text, state, attempts, not_before::text, failure_reason, payload "
                                 "FROM background_jobs WHERE kind = 'publish_domain_event' ORDER BY id")

    def receipt_map(self, database="effect"):
        rows = self.session.rows(database, "SELECT logical_id, event_type, schema_version, occurred_at, counter_id, delta "
                                  "FROM recovery_effect_receipts ORDER BY logical_id")
        return {row["logical_id"]: row for row in rows}

    def counter_map(self, database="effect", producer=False):
        table = "recovery_producer_counters" if producer else "recovery_counters"
        return {row["counter_id"]: row["value"] for row in self.session.rows(database, f"SELECT * FROM {table} ORDER BY counter_id")}

    def assert_effects(self, ids, *, database="effect"):
        actual = self.receipt_map(database)
        self.c.require(set(actual) == set(ids), "effect_id_set_mismatch")
        totals = {}
        for logical_id in ids:
            expected = self.oracle[logical_id]
            row = actual[logical_id]
            for key in ("event_type", "schema_version", "occurred_at", "counter_id", "delta"):
                self.c.require(row[key] == expected[key], "effect_meaning_mismatch")
            totals[expected["counter_id"]] = totals.get(expected["counter_id"], 0) + expected["delta"]
        self.c.require(self.counter_map(database) == totals, "effect_counter_mismatch")
        return actual

    def effects(self, ids, *, seconds=30):
        self.wait("effects_not_established", lambda: set(self.receipt_map()) == set(ids), seconds)
        return self.assert_effects(ids)

    def info(self, stream=None):
        response = self.session.request(f"$JS.API.STREAM.INFO.{stream or self.session.data['source_stream']}", {})
        self.c.require(response is not None and "error" not in response, "stream_information_unavailable")
        return response

    def records(self, stream=None):
        stream = stream or self.session.data["source_stream"]
        state = self.info(stream)["state"]
        if state["messages"] == 0:
            return []
        self.c.require(state["last_seq"] - state["first_seq"] <= 10000, "rehearsal_scan_bound")
        records = []
        for sequence in range(state["first_seq"], state["last_seq"] + 1):
            response = self.session.request(f"$JS.API.STREAM.MSG.GET.{stream}", {"seq": sequence})
            if response.get("error", {}).get("err_code") == 10037:
                continue
            self.c.require("error" not in response, "retained_record_read_failed")
            records.append(record_meaning(response["message"]))
        self.c.require(len(records) == state["messages"], "stream_scan_changed_or_incomplete")
        return records

    def source_ids(self, records=None):
        observed = {}
        for raw in self.records() if records is None else records:
            logical_id = header(raw, "Message-Id")
            self.c.require(logical_id in self.oracle, "unaccounted_source_id")
            expected = self.oracle[logical_id]
            self.c.require(raw["subject"] == expected["subject"] and raw["data"] == expected["payload_base64"],
                           "source_payload_or_subject_changed")
            self.c.require(header(raw, "Event-Type") == expected["event_type"]
                           and header(raw, "Nats-Msg-Id") == expected["publication_id"]
                           and header(raw, "Event-Schema") == f"v{expected['schema_version']}"
                           and self.c.timestamp(header(raw, "Created-At")) == self.c.timestamp(expected["occurred_at"]),
                           "source_event_identity_changed")
            observed.setdefault(logical_id, []).append(raw["seq"])
        return observed

    def publications(self, ids):
        wanted = set(ids)
        self.wait("source_publications_missing", lambda: wanted <= set(self.source_ids()))
        jobs = self.wait("outbox_completion_unresolved", lambda: (rows := self.jobs())
                         and wanted <= {row["payload"]["message_id"] for row in rows}
                         and all(row["state"] == "completed" for row in rows if row["payload"]["message_id"] in wanted)
                         and rows)
        return jobs

    def consumers(self):
        response = self.session.request(f"$JS.API.CONSUMER.LIST.{self.session.data['source_stream']}", {"offset": 0})
        self.c.require("error" not in response and response.get("total", 0) <= 32, "consumer_inventory")
        fields = ("name", "created", "config", "delivered", "ack_floor", "num_ack_pending", "num_redelivered", "num_pending")
        return {item["name"]: {key: item[key] for key in fields} for item in response.get("consumers", [])}

    def quiesce(self):
        for name in list(self.session.data.get("workers", {})):
            self.session.worker_stop(name)
        self.session.verify()
        self.c.require(all(row["state"] != "running" for row in self.jobs()), "quiescent_jobs_still_claimed")

    def observation(self):
        return {
            "producer_events": self.session.rows("producer", "SELECT * FROM recovery_producer_events ORDER BY logical_id"),
            "producer_counters": self.counter_map("producer", producer=True), "outbox": self.jobs(),
            "source": self.records(), "dlq": self.records(self.session.data["dlq_stream"]),
            "consumers": self.consumers(), "receipts": self.receipt_map(), "effects": self.counter_map(),
        }

    def backup(self, name):
        self.quiesce()
        s = self.session
        directory = s.path / "backups" / name
        directory.mkdir(parents=True, mode=0o700)
        before = self.observation()
        started = time.monotonic()
        for stream in (s.data["source_stream"], s.data["dlq_stream"]):
            target = f"/session/backups/{name}/{stream}"
            s.nats("--timeout", "60s", "backup", "stream", "--consumers", "--no-progress", stream, target, timeout=65)
            s.nats("backup", "validate", "--no-progress", target, timeout=30)
        for database in ("producer", "effect"):
            target = f"/session/backups/{name}/{database}.dump"
            s.client_exec("pg_dump", "--format=custom", "--no-owner", "--file", target, database, timeout=60)
            s.client_exec("pg_restore", "--list", target)
        after = self.observation()
        self.c.require(before == after, "backup_boundary_was_not_quiescent")
        files = {str(file.relative_to(directory)): self.c.sha(file.read_bytes()) for file in directory.rglob("*") if file.is_file()}
        metadata = {"version": 1, "generation": s.data["generation"], "boundary": before,
                    "archive_sha256": files, "backup_seconds": time.monotonic() - started}
        self.c.atomic(directory / "boundary.json", metadata, exclusive=True)
        s.evidence("native-backup", {"name": name, "logical_ids": sorted(row["logical_id"] for row in before["producer_events"]),
                                     "archives": files, "seconds": metadata["backup_seconds"]})
        return directory

    def restore(self, name, broker, producer, effect):
        originals = {"broker": Path(broker), "producer": Path(producer), "effect": Path(effect)}
        expected = {key: self.c.load(path / "boundary.json") for key, path in originals.items()}
        self.fresh(name, empty=True)
        with self.session.lock():
            return self.restore_archives(originals, expected)

    def restore_archives(self, originals, expected):
        s = self.session
        started = time.monotonic()
        restore_dir = s.path / "restore"
        restore_dir.mkdir(mode=0o700)
        for role, path in originals.items():
            for relative, digest in expected[role]["archive_sha256"].items():
                self.c.require(self.c.sha((path / relative).read_bytes()) == digest, "native_archive_changed")
            shutil.copytree(path, restore_dir / role)
        with s.operation("native-restore"):
            self.c.require(not s.data["topology_frozen"], "restore_lifetime_frozen")
            for stream in (s.data["source_stream"], s.data["dlq_stream"]):
                absent = s.request(f"$JS.API.STREAM.INFO.{stream}", {})
                self.c.require(absent.get("error", {}).get("err_code") == 10059, "restore_requires_absent_stream")
                source = f"/session/restore/broker/{stream}"
                s.nats("backup", "validate", "--no-progress", source, timeout=30)
                s.nats("--timeout", "60s", "backup", "restore", "stream", "--no-progress", "--replicas", "3",
                       "--cluster", s.data["project"], source, timeout=65)
            for database in ("producer", "effect"):
                s.sql("app", f'CREATE DATABASE "{database}"')
                source = f"/session/restore/{database}/{database}.dump"
                s.client_exec("pg_restore", "--exit-on-error", "--no-owner", "--no-privileges", "--dbname", database, source, timeout=60)
                s.worker_environment("publisher" if database == "producer" else "consumer", database, "recovery_effect")
        self.wait("restored_r3_not_current", lambda: all(
            info.get("cluster", {}).get("leader") and len(info["cluster"].get("replicas", [])) == 2
            and all(replica["current"] for replica in info["cluster"]["replicas"])
            for info in (self.info(), self.info(s.data["dlq_stream"]))))
        restored = self.observation()
        broker_boundary = expected["broker"]["boundary"]
        self.c.require(restored["source"] == broker_boundary["source"] and restored["dlq"] == broker_boundary["dlq"],
                       "native_restore_record_bytes_changed")
        for consumer, prior in broker_boundary["consumers"].items():
            current = restored["consumers"].get(consumer)
            self.c.require(current is not None, "native_restore_consumer_missing")
            for field in ("ack_floor", "delivered"):
                for sequence in ("stream_seq", "consumer_seq"):
                    self.c.require(current[field][sequence] == prior[field][sequence], "native_restore_consumer_position")
            self.c.require(current["num_ack_pending"] == prior["num_ack_pending"], "native_restore_ack_pending")
        for key in ("producer_events", "producer_counters", "outbox"):
            self.c.require(restored[key] == expected["producer"]["boundary"][key], "producer_restore_boundary_changed")
        for key in ("receipts", "effects"):
            self.c.require(restored[key] == expected["effect"]["boundary"][key], "effect_restore_boundary_changed")
        s.evidence("native-restore", {"archives": {key: value["generation"] for key, value in expected.items()},
                                     "seconds": time.monotonic() - started, "before_replay": restored})
        return restored

    def result(self, name, status, details):
        boundary = self.observation()
        source_ids = self.source_ids(boundary["source"])
        jobs = {row["payload"]["message_id"]: row for row in boundary["outbox"]}
        producer_ids = {row["logical_id"] for row in boundary["producer_events"]}
        dlq_ids = {}
        for raw in boundary["dlq"]:
            logical_id = header(raw, "Message-Id")
            self.c.require(logical_id is not None, "dlq_identity_missing")
            dlq_ids.setdefault(logical_id, []).append(raw["seq"])
        known = producer_ids | set(source_ids) | set(boundary["receipts"]) | set(dlq_ids)
        accounting = {logical_id: {
            "producer_receipt": logical_id in producer_ids,
            "source_sequences": source_ids.get(logical_id, []),
            "dlq_sequences": dlq_ids.get(logical_id, []),
            "outbox_state": jobs.get(logical_id, {}).get("state"),
            "effect_receipt": logical_id in boundary["receipts"],
        } for logical_id in sorted(known)}
        self.results[name] = {"status": status, **details, "per_id": accounting, "boundary": boundary}
        self.manifest["scenarios"][name] = self.results[name]
        self.save()
        self.session.evidence(name, self.results[name])

    def archive_scenarios(self):
        s = self.fresh("archive-source")
        with s.lock():
            self.databases()
            self.worker_permissions()
            self.produce("coherent-existing")
            # Same-ID producer replay must reconcile its original transaction.
            self.produce("coherent-existing")
            self.c.require(len(self.jobs()) == 1 and self.counter_map("producer", producer=True) == {"orders": 1},
                           "producer_retry_repeated_business_or_enqueue")
            conflict, _ = s.client_exec("/artifacts/messaging_recovery", "produce", "coherent-existing",
                                       "2026-10-06T12:00:00.123456789Z", "orders", "2",
                                       environment=s.path / "publisher.env", check=False)
            self.c.require(conflict != 0 and len(self.jobs()) == 1 and self.counter_map("producer", producer=True) == {"orders": 1},
                           "producer_conflicting_retry_changed_business_or_intent")
            s.worker_start("publisher", "producer")
            self.publications(["coherent-existing"])
            s.worker_start("consumer", "effect")
            self.effects(["coherent-existing"])
            self.quiesce()
            s.seed("archive-dlq")
            dlq_ids = [header(raw, "Message-Id") for raw in self.records(s.data["dlq_stream"])]
            self.produce("coherent-pending")
            coherent = self.backup("coherent")
            self.produce("deleted-terminal")
            s.worker_start("publisher", "producer")
            self.publications(["coherent-existing", "coherent-pending", "deleted-terminal"])
            s.worker_start("consumer", "effect")
            self.effects(["coherent-existing", "coherent-pending", "deleted-terminal"])
            self.quiesce()
            deleted = json.loads(s.sql(
                                 "producer", "WITH removed AS (DELETE FROM background_jobs WHERE state = 'completed' "
                                 "AND payload->>'message_id' = 'deleted-terminal' RETURNING id::text) "
                                 "SELECT coalesce(json_agg(removed), '[]'::json) FROM removed"))
            self.c.require(len(deleted) == 1, "declared_terminal_retention_not_applied")
            s.evidence("declared-terminal-retention", {"logical_ids": ["deleted-terminal"], "removed_job_ids": deleted})
            self.produce("newer-pending")
            newer = self.backup("newer")
        restored = self.restore("coherent-restore", coherent, coherent, coherent)
        s = self.session
        with s.lock():
            self.c.require(set(restored["receipts"]) == {"coherent-existing"}, "coherent_existing_effect")
            s.worker_start("publisher", "producer")
            self.publications(["coherent-existing", "coherent-pending"])
            s.worker_start("consumer", "effect")
            self.effects(["coherent-existing", "coherent-pending"])
            self.quiesce()
            before = self.receipt_map()
            s.worker_start("replay", "effect", durable="coherent_replay")
            self.wait("coherent_replay_not_acknowledged", lambda: self.consumers().get("coherent_replay", {}).get("num_pending") == 0
                      and self.consumers()["coherent_replay"]["num_ack_pending"] == 0)
            s.worker_stop("replay")
            self.c.require(self.receipt_map() == before, "replay_duplicated_existing_effect")
            self.assert_effects(["coherent-existing", "coherent-pending"])
            self.result("coherent", "recovered", {"ids": sorted(before), "pre_existing": ["coherent-existing"],
                        "newly_applied": ["coherent-pending"], "duplicate_suppressed": sorted(before), "dlq_retained_ids": dlq_ids,
                        "dlq_bytes_restored": True})
        restored = self.restore("older-broker", coherent, newer, newer)
        s = self.session
        with s.lock():
            present = set(self.source_ids(restored["source"]))
            jobs = {row["payload"]["message_id"]: row for row in restored["outbox"]}
            missing = sorted(row["logical_id"] for row in restored["producer_events"] if row["logical_id"] not in present)
            classification = {logical_id: "publishable_retained_intent" if logical_id in jobs and jobs[logical_id]["state"] == "pending"
                              else "terminal_retained_bytes_reconciliation_required" if logical_id in jobs
                              else "terminal_or_deleted_intent_missing_bytes" for logical_id in missing}
            self.c.require(classification == {"coherent-pending": "terminal_retained_bytes_reconciliation_required",
                           "deleted-terminal": "terminal_or_deleted_intent_missing_bytes", "newer-pending": "publishable_retained_intent"},
                           "mismatched_producer_authority_classification")
            s.worker_start("publisher", "producer")
            self.publications(["newer-pending"])
            s.worker_start("consumer", "effect")
            self.effects(["coherent-existing", "coherent-pending", "deleted-terminal", "newer-pending"])
            self.quiesce()
            self.c.require(set(self.source_ids()) == {"coherent-existing", "newer-pending"}, "missing_authority_was_silently_recreated")
            self.result("older_broker", "permitted_stop", {"classification": classification, "replayed_from_live_outbox": ["newer-pending"],
                        "missing_bytes": ["deleted-terminal"], "requires_reconciliation": ["coherent-pending"], "lossless": False})
        restored = self.restore("older-effect", newer, newer, coherent)
        s = self.session
        with s.lock():
            floor = restored["consumers"]["recovery_effect"]["ack_floor"]["stream_seq"]
            passed = {header(raw, "Message-Id") for raw in restored["source"] if raw["seq"] <= floor}
            absent = passed - set(restored["receipts"])
            self.c.require(absent == {"coherent-pending", "deleted-terminal"}, "older_effect_missing_ids")
            s.worker_start("consumer", "effect")
            self.c.require(self.consumers()["recovery_effect"]["num_pending"] == 0 and set(self.receipt_map()) == {"coherent-existing"},
                           "ordinary_resume_misrepresented_recovery")
            s.worker_stop("consumer")
            s.worker_start("replay", "effect", durable="effect_replay")
            self.effects(sorted(passed))
            self.quiesce()
            self.result("older_effect", "recovered_by_explicit_replay", {"newer_ack_floor": floor, "initial_missing_ids": sorted(absent),
                        "replay_durable": "effect_replay", "replay_position": "all_retained", "ids": sorted(passed),
                        "pre_existing_effect": ["coherent-existing"], "producer_pending_not_yet_published": ["newer-pending"]})

    def retained_publication(self, logical_id):
        """A native publication probe reads only real outbox bytes, never oracle bytes."""
        rows = [row for row in self.jobs() if row["payload"]["message_id"] == logical_id]
        self.c.require(len(rows) == 1 and rows[0]["state"] in {"pending", "running", "failed"}, "no_publishable_retained_intent")
        intent = rows[0]["payload"]
        self.c.require(intent["version"] == 1, "retained_intent_version")
        payload = base64.b64decode(intent["payload_base64"], validate=True)
        audit = self.session.rows("producer", "SELECT logical_id, payload_sha256 FROM recovery_producer_events ORDER BY logical_id")
        self.c.require(any(row["logical_id"] == logical_id and row["payload_sha256"] == self.c.sha(payload) for row in audit),
                       "retained_intent_business_digest")
        occurred = datetime.fromtimestamp(intent["occurred_at_unix_seconds"], timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")
        fraction = f"{intent['occurred_at_nanosecond']:09d}".rstrip("0")
        occurred += ("." + fraction if fraction else "") + "Z"
        response = self.session.request_bytes(intent["subject"], payload, check=False, headers=(
            ("Message-Id", intent["message_id"]), ("Nats-Msg-Id", intent["publication_id"]),
            ("Event-Type", intent["event_type"]), ("Event-Schema", f"v{intent['schema_version']}"),
            ("Created-At", occurred), ("Nats-Expected-Stream", self.session.data["source_stream"])))
        state = "ambiguous" if response is None else "rejected" if "error" in response else "confirmed"
        if state == "confirmed":
            self.c.require(response.get("stream") == self.session.data["source_stream"] and response.get("seq", 0) > 0, "wrong_puback")
        self.session.evidence("native-publication", {"logical_id": logical_id, "publication_id": intent["publication_id"],
                              "outcome": state, "native_result": response, "authority": "live_outbox_row", "job_id": rows[0]["id"]})
        return state, response

    def one_node_loss(self):
        s = self.fresh("one-node-loss")
        with s.lock():
            self.databases()
            ids = ["fault-before-a", "fault-before-b", "fault-during", "fault-after"]
            for logical_id in ids:
                self.produce(logical_id)
            outcomes = {logical_id: self.retained_publication(logical_id)[0] for logical_id in ids[:2]}
            self.c.require(all(value == "confirmed" for value in outcomes.values()), "pre_fault_puback_missing")
            before = self.records()
            leader = self.info()["cluster"]["leader"]
            resources = s.resources()
            selected = [name for name, item in resources.items() if name.startswith("nats") and item["Config"]["Hostname"] == leader]
            self.c.require(len(selected) == 1, "leader_not_owned")
            s.data["stopped_nodes"] = selected
            s.save()
            fault_at = time.monotonic()
            self.c.run(["docker", "kill", s.data["containers"][selected[0]]["id"]], timeout=10)
            outcomes["fault-during"] = self.retained_publication("fault-during")[0]
            self.wait("remaining_quorum_not_recovered", lambda: self.info().get("cluster", {}).get("leader") != leader
                      and sum(replica.get("current", False) for replica in self.info()["cluster"]["replicas"]) >= 1)
            after = self.records()
            by_sequence = {row["seq"]: row for row in after}
            self.c.require(all(by_sequence.get(row["seq"]) == row for row in before), "FAIL_acknowledged_in_retention_bytes_lost")
            outcomes["fault-after"] = self.retained_publication("fault-after")[0]
            s.worker_start("publisher", "producer")
            self.publications(ids)
            s.worker_start("consumer", "effect")
            self.effects(ids)
            self.quiesce()
            self.c.require(set(self.source_ids()) == set(ids), "fault_id_accounting")
            self.result("one_node_loss", "recovered", {"killed_leader": leader, "remaining_nodes": 2,
                        "initial_publication_outcomes": outcomes, "ids": ids, "catch_up_seconds": time.monotonic() - fault_at,
                        "independent_zone_claim": False})

    def change_stream(self, **changes):
        self.session.verify()
        info = self.info()
        config = {**info["config"], **changes}
        response = self.session.request(f"$JS.API.STREAM.UPDATE.{self.session.data['source_stream']}", config)
        self.c.require("error" not in response, "owned_stream_configuration_refused")
        observed = self.info()
        self.c.require(all(observed["config"].get(key) == value for key, value in changes.items()), "effective_limit_mismatch")
        self.session.evidence("owned-stream-limit", {"before": info["config"], "after": observed["config"]})
        return observed

    def capacity(self):
        s = self.fresh("capacity")
        with s.lock():
            self.databases()
            self.change_stream(max_bytes=2048, discard="new")
            ids = []
            refused = None
            for index in range(8):
                logical_id = f"capacity-{index}"
                ids.append(logical_id)
                self.produce(logical_id, counter="capacity-" + "x" * 768)
                state, response = self.retained_publication(logical_id)
                if state == "rejected":
                    refused = response
                    break
                self.c.require(state == "confirmed", "capacity_probe_ambiguous_before_boundary")
            self.c.require(refused is not None, "capacity_boundary_not_reached")
            description = refused["error"].get("description", "").lower()
            self.c.require("maximum bytes" in description or "storage" in description, "unexpected_capacity_refusal")
            full = self.info()
            self.c.require(full["config"]["max_bytes"] == 2048 and full["config"]["discard"] == "new"
                           and full["state"]["bytes"] > 0, "capacity_evidence_missing")
            s.worker_start("publisher", "producer")
            self.wait("outbox_did_not_retain_failed_publication", lambda: any(row["payload"]["message_id"] == ids[-1]
                      and row["attempts"] > 0 and row["state"] != "completed" for row in self.jobs()))
            retained = self.jobs()
            returned_at = time.monotonic()
            self.change_stream(max_bytes=16 * 1024 ** 2)
            self.publications(ids)
            s.worker_start("consumer", "effect")
            self.effects(ids)
            self.quiesce()
            self.result("capacity", "recovered", {"boundary": "stream_max_bytes_discard_new", "native_refusal": refused,
                        "full_stream_state": full["state"], "retained_outbox": retained, "ids": ids,
                        "catch_up_seconds": time.monotonic() - returned_at, "host_disk_full_claim": False})

    def retention(self):
        s = self.fresh("retention")
        with s.lock():
            self.databases()
            s.worker_start("consumer", "effect")
            s.worker_stop("consumer")
            self.change_stream(max_age=8_000_000_000)
            self.produce("retention-expired")
            s.worker_start("publisher", "producer")
            self.publications(["retention-expired"])
            s.worker_stop("publisher")
            retained_before = self.records()
            self.c.require(set(self.source_ids(retained_before)) == {"retention-expired"}, "retention_initial_ack_bytes")
            self.wait("declared_retention_did_not_expire", lambda: self.info()["state"]["messages"] == 0, seconds=25)
            expiration = self.info()
            self.change_stream(max_age=60_000_000_000)
            retained = ["retention-kept-a", "retention-kept-b"]
            for logical_id in retained:
                self.produce(logical_id)
            s.worker_start("publisher", "producer")
            self.publications(retained)
            resume_at = time.monotonic()
            s.worker_start("consumer", "effect")
            self.effects(retained)
            self.quiesce()
            actual = set(self.source_ids())
            self.c.require(actual == set(retained) and "retention-expired" not in self.receipt_map(), "retention_missing_identity_hidden")
            self.result("retention_outage", "permitted_stop", {"caught_up": retained, "expired_missing": ["retention-expired"],
                        "expiration_boundary": expiration, "pre_expiration_records": retained_before,
                        "catch_up_seconds": time.monotonic() - resume_at, "lossless": False})

    def execute(self):
        try:
            self.archive_scenarios()
            self.one_node_loss()
            self.capacity()
            self.retention()
            self.c.require(set(self.results) == {"coherent", "older_broker", "older_effect", "one_node_loss", "capacity", "retention_outage"},
                           "unverified_scenario")
            self.manifest["status"] = "observed"
            self.manifest["duration_seconds"] = time.time() - self.manifest["started_at"]
            self.save()
            return self.manifest
        except BaseException:
            self.manifest["status"] = "unverified_or_failed"
            self.save()
            raise
        finally:
            self.finish_session()


def rehearse(controller, path, artifacts):
    return Rehearsal(controller, path, artifacts).execute()
