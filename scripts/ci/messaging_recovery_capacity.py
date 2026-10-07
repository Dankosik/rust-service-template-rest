"""Capacity observation in the existing owned Session; no alternate delivery path.

Counters use actual adjacent database-observation timestamps. PubAck-confirmed
IDs are completed native outbox jobs, a conservative durable lower bound (a
PubAck whose completion is unresolved stays in the retained/ambiguous set).
"""

from collections import Counter
import csv
from datetime import datetime, timezone
import json
import math
import os
import re
import time

from messaging_recovery_scenarios import Rehearsal, header


STAGES = (
    ("baseline", 12, 2, 1024), ("steady", 12, 10, 1024),
    ("ramp_50", 12, 50, 1024), ("ramp_150", 12, 150, 1024),
    ("ramp_350", 8, 350, 1024), ("burst_450", 4, 450, 1024),
    ("drain_1k", 40, 0, 1024), ("slice_64k", 8, 8, 65536),
    ("drain_64k", 30, 0, 1024), ("outage", 10, 10, 1024),
    ("catch_up", 90, 0, 1024),
)
COUNTS = ("offered", "admitted", "puback_confirmed", "applied")
METRIC_PREFIXES = ("db_client_connection_", "db_client_operation_duration_seconds",
                   "postgres_transaction_duration_seconds", "messaging_", "jobs_")
METRIC = re.compile(r'^([a-zA-Z_:][a-zA-Z_0-9:]*(?:\{[^\n]*\})?)\s+([-+0-9.eE]+|NaN|[+-]?Inf)(?:\s+\d+)?$')


def rates(previous, current):
    """Only same clock, monotonic cumulative counters form an interval rate."""
    if previous is None:
        return {key + "_per_second": None for key in COUNTS}
    seconds = current["observed_unix"] - previous["observed_unix"]
    if seconds <= 0 or any(current[key] < previous[key] for key in COUNTS):
        raise ValueError("nonmonotonic_measurement")
    return {key + "_per_second": (current[key] - previous[key]) / seconds for key in COUNTS}


def metrics(raw):
    """Export only the existing bounded metric families; never retain raw logs."""
    result = {}
    for line in raw.decode().splitlines():
        match = METRIC.fullmatch(line)
        if match and match[1].startswith(METRIC_PREFIXES):
            value = float(match[2])
            if math.isfinite(value):
                result[match[1]] = value
    return result


def inventory(plan):
    result = {}
    elapsed = 0
    for stage in plan["stages"]:
        for index in range(stage["seconds"] * stage["rate"]):
            ordinal = len(result)
            logical_id = f"capacity-{ordinal:06}"
            result[logical_id] = {"logical_id": logical_id, "publication_id": logical_id,
                                  "phase": stage["name"], "counter_id": f"counter-{ordinal:06}",
                                  "delta": 1, "payload_bytes": stage["payload_bytes"],
                                  "scheduled_unix_ms": plan["start_unix_ms"] + elapsed * 1000
                                                       + index * 1000 // stage["rate"]}
        elapsed += stage["seconds"]
    return result


def expected_body(expected):
    compact = json.dumps({"counter_id": expected["counter_id"], "delta": expected["delta"]}, separators=(",", ":"))
    return ("{" + " " * (expected["payload_bytes"] - len(compact.encode())) + compact[1:]).encode()


def complete_lines(path):
    if not path.exists():
        return []
    if path.stat().st_size > 16 * 1024 * 1024:
        raise ValueError("observation_file_limit")
    raw = path.read_bytes()
    return [json.loads(line) for line in raw[:raw.rfind(b"\n") + 1].splitlines()]


def phase_at(plan, now):
    elapsed = (now * 1000 - plan["start_unix_ms"]) / 1000
    if elapsed < 0:
        return "warmup"
    for stage in plan["stages"]:
        if elapsed < stage["seconds"]:
            return stage["name"]
        elapsed -= stage["seconds"]
    return "settle"


class Measurement(Rehearsal):
    def __init__(self, controller, path, artifacts):
        super().__init__(controller, path, artifacts)
        self.samples = []
        self.previous_ids = {key: set() for key in ("admitted", "puback_confirmed", "applied")}
        self.outage = {}
        self.shared_failure = {}
        self.plan = None
        self.expected = {}
        self.final_ids = {}

    def save(self):
        self.c.atomic(self.path / "capacity.json", self.manifest)

    def pause(self):
        s = self.session
        s.verify()
        s.data["paused_nodes"] = ["nats1", "nats2", "nats3"]
        s.save()  # Persist custody before the possibly partial native effect.
        self.outage["pause_invoked_unix"] = time.time()
        self.c.run(["docker", "pause", *[s.data["containers"][name]["id"] for name in s.data["paused_nodes"]]], timeout=10)
        state = s.resources()
        self.c.require(all(state[name]["State"].get("Paused") for name in s.data["paused_nodes"]), "broker_pause_unobserved")
        self.outage["paused_unix"] = time.time()
        s.evidence("capacity-outage-start", self.outage)

    def unpause(self):
        s = self.session
        if not s or not s.data.get("paused_nodes"):
            return
        state = s.resources()
        ids = []
        for name in s.data["paused_nodes"]:
            self.c.require(state[name]["Id"] == s.data["containers"][name]["id"], "paused_broker_identity_changed")
            if state[name]["State"].get("Paused"):
                ids.append(state[name]["Id"])
        if ids:
            self.c.run(["docker", "unpause", *ids], timeout=10)
        s.data["paused_nodes"] = []
        s.save()
        self.outage["resumed_unix"] = time.time()
        self.outage["seconds"] = self.outage["resumed_unix"] - self.outage.get("paused_unix", self.outage["pause_invoked_unix"])
        s.evidence("capacity-outage-end", self.outage)

    def database_sample(self):
        s = self.session
        # One statement snapshot, no payload bodies or query text in evidence.
        return s.rows("producer", """
            SELECT extract(epoch FROM clock_timestamp())::float8 AS observed_unix,
              (SELECT coalesce(json_agg(logical_id ORDER BY logical_id), '[]') FROM recovery_producer_events) AS admitted_ids,
              (SELECT coalesce(json_agg(payload->>'message_id' ORDER BY payload->>'message_id'), '[]')
                 FROM background_jobs WHERE kind='publish_domain_event' AND state='completed') AS confirmed_ids,
              (SELECT coalesce(json_agg(logical_id ORDER BY logical_id), '[]') FROM recovery_effect_receipts) AS applied_ids,
              (SELECT count(*) FROM background_jobs WHERE kind='publish_domain_event' AND state IN ('pending','running')) AS backlog,
              (SELECT coalesce(max(extract(epoch FROM clock_timestamp()-created_at)),0)::float8
                 FROM background_jobs WHERE kind='publish_domain_event' AND state IN ('pending','running')) AS oldest_queue_seconds,
              (SELECT count(*) FROM background_jobs WHERE kind='publish_domain_event' AND state='running') AS publisher_running,
              (SELECT coalesce(sum(greatest(attempts-1,0)),0) FROM background_jobs WHERE kind='publish_domain_event') AS retry_attempts,
              (SELECT count(*) FROM background_jobs WHERE kind='publish_domain_event' AND state='failed') AS retained_failures,
              (SELECT count(*) FROM background_jobs WHERE kind='test.probe' AND state='completed') AS jobs_completed,
              (SELECT count(*) FROM background_jobs WHERE kind='test.probe' AND state IN ('pending','running')) AS jobs_backlog,
              (SELECT coalesce(json_agg(json_build_object('id',j.id::text,'created_unix',extract(epoch FROM j.created_at),
                        'started_unix',extract(epoch FROM p.started_at))), '[]')
                 FROM background_jobs j JOIN probe_attempts p ON p.job_id=j.id WHERE j.kind='test.probe') AS job_attempts,
              (SELECT count(*) FROM pg_stat_activity WHERE datname='producer' AND state='active') AS db_active,
              (SELECT count(*) FROM pg_stat_activity WHERE datname='producer' AND wait_event_type='Lock') AS db_lock_waiters,
              (SELECT coalesce(max(extract(epoch FROM clock_timestamp()-xact_start)),0)::float8
                 FROM pg_stat_activity WHERE datname='producer' AND pid<>pg_backend_pid()) AS db_oldest_transaction_seconds,
              (SELECT count(*) FROM pg_locks WHERE NOT granted) AS db_ungranted_locks,
              (SELECT wal_bytes::float8 FROM pg_stat_wal) AS wal_bytes,
              (SELECT json_build_object('commits',xact_commit,'rollbacks',xact_rollback,'blocks_read',blks_read,
                        'blocks_hit',blks_hit,'temp_bytes',temp_bytes,'deadlocks',deadlocks)
                 FROM pg_stat_database WHERE datname='producer') AS database
        """)[0]

    def broker_sample(self):
        s = self.session
        if s.data.get("paused_nodes"):
            return {"available": False, "reason": "owned_brokers_paused"}
        stream = self.info()
        consumer = s.request(f"$JS.API.CONSUMER.INFO.{s.data['source_stream']}.recovery_effect", {})
        self.c.require("error" not in consumer, "measurement_consumer_information_unavailable")
        native = {}
        for name in ("nats1", "nats2", "nats3"):
            raw = self.c.run(["docker", "exec", s.data["containers"][name]["id"], "wget", "-q", "-O", "-",
                              "http://127.0.0.1:8222/varz"], timeout=5)[1]
            value = json.loads(raw)
            native[name] = {key: value.get(key) for key in ("cpu", "mem", "connections", "slow_consumers", "in_msgs", "out_msgs", "in_bytes", "out_bytes")}
        return {"available": True, "source_state": stream["state"], "stream_config": stream["config"],
                "replication": stream.get("cluster"), "brokers": native,
                "consumer": {key: consumer.get(key) for key in ("config", "num_ack_pending", "num_pending", "num_redelivered", "delivered", "ack_floor", "cluster")}}

    def sample(self):
        began = time.time()
        s = self.session
        s.tick()
        row = self.database_sample()
        row["phase"] = phase_at(self.plan, row["observed_unix"])
        row["offered"] = sum(item["scheduled_unix_ms"] <= row["observed_unix"] * 1000 for item in self.expected.values())
        for key, source in (("admitted", "admitted_ids"), ("puback_confirmed", "confirmed_ids"), ("applied", "applied_ids")):
            ids = set(row.pop(source))
            self.c.require(ids <= self.expected.keys() and self.previous_ids[key] <= ids, "measurement_identity_or_counter_changed")
            row[key] = len(ids)
            row[key + "_new_ids"] = sorted(ids - self.previous_ids[key])
            self.previous_ids[key] = ids
        row["effect_backlog"] = row["admitted"] - row["applied"]
        row.update(rates(self.samples[-1] if self.samples else None, row))
        observations = complete_lines(s.path / "load.jsonl")
        row["generator_outcomes"] = dict(Counter(item["outcome"] for item in observations if item.get("kind") == "event"))
        metric_code, raw = s.client_exec("/artifacts/messaging_recovery", "metrics", timeout=5, check=False)
        row["metrics"] = metrics(raw) if metric_code == 0 else {}
        row["metrics_available"] = metric_code == 0 and bool(row["metrics"])
        try:
            row["broker"] = self.broker_sample()
        except self.c.Refused as error:
            if str(error) not in {"native_command_failed", "native_command_deadline", "stream_information_unavailable",
                                  "measurement_consumer_information_unavailable"}:
                raise
            row["broker"] = {"available": False, "reason": str(error)}
        ids = [item["id"] for item in s.data["containers"].values()]
        raw = self.c.run(["docker", "stats", "--no-stream", "--format", "{{json .}}", *ids], timeout=10)[1]
        by_id = {item["id"][:12]: name for name, item in s.data["containers"].items()}
        row["containers"] = {}
        for line in raw.splitlines():
            stat = json.loads(line)
            name = by_id.get(stat["ID"][:12])
            self.c.require(name is not None, "unowned_statistics")
            row["containers"][name] = {key: stat[key] for key in ("CPUPerc", "MemUsage", "MemPerc", "BlockIO", "NetIO", "PIDs")}
        row["host_load"] = os.getloadavg()
        row["collection_seconds"] = time.time() - began
        self.samples.append(row)
        s.evidence("capacity-sample", row)
        self.write_timeseries()
        if "resumed_unix" in self.outage and "caught_up_unix" not in self.outage and row["phase"] in {"catch_up", "settle"}:
            if row["admitted"] == row["puback_confirmed"] == row["applied"] and row["offered"] == len(self.expected):
                self.outage["caught_up_unix"] = row["observed_unix"]
                self.outage["catch_up_seconds"] = row["observed_unix"] - self.outage["resumed_unix"]
        return row

    def write_timeseries(self):
        fields = ("observed_unix", "phase", *COUNTS, *(key + "_per_second" for key in COUNTS),
                  "backlog", "effect_backlog", "oldest_queue_seconds", "publisher_running", "retry_attempts",
                  "retained_failures", "jobs_completed", "jobs_backlog", "db_active", "db_lock_waiters",
                  "db_ungranted_locks", "db_oldest_transaction_seconds", "wal_bytes", "metrics_available", "collection_seconds")
        with (self.session.path / "timeseries.csv").open("w", newline="") as output:
            writer = csv.DictWriter(output, fieldnames=fields, extrasaction="ignore")
            writer.writeheader()
            writer.writerows(self.samples)

    def shared_admission_failure(self):
        s = self.session
        s.worker_stop("worker")
        # The provider's existing ACK admission rejects this native topology.
        original = self.info()["config"]
        self.change_stream(no_ack=True)
        try:
            raw = s.client_exec("/artifacts/messaging_recovery", "probe", environment=s.path / "worker.env")[1]
            job_id = raw.decode().strip()
            self.c.require(re.fullmatch(r"[0-9a-f-]{36}", job_id), "ordinary_probe_identity")
            began = time.time()
            try:
                s.worker_start("worker", "producer")
            except self.c.Refused as error:
                self.c.require(str(error) == "worker_start_failed", "shared_role_failure_unattributed")
            else:
                raise self.c.Refused("unsafe_messaging_admission_started_shared_jobs")
            worker = s.data["workers"]["worker"]
            running = self.c.native_json(["docker", "inspect", s.data["containers"]["client"]["id"]])[0].get("ExecIDs") or []
            self.c.require(worker["exec_id"] not in running, "failed_shared_worker_still_alive")
            del s.data["workers"]["worker"]
            s.save()
            observed = s.rows("producer", "SELECT state,(SELECT count(*) FROM probe_attempts WHERE job_id=j.id) AS attempts "
                              f"FROM background_jobs j WHERE id='{job_id}'::uuid")[0]
            self.c.require(observed == {"state": "pending", "attempts": 0}, "ordinary_probe_escaped_failed_admission")
            self.shared_failure = {"cause": "native_source_no_ack_admission_refused", "job_id": job_id,
                                   "began_unix": began, "refused_unix": time.time(), "pending_probe": observed,
                                   "ordinary_jobs_interrupted": True}
        finally:
            self.change_stream(no_ack=original.get("no_ack", False))
        s.worker_start("worker", "producer")
        self.wait("ordinary_probe_did_not_recover", lambda: s.sql("producer", f"SELECT state FROM background_jobs WHERE id='{job_id}'::uuid") == "completed", 30)
        self.shared_failure["recovered_unix"] = time.time()
        self.shared_failure["recovery_seconds"] = self.shared_failure["recovered_unix"] - self.shared_failure["refused_unix"]
        s.evidence("shared-admission-failure", self.shared_failure)

    def reconcile(self):
        s = self.session
        producers = s.rows("producer", "SELECT logical_id,payload_sha256,occurred_at FROM recovery_producer_events ORDER BY logical_id")
        effects = s.rows("producer", "SELECT logical_id,event_type,schema_version,occurred_at,counter_id,delta FROM recovery_effect_receipts ORDER BY logical_id")
        jobs = s.rows("producer", "SELECT payload->>'message_id' AS logical_id,state,attempts,failure_reason FROM background_jobs WHERE kind='publish_domain_event' ORDER BY id")
        observations = complete_lines(s.path / "load.jsonl")
        actual = {row["logical_id"] for row in producers}
        expected_applied = {}
        for row in producers:
            wanted = self.expected[row["logical_id"]]
            self.c.require(row["payload_sha256"] == self.c.sha(expected_body(wanted)), "admitted_wire_digest_changed")
            expected_applied[row["logical_id"]] = {"counter_id": wanted["counter_id"], "delta": wanted["delta"], "occurred_at": row["occurred_at"]}
        for row in effects:
            wanted = expected_applied.get(row["logical_id"])
            self.c.require(wanted is not None and all(row[key] == value for key, value in wanted.items())
                           and row["event_type"] == "recovery.counter.incremented" and row["schema_version"] == 1,
                           "capacity_effect_identity_changed")
        totals = {row["counter_id"]: row["value"] for row in s.rows("producer", "SELECT * FROM recovery_counters")}
        producer_totals = {row["counter_id"]: row["value"] for row in s.rows("producer", "SELECT * FROM recovery_producer_counters")}
        self.c.require(totals == {row["counter_id"]: row["delta"] for row in effects}, "capacity_effect_duplicate_or_loss")
        self.c.require(producer_totals == {row["counter_id"]: row["delta"] for row in expected_applied.values()}, "capacity_business_intent_mismatch")
        self.c.require({row["logical_id"] for row in jobs} == actual and len(jobs) == len(actual), "capacity_outbox_identity_mismatch")
        source = {}
        if actual:
            s.client_exec("/artifacts/messaging_recovery", "audit", "/session/source-audit.jsonl",
                          environment=s.path / "worker.env", timeout=130)
            for row in complete_lines(s.path / "source-audit.jsonl"):
                logical_id = header(row, "Message-Id")
                wanted = self.expected.get(logical_id)
                self.c.require(wanted is not None and logical_id in actual, "unaccounted_capacity_source_id")
                self.c.require(row["subject"] == s.data["subject"] and header(row, "Nats-Msg-Id") == logical_id
                               and header(row, "Event-Type") == "recovery.counter.incremented" and header(row, "Event-Schema") == "v1"
                               and self.c.timestamp(header(row, "Created-At")) == self.c.timestamp(expected_applied[logical_id]["occurred_at"])
                               and row["payload_bytes"] == wanted["payload_bytes"] and row["payload_sha256"] == self.c.sha(expected_body(wanted))
                               and row["meaning"] == {"counter_id": wanted["counter_id"], "delta": wanted["delta"]}, "capacity_wire_identity_changed")
                source.setdefault(logical_id, []).append({key: row[key] for key in ("seq", "payload_bytes", "header_bytes", "payload_sha256")})
        confirmed = {row["logical_id"] for row in jobs if row["state"] == "completed"}
        applied = {row["logical_id"] for row in effects}
        self.c.require(confirmed <= source.keys() and applied <= source.keys(), "capacity_ack_or_effect_without_source")
        event_observations = [row for row in observations if row.get("kind") == "event"]
        observed_ids = {row["id"] for row in event_observations}
        self.c.require(observed_ids == self.expected.keys() and len(event_observations) == len(self.expected), "incomplete_generator_accounting")
        for row in event_observations:
            self.c.require(row.get("payload_sha256", self.c.sha(expected_body(self.expected[row["id"]])))
                           == self.c.sha(expected_body(self.expected[row["id"]])), "generator_wire_digest_changed")
            if row["outcome"] in {"intent_committed", "producer_receipt_reconciled"}:
                self.c.require(row["id"] in actual, "acknowledged_producer_commit_missing")
        self.final_ids = {"offered": list(self.expected.values()), "producer_observations": observations,
                          "admitted": producers, "outbox": jobs, "source": source, "applied": effects,
                          "not_admitted": sorted(self.expected.keys() - actual),
                          "not_puback_confirmed": sorted(actual - confirmed), "not_applied": sorted(actual - applied),
                          "source_missing": sorted(actual - source.keys()),
                          "exact_catch_up": actual == confirmed == applied == source.keys()}
        self.c.atomic(s.path / "IDs.json", self.final_ids)

    def report(self):
        ordinary = []
        for sample in self.samples:
            for attempt in sample["job_attempts"]:
                ordinary.append((attempt["id"], phase_at(self.plan, attempt["created_unix"]),
                                 attempt["started_unix"] - attempt["created_unix"]))
        ordinary = sorted(set(ordinary))
        by_phase = {}
        for _, phase, latency in ordinary:
            by_phase.setdefault(phase, []).append(latency)
        summaries = {}
        for phase, values in by_phase.items():
            values.sort()
            summaries[phase] = {"attempts": len(values), "min_seconds": min(values), "max_seconds": max(values),
                                "p95_seconds": values[math.ceil(len(values) * 0.95) - 1]}
        stable = any(row["phase"] in {"baseline", "steady"} and row["admitted"] >= 4
                     and row["backlog"] <= 2 and row["oldest_queue_seconds"] <= 2 for row in self.samples)
        growing = any(b["phase"] == a["phase"] and b["phase"].startswith(("ramp_", "burst_"))
                      and b["backlog"] > a["backlog"] + 2 and b["oldest_queue_seconds"] > a["oldest_queue_seconds"]
                      for a, b in zip(self.samples, self.samples[1:]))
        required_metrics = ("db_client_connection_count", "db_client_connection_wait_time_seconds_count",
                            "db_client_operation_duration_seconds_count", "postgres_transaction_duration_seconds_count")
        observed_metrics = {name for row in self.samples for name in row["metrics"]}
        missing_metrics = [prefix for prefix in required_metrics if not any(name.startswith(prefix) for name in observed_metrics)]
        slice_ids = {row["logical_id"] for row in self.final_ids.get("applied", []) if self.expected[row["logical_id"]]["payload_bytes"] == 65536}
        gaps = []
        for observed, reason in ((stable, "stable_baseline_not_observed"), (growing, "growing_backlog_not_observed"),
                                 (len(slice_ids) >= 32, "64KiB_slice_not_observed"),
                                 (not missing_metrics, "required_worker_metrics_missing"),
                                 (self.final_ids.get("exact_catch_up"), "exact_catch_up_incomplete"),
                                 ("caught_up_unix" in self.outage, "outage_catch_up_not_observed"),
                                 (bool(summaries.get("baseline")), "ordinary_job_baseline_missing"),
                                 (self.shared_failure.get("ordinary_jobs_interrupted"), "shared_admission_failure_not_observed")):
            if not observed:
                gaps.append(reason)
        consumer = next((row["broker"]["consumer"]["config"] for row in self.samples if row["broker"].get("available")), {})
        if consumer.get("max_ack_pending") is None:
            gaps.append("effective_max_ack_pending_missing")
        result = {"status": "observed" if not gaps else "incomplete", "missing_observations": gaps,
                  "sample_count": len(self.samples), "ordinary_job_latency": summaries,
                  "outage": self.outage, "shared_admission_failure": self.shared_failure,
                  "missing_metrics": missing_metrics, "stable_load_observed": stable, "growing_backlog_observed": growing,
                  "producer_slots": 8, "producer_pool_max": 8, "worker_pool_max": 6, "worker_replicas": 1,
                  "publisher_concurrency": 1, "ordinary_job_slots": 1, "consumer_concurrency": 1,
                  "effective_max_ack_pending": consumer.get("max_ack_pending"), "consumer_config": consumer,
                  "client_tls": True, "route_tls": True, "stream_replicas": 3,
                  "host": self.session.data["resource_admission"], "artifacts": self.session.data["artifacts"],
                  "puback_meaning": "durably completed outbox IDs; ambiguous completion remains unconfirmed even if broker stored bytes",
                  "limits": ["single-host R3", "fixed container CPU and memory quotas", "JSON whitespace measures wire/storage size, not rich handler work",
                             "observer sampling and SQL/native command overhead are included", "no throughput guarantee or topology tuning"],
                  "dispositions": {
                      "publisher_concurrency_1": {"decision": "pending_final_delivery_review", "initial": "retain",
                          "reopen_if": "publisher saturated and queue age grows while measured database and broker have headroom"},
                      "broker_default_max_ack_pending": {"decision": "pending_final_delivery_review", "initial": "retain",
                          "reopen_if": "effective pending limit prevents aggregate replicas from using free handler slots"},
                      "shared_worker_roles": {"decision": "pending_final_delivery_review", "initial": "retain",
                          "reopen_if": "native messaging admission/critical failure interrupts ordinary jobs or exceeds baseline latency",
                          "native_admission_interruption_observed": bool(self.shared_failure.get("ordinary_jobs_interrupted"))}}}
        self.c.atomic(self.session.path / "measurement.json", result)
        report = [f"# R3/TLS shared-worker capacity: {result['status']}", "",
                  f"Samples: {len(self.samples)}. Offered: {len(self.expected)}. Admitted: {len(self.previous_ids['admitted'])}.",
                  "Actual interval rates and oldest queue age are in timeseries.csv; identity transitions and native telemetry are in evidence/.",
                  "IDs.json reconciles every scheduled event, producer outcome, durable PubAck completion, native retained source byte digest and effect.", "",
                  "JSON bodies are exactly 1 KiB/64 KiB with legal internal whitespace; native header bytes are additional. This is a transport/storage envelope.",
                  "One shared worker: publisher 1, consumer 1, ordinary jobs 1, shared pool 6. The separate ingress process has pool/concurrency 8.",
                  f"Broker effective MaxAckPending: {consumer.get('max_ack_pending')}. Single-host R3 with client and route TLS.",
                  f"Stable load observed: {stable}; growing backlog observed: {growing}; missing observations: {', '.join(gaps) or 'none'}.",
                  f"Outage/catch-up: {json.dumps(self.outage, sort_keys=True)}.",
                  f"Ordinary-job latency by enqueue phase: {json.dumps(summaries, sort_keys=True)}.", "",
                  "| Choice | Initial | Final delivery disposition |", "| --- | --- | --- |",
                  "| Publisher concurrency 1 | retain | pending evidence assessment |",
                  "| Broker-default MaxAckPending | retain | pending evidence assessment |",
                  "| Shared roles | retain | pending native admission interruption assessment |", "",
                  "Final delivery must resolve all three dispositions against the named triggers in measurement.json. A triggered change needs its reviewed Technical Design delta before runtime edits.",
                  "No capacity claim is complete when required observations are missing; generator saturation is not publisher saturation.", ""]
        (self.session.path / "report.md").write_text("\n".join(report))
        return result

    def execute(self):
        try:
            s = self.fresh("baseline")
            with s.lock():
                self.database("producer")
                s.worker_start("worker", "producer")
                self.plan = {"start_unix_ms": int((time.time() + 15) * 1000), "stages": [
                    {"name": name, "seconds": seconds, "rate": rate, "payload_bytes": size} for name, seconds, rate, size in STAGES]}
                self.expected = inventory(self.plan)
                self.c.atomic(self.path / "measurement-plan.json", self.plan)
                self.c.atomic(s.path / "measurement-plan.json", self.plan)
                total = sum(stage[1] for stage in STAGES)
                outage_start = self.plan["start_unix_ms"] / 1000 + sum(stage[1] for stage in STAGES[:9])
                s.workload_start()
                until = self.plan["start_unix_ms"] / 1000 + total + 30
                while time.time() < until:
                    if time.time() >= outage_start and not self.outage:
                        self.pause()
                    if "paused_unix" in self.outage and "resumed_unix" not in self.outage and time.time() - self.outage["paused_unix"] >= 10:
                        self.unpause()
                    row = self.sample()
                    observations = complete_lines(s.path / "load.jsonl")
                    if any(item.get("kind") == "finished" for item in observations) and row["phase"] == "settle":
                        break
                    time.sleep(1)
                self.unpause()
                self.c.require(any(item.get("kind") == "finished" for item in complete_lines(s.path / "load.jsonl")), "load_did_not_finish")
                s.worker_stop("load")
                self.shared_admission_failure()
                s.worker_stop("worker")
                self.reconcile()
                result = self.report()
                self.manifest.update({"status": result["status"], "measurement": "baseline/measurement.json",
                                      "report": "baseline/report.md", "duration_seconds": time.time() - self.manifest["started_at"]})
                self.save()
                return self.manifest
        except BaseException:
            self.manifest["status"] = "unverified_or_failed"
            self.save()
            raise
        finally:
            try:
                self.unpause()
            finally:
                self.finish_session()


def measure(controller, path, artifacts):
    return Measurement(controller, path, artifacts).execute()
