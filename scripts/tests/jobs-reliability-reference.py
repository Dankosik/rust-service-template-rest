#!/usr/bin/env python3
"""Run the fixed local jobs reference on the existing integration carrier.

The caller owns the Compose project and validation lock. This script owns only
its named databases, stream, children and derived checkout. It requires a
committed source candidate; compilation is setup, never recovery time. Receipts
remain on failure, and no failed bound is enlarged or treated as a skip.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

BASELINE = "ac88395be87cba3a1e0587f533dc50a71e358c8d"
# The derived rehearsal preserves its accepted Cargo/business graph. Source
# scenarios use the current candidate; this historical scoped patch stays fixed.
RUNTIME_ADOPTION_SOURCE = "cf0f1b7a816fd63e6fc019aa77b1a3eb45fcbc4b"
OPERATIONS = 128
FAULT_SECONDS = 5
RECOVERY_SECONDS = 180
SCENARIO_SECONDS = 300
SHUTDOWN_SECONDS = 45
RUNTIME_PATHS = (
    "crates/infra-jobs/src/attempt.rs",
    "crates/infra-jobs/src/claim.rs",
    "crates/infra-jobs/src/engine.rs",
    "crates/infra-jobs/src/maintenance.rs",
    "crates/jobs-worker/src/bootstrap.rs",
    "crates/jobs-worker/src/shutdown.rs",
)
MESSAGING_RUNTIME_PATH = "crates/infra-messaging/src/messaging.rs"
RETENTION_METADATA = ".sqlx/query-4ff5eea87475148656b3e4c0a62fb90fdc1e96997a8b96873e48285836003675.json"
BASELINE_RETENTION_METADATA = ".sqlx/query-0f5e344ad4e3a432b557d157a81c5a78328a75c31a2e6746c6af4781fe414b96.json"
UPSTREAM_COMPATIBILITY = "78abab7c9114f039644db4d6d3928f1541668df6"
REFERENCE_PATHS = (
    "test/src/reading_counter.rs",
    "test/src/reading_counter_receiver.rs",
    "test/src/bin/reading_counter_fixture.rs",
    "test/fixtures/migrations/reading_counter/0001_reading_counter.sql",
)
GAUGES = ("jobs_owned_attempts", "jobs_completion_memberships")
# Same initializer input boundary as scripts/ci/template-init-check.sh. These
# values belong to this derived recipe's flags/defaults, not the outer Make run.
INIT_ENV_KEYS = (
    "SERVICE_NAME", "REPOSITORY", "DESCRIPTION", "CODEOWNER", "DATABASE", "AUTHN",
    "OUTBOUND_HTTP", "OUTBOUND_AUTH", "GRPC", "HTTP_IDEMPOTENCY", "JOBS", "MESSAGING",
    "OUTBOX", "WEBHOOKS", "INBOUND_WEBHOOKS", "CACHE", "OBJECT_STORAGE", "AGENT_HARNESS",
)


def setup_environment(source: Path, environment: dict[str, str]) -> dict[str, str]:
    result = {key: value for key, value in environment.items() if key not in INIT_ENV_KEYS}
    # Initializer staging accepts an absolute caller cache. Resolve it once,
    # before changing cwd, and reuse it sequentially for every derived build.
    target = Path(result.get("CARGO_TARGET_DIR") or source / "target")
    result["CARGO_TARGET_DIR"] = str(target.absolute())
    return result


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def encode(value: object) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True)


def records(text: str) -> list[dict]:
    result = []
    for line in text.splitlines():
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if isinstance(value, dict):
            result.append(value)
    return result


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


class Child:
    def __init__(self, run: Run, name: str, argv: list[str], env: dict[str, str], cwd: Path):
        self.run = run
        self.name = name
        self.log = run.directory / f"{name}-{len(run.children)}.log"
        self.output = self.log.open("wb")
        self.process = subprocess.Popen(argv, cwd=cwd, env=env, stdout=self.output,
                                        stderr=subprocess.STDOUT, start_new_session=True)
        self.paused = False
        run.children.append(self)
        self.receipt = {"name": name, "pid": self.process.pid, "log": self.log.name,
                        "started_monotonic": time.monotonic()}
        run.receipt["children"].append(self.receipt)
        run.save()

    def read(self) -> list[dict]:
        return records(self.log.read_text(errors="replace"))

    def wait_record(self, predicate, deadline: float) -> dict:
        while time.monotonic() < deadline:
            for record in self.read():
                if predicate(record):
                    return record
            require(self.process.poll() is None, f"{self.name} exited before readiness; see {self.log}")
            time.sleep(0.05)
        raise RuntimeError(f"{self.name} handshake deadline expired; see {self.log}")

    def stop(self, *, kill: bool = False, deadline: float | None = None) -> None:
        if self.process.poll() is None:
            if self.paused:
                self.process.send_signal(signal.SIGCONT)
                self.paused = False
            self.process.send_signal(signal.SIGKILL if kill else signal.SIGTERM)
            remaining = SHUTDOWN_SECONDS if deadline is None else min(
                SHUTDOWN_SECONDS, max(0.01, deadline - time.monotonic()))
            try:
                code = self.process.wait(timeout=remaining)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
                self.receipt.update(forced_cleanup=True, exit_code=self.process.returncode,
                                    joined_monotonic=time.monotonic(), signal="SIGKILL")
                self.run.save()
                self.output.close()
                raise RuntimeError(f"{self.name} exceeded its shutdown boundary")
        else:
            code = self.process.returncode
        self.output.close()
        self.receipt.update(exit_code=code, joined_monotonic=time.monotonic(),
                            signal="SIGKILL" if kill else "SIGTERM")
        self.run.save()
        if kill:
            require(code == -signal.SIGKILL, f"{self.name}: actual SIGKILL exit was not observed")
        else:
            require(code == 0, f"{self.name} shutdown returned {code}; see {self.log}")


class Run:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.source = args.source.resolve()
        self.started = time.monotonic()
        self.setup_env = setup_environment(self.source, dict(os.environ))
        args.artifacts.mkdir(parents=True, exist_ok=True)
        self.directory = Path(tempfile.mkdtemp(prefix="jobs-reference-", dir=args.artifacts)).resolve()
        self.id = uuid.uuid4().hex[:16]
        self.project = os.environ.get("COMPOSE_PROJECT") or os.environ.get("COMPOSE_PROJECT_NAME")
        self.compose_file = (args.compose_file or self.source / "env/docker-compose.yml").resolve()
        self.database_url = os.environ.get("DATABASE_URL", "")
        self.nats_url = os.environ.get("NATS_URL", "")
        self.children: list[Child] = []
        self.databases: list[str] = []
        self.streams: list[dict] = []
        self.broker_stopped = False
        self.nats_container = ""
        self.derived: Path | None = None
        self.receipt = {
            "schema_version": 1, "status": "running", "run_id": self.id,
            "baseline_template": BASELINE, "candidate_template": args.candidate,
            "runtime_adoption_source": RUNTIME_ADOPTION_SOURCE,
            "runtime_adoption_scope": "historical-scoped-runtime-patch",
            "carrier": {"compose_project": self.project, "compose_file": str(self.compose_file),
                        "database_target": "loopback; credentials redacted", "nats_target": "loopback"},
            "bounds": {"operations": OPERATIONS, "initial_rows": 384, "payload_bytes": 1024,
                       "ordinary_slots": 3, "publisher_slots": 1, "worker_pool": 8,
                       "fault_seconds": FAULT_SECONDS, "recovery_seconds": RECOVERY_SECONDS,
                       "scenario_seconds": SCENARIO_SECONDS, "process_shutdown_seconds": SHUTDOWN_SECONDS},
            "children": [], "databases": self.databases, "streams": self.streams,
            "commands": [], "setup": [], "scenarios": [], "cleanup": [],
            "structural_bound": "T1 owner-local proof is separate; sparse samples do not prove the structural bound",
        }
        self.save()

    def progress(self, phase: str, label: str, event: str) -> None:
        # Only driver-owned labels and elapsed time enter CI stdout. Full
        # command, resource and operation evidence remains in the receipt.
        print(encode({"phase": phase, "label": label, "event": event,
                      "elapsed_seconds": round(time.monotonic() - self.started, 3)}), flush=True)

    def save(self) -> None:
        pending = self.directory / "receipt.tmp"
        pending.write_text(json.dumps(self.receipt, indent=2) + "\n")
        pending.replace(self.directory / "receipt.json")

    def command(self, argv: list[str], *, cwd: Path | None = None, env: dict | None = None,
                data: bytes | None = None, timeout: float = 30, okay: tuple[int, ...] = (0,),
                output_file: Path | None = None) -> subprocess.CompletedProcess:
        start = time.monotonic()
        # Credentials enter through environment; argv records contain fixture data only.
        safe = [re.sub(r"(?:postgres(?:ql)?|nats)://[^\s]+", "<redacted-dsn>", str(arg)) for arg in argv]
        entry = {"argv": safe, "cwd": str(cwd or self.source), "started_monotonic": start}
        self.receipt["commands"].append(entry)
        if data is not None:
            entry["stdin_sha256"] = hashlib.sha256(data).hexdigest()
        try:
            result = subprocess.run(argv, cwd=cwd or self.source, env=env, input=data,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
        except subprocess.TimeoutExpired:
            entry.update(outcome="timeout", elapsed_seconds=time.monotonic() - start)
            self.save()
            raise RuntimeError(f"command timed out: {safe[0]}") from None
        entry.update(exit_code=result.returncode, elapsed_seconds=time.monotonic() - start)
        if result.stderr:
            diagnostic = result.stderr.decode(errors="replace")
            for secret in (self.database_url, self.nats_url):
                diagnostic = diagnostic.replace(secret, "<redacted-dsn>")
            diagnostic = re.sub(r"(?:postgres(?:ql)?|nats)://[^\s]+", "<redacted-dsn>", diagnostic)
            log = self.directory / f"command-{len(self.receipt['commands'])}.stderr"
            log.write_text(diagnostic)
            entry["stderr"] = log.name
        if output_file is not None:
            output_file.write_bytes(result.stdout)
            entry["output"] = output_file.name
        self.save()
        require(result.returncode in okay, f"command failed ({result.returncode}): {' '.join(safe[:4])}")
        return result

    def compose(self, *args: str, **kwargs) -> subprocess.CompletedProcess:
        env = dict(os.environ, POSTGRES_PORT="0", PGBOUNCER_PORT="0", NATS_PORT="0")
        return self.command(["docker", "compose", "-p", self.project, "-f", str(self.compose_file), *args],
                            env=env, **kwargs)

    def sql(self, database: str, query: str, *, timeout: float = 8) -> str:
        result = self.compose("exec", "-T", "postgres", "psql", "-X", "-qAt", "-v", "ON_ERROR_STOP=1",
                              "-U", "app", "-d", database, data=query.encode(), timeout=timeout)
        return result.stdout.decode().strip()

    def query(self, database: str, query: str) -> object:
        return json.loads(self.sql(database, query))

    def create_database(self, label: str) -> str:
        name = f"reading_{self.id}_{label}"
        require(re.fullmatch(r"[a-z0-9_]+", name) is not None, "invalid disposable DB identity")
        # A lost CREATE acknowledgement must not leave an unowned database.
        self.databases.append(name)
        self.save()
        self.sql("app", f'CREATE DATABASE "{name}"')
        return name

    def dsn(self, database: str) -> str:
        parsed = urllib.parse.urlsplit(self.database_url)
        return urllib.parse.urlunsplit(parsed._replace(path="/" + database))

    def env(self, database: str, *, stream: str = "", endpoint: str = "", extra: dict | None = None) -> dict:
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(("APP__", "PG", "READING_"))}
        env.update(DATABASE_URL=self.dsn(database), NATS_URL=self.nats_url,
                   APP__POSTGRES__DSN=self.dsn(database), APP__POSTGRES__ENABLED="true",
                   APP__POSTGRES__MAX_CONNECTIONS="8", APP__JOBS__MAX_WORKERS="3",
                   APP__HTTP__ADDR="127.0.0.1:0", APP__OBSERVABILITY__METRICS__ADDR="127.0.0.1:0",
                   APP__LOG__FORMAT="json", APP__APP__ENV="local",
                   APP__MESSAGING__URLS=self.nats_url, APP__MESSAGING__SOURCE_STREAM=stream,
                   APP__MESSAGING__MAX_PAYLOAD_BYTES="1 KiB", APP__MESSAGING__ALLOW_PLAINTEXT="true",
                   APP__MESSAGING__ALLOW_UNAUTHENTICATED="true", READING_WEBHOOK_URL=endpoint)
        env.update(extra or {})
        return env

    def fixture(self, binary: Path, database: str, *args: str, cwd: Path | None = None,
                env: dict | None = None, timeout: float = 20, okay: tuple[int, ...] = (0,)) -> list[dict]:
        result = self.command([str(binary), *args], cwd=cwd, env=env or self.env(database),
                              timeout=timeout, okay=okay)
        decoded = records(result.stdout.decode())
        require(bool(decoded), f"fixture command {args[0]} emitted no JSON result")
        return decoded

    def build(self, source: Path, label: str) -> Path:
        started = time.monotonic()
        self.progress("setup", label, "build_started")
        log = self.directory / f"build-{label}.jsonl"
        result = self.command(["cargo", "build", "--release", "--locked", "-p", "integration-tests",
                               "--features", "integration", "--bin", "reading-counter-fixture",
                               "--message-format=json"], cwd=source, env=self.setup_env,
                              timeout=1800, output_file=log)
        paths = [r["executable"] for r in records(result.stdout.decode())
                 if r.get("reason") == "compiler-artifact" and r.get("executable")
                 and r.get("target", {}).get("name") == "reading-counter-fixture"]
        require(len(paths) == 1, "build did not identify exactly one reference executable")
        binary = self.directory / f"reading-counter-{label}"
        shutil.copy2(paths[0], binary)
        self.receipt["setup"].append({"label": label, "seconds": time.monotonic() - started,
                                      "binary": str(binary), "sha256": digest(binary),
                                      "source_revision": self.git(source, "rev-parse", "HEAD")})
        self.save()
        self.progress("setup", label, "build_completed")
        return binary

    def git(self, repo: Path, *args: str, okay: tuple[int, ...] = (0,)) -> str:
        return self.command(["git", *args], cwd=repo, okay=okay).stdout.decode().strip()

    def preflight(self) -> None:
        require(bool(self.project), "COMPOSE_PROJECT or COMPOSE_PROJECT_NAME must name the caller-owned carrier")
        require(bool(self.database_url and self.nats_url), "DATABASE_URL and NATS_URL are required")
        for value, schemes in ((self.database_url, ("postgres", "postgresql")),
                               (self.nats_url, ("nats",))):
            parsed = urllib.parse.urlsplit(value)
            require(parsed.scheme in schemes and parsed.hostname in ("127.0.0.1", "localhost", "::1"),
                    "reference requires the loopback disposable Compose carrier")
        require(re.fullmatch(r"[0-9a-f]{40}", self.args.candidate) is not None,
                "--candidate must be an immutable full commit SHA")
        require(self.args.candidate != BASELINE, "baseline and candidate must be distinct")
        require(self.git(self.source, "rev-parse", "HEAD") == self.args.candidate,
                "source checkout HEAD differs from candidate")
        require(not self.git(self.source, "status", "--porcelain", "--untracked-files=no"),
                "candidate source checkout has tracked changes")
        self.git(self.source, "cat-file", "-e", BASELINE + "^{commit}")
        self.git(self.source, "cat-file", "-e", RUNTIME_ADOPTION_SOURCE + "^{commit}")
        self.git(self.source, "merge-base", "--is-ancestor", UPSTREAM_COMPATIBILITY, RUNTIME_ADOPTION_SOURCE)
        self.git(self.source, "merge-base", "--is-ancestor", RUNTIME_ADOPTION_SOURCE, self.args.candidate)
        self.git(self.source, "merge-base", "--is-ancestor", UPSTREAM_COMPATIBILITY, self.args.candidate)
        postgres_port = self.compose("port", "postgres", "5432").stdout.decode().strip().rsplit(":", 1)[-1]
        require(urllib.parse.urlsplit(self.database_url).port == int(postgres_port),
                "DATABASE_URL differs from the selected direct PostgreSQL carrier")
        maximum = int(self.sql("app", "SHOW max_connections"))
        reserved = int(self.sql("app", "SHOW superuser_reserved_connections")) + int(
            self.sql("app", "SHOW reserved_connections"))
        require(maximum - reserved >= 16, "PostgreSQL cannot admit the fixed 16-session allocation")
        self.nats_container = self.compose("ps", "--quiet", "nats").stdout.decode().strip()
        require(bool(re.fullmatch(r"[0-9a-f]+", self.nats_container)), "one existing NATS container is required")
        # The carrier, not a developer's ambient broker, must own the fault target.
        labels = self.command(["docker", "inspect", "--format", "{{json .Config.Labels}}",
                               self.nats_container]).stdout.decode()
        require(json.loads(labels).get("com.docker.compose.project") == self.project,
                "NATS container does not belong to the selected carrier")
        port = self.compose("port", "nats", "4222").stdout.decode().strip().rsplit(":", 1)[-1]
        require(urllib.parse.urlsplit(self.nats_url).port == int(port), "NATS_URL differs from carrier broker")
        self.receipt["preflight"] = {"postgres_max_connections": maximum, "postgres_reserved": reserved,
                                     "session_allocation": 16, "nats_container": self.nats_container,
                                     "postgres_version": self.sql("app", "SHOW server_version"),
                                     "pg_dump": self.compose("exec", "-T", "postgres", "pg_dump", "--version").stdout.decode().strip()}
        self.receipt["reference_sources"] = {p: digest(self.source / p) for p in REFERENCE_PATHS}
        self.save()

    def cleanup(self) -> None:
        if self.broker_stopped:
            try:
                self.command(["docker", "start", self.nats_container], timeout=15)
                self.broker_stopped = False
                self.receipt["cleanup"].append({"resource": "broker", "outcome": "restarted"})
            except Exception as error:
                self.receipt["cleanup"].append({"resource": "broker", "error": str(error)})
        for child in reversed(self.children):
            if "joined_monotonic" not in child.receipt:
                try:
                    child.stop()
                    self.receipt["cleanup"].append({"resource": child.name, "outcome": "stopped_and_joined"})
                except Exception as error:
                    self.receipt["cleanup"].append({"resource": child.name, "error": str(error)})
        for stream in self.streams:
            try:
                self.fixture(Path(stream["binary"]), "app", "broker-cleanup", "--nats-url", self.nats_url,
                             "--stream", stream["stream"])
                self.receipt["cleanup"].append({"resource": stream["stream"], "outcome": "deleted"})
            except Exception as error:
                self.receipt["cleanup"].append({"resource": stream["stream"], "error": str(error)})
        for database in reversed(self.databases):
            try:
                self.sql("app", f'DROP DATABASE IF EXISTS "{database}"')
                self.receipt["cleanup"].append({"resource": database, "outcome": "deleted"})
            except Exception as error:
                self.receipt["cleanup"].append({"resource": database, "error": str(error)})
        if self.derived is not None and self.derived.exists():
            try:
                shutil.rmtree(self.derived)
                self.receipt["cleanup"].append({"resource": str(self.derived), "outcome": "deleted"})
            except OSError as error:
                self.receipt["cleanup"].append({"resource": str(self.derived), "error": str(error)})
        self.save()


class Scenario:
    def __init__(self, run: Run, binary: Path, label: str, *, cwd: Path | None = None,
                 producer: str | None = None, receiver: str | None = None, scope: str | None = None):
        self.run = run
        self.binary = binary
        self.cwd = cwd or run.source
        self.label = label
        self.producer = producer or run.create_database(label.replace("-", "_") + "_p")
        self.receiver_db = receiver or run.create_database(label.replace("-", "_") + "_r")
        self.scope = scope or str(uuid.uuid4())
        self.stream = "READING_" + uuid.uuid4().hex.upper()
        self.subject = "reading." + self.stream.lower() + ".accepted"
        self.consumer = "reading_reference"
        self.broker_initialized = False
        self.receiver: Child | None = None
        self.worker: Child | None = None
        self.metrics = ""
        self.endpoint = ""
        self.started = time.monotonic()
        self.deadline = self.started + SCENARIO_SECONDS
        self.receipt = {"label": label, "scope": self.scope, "producer_database": self.producer,
                        "receiver_database": self.receiver_db, "stream": self.stream,
                        "started_monotonic": self.started, "milestones": [], "samples": [],
                        "status": "running", "reconciliation": []}
        run.receipt["scenarios"].append(self.receipt)
        run.streams.append({"stream": self.stream, "consumer": self.consumer, "binary": str(binary)})
        run.save()
        run.progress("scenario", label, "started")
        if producer is None:
            self.cli("migrate", "--target", "producer")
        if receiver is None:
            self.cli("migrate", "--target", "receiver", database=self.receiver_db)

    def remaining(self) -> float:
        value = self.deadline - time.monotonic()
        require(value > 0, f"{self.label}: fixed 300-second runtime ceiling exceeded")
        return value

    def mark(self, event: str, **fields) -> None:
        self.receipt["milestones"].append({"event": event, "elapsed_seconds": time.monotonic() - self.started, **fields})
        self.run.save()
        if event in {
            "worker_ready", "fault_backlog_confirmed", "fault_released", "worker_killed_and_joined",
            "actual_backup_completed", "actual_restore_completed", "pre_restore_commands_discarded",
            "recovery_complete", "unknown_external_effect_held",
        }:
            self.run.progress("scenario", self.label, event)

    def cli(self, *args: str, database: str | None = None, timeout: float = 20,
            okay: tuple[int, ...] = (0,), extra: dict | None = None) -> list[dict]:
        database = database or self.producer
        return self.run.fixture(self.binary, database, *args, cwd=self.cwd,
                                env=self.run.env(database, stream=self.stream, endpoint=self.endpoint, extra=extra),
                                timeout=min(timeout, self.remaining()), okay=okay)

    def start_receiver(self, hold: tuple[str, dict, Path] | None = None) -> None:
        if not self.broker_initialized:
            self.cli("broker-init", "--nats-url", self.run.nats_url, "--stream", self.stream, "--subject", self.subject)
            self.broker_initialized = True
        argv = [str(self.binary), "receiver", "--nats-url", self.run.nats_url,
                "--subject", self.subject, "--stream", self.stream, "--consumer", self.consumer,
                "--http-bind", "127.0.0.1:0", "--run-seconds", str(min(250, max(1, int(self.remaining()) - 22)))]
        if hold:
            channel, operation, directory = hold
            directory.mkdir()
            argv.extend(["--hold-channel", channel, "--hold-operation", operation["operation_id"],
                         "--fault-ready", str(directory / "held"), "--fault-release", str(directory / "release")])
        self.receiver = Child(self.run, self.label + "-receiver", argv,
                              self.run.env(self.receiver_db), self.cwd)
        record = self.receiver.wait_record(lambda r: r.get("status") == "ready", min(self.deadline, time.monotonic() + 20))
        self.endpoint = "http://" + record["http_addr"] + "/reading"
        self.mark("receiver_ready", pid=self.receiver.process.pid)

    def start_worker(self, extra: dict | None = None) -> None:
        self.worker = Child(self.run, self.label + "-worker", [str(self.binary), "worker"],
                            self.run.env(self.producer, stream=self.stream, endpoint=self.endpoint, extra=extra), self.cwd)
        ready = min(self.deadline, time.monotonic() + 30)
        record = self.worker.wait_record(lambda r: r.get("message") == "diagnostics listener bound", ready)
        self.metrics = "http://" + record["addr"] + "/metrics"
        self.worker.wait_record(lambda r: r.get("message") == "jobs_worker_ready", ready)
        self.mark("worker_ready", pid=self.worker.process.pid)

    def stop_worker(self, *, kill: bool = False) -> None:
        if self.worker:
            self.worker.stop(kill=kill, deadline=self.deadline)
            self.mark("worker_killed_and_joined" if kill else "worker_stopped_and_joined", pid=self.worker.process.pid)
            self.worker = None

    def stop_receiver(self) -> None:
        if self.receiver:
            self.receiver.stop(deadline=self.deadline)
            self.mark("receiver_stopped_and_joined", pid=self.receiver.process.pid)
            self.receiver = None

    def operation(self, number: int) -> dict:
        operation = {"scope": self.scope, "operation_id": str(uuid.uuid5(uuid.UUID(self.scope), str(number))),
                     "article_id": str(uuid.uuid5(uuid.UUID(self.scope), "article-" + str(number % 8))),
                     "content_version": 1, "content": "reading-reference"}
        require(len(encode(operation).encode()) <= 1024, "fixture payload exceeded 1 KiB")
        return operation

    def accept(self, operations: list[dict], *, timeout: float = 40) -> list[dict]:
        path = self.run.directory / f"{self.label}-accept-{len(self.receipt['milestones'])}.json"
        path.write_text(encode(operations))
        results = self.cli("accept", "--operations-file", str(path), "--subject", self.subject, timeout=timeout)
        results = [r for r in results if r.get("status") in ("accepted", "unknown", "conflict")]
        require(len(results) == len(operations), "acceptance output did not cover each logical operation")
        require(all(r["status"] == "accepted" for r in results), "acceptance was not acknowledged; reconcile before replay")
        self.mark("acceptance_committed", operations=[op["operation_id"] for op in operations], receipts=results)
        return results

    def state(self) -> dict:
        # These bounded tables belong only to this scenario; queue readback is
        # deliberately separate from the authoritative effect projections.
        query = """SELECT json_build_object(
          'observed_at',statement_timestamp(),
          'available',(SELECT count(*) FROM background_jobs WHERE state='pending' AND not_before<=statement_timestamp()),
          'scheduled',(SELECT count(*) FROM background_jobs WHERE state='pending' AND not_before>statement_timestamp()),
          'running',(SELECT count(*) FROM background_jobs WHERE state='running'),
          'failed',(SELECT count(*) FROM background_jobs WHERE state='failed'),
          'completed',(SELECT count(*) FROM background_jobs WHERE state='completed'),
          'attempts',(SELECT coalesce(sum(attempts),0) FROM background_jobs),
          'oldest_age_seconds',(SELECT coalesce(max(extract(epoch FROM statement_timestamp()-created_at)),0)
             FROM background_jobs WHERE state IN ('pending','running')),
          'rows',(SELECT count(*) FROM background_jobs))"""
        return self.run.query(self.producer, query)

    def sample(self) -> None:
        sample = {"elapsed_seconds": time.monotonic() - self.started, "queue": self.state(),
                  "unknown_effect_readbacks": None}
        if self.worker and self.worker.process.poll() is None:
            result = self.run.command(["ps", "-p", str(self.worker.process.pid), "-o", "rss=,pcpu=,time="], timeout=3)
            sample["process_rss_kib_cpu_percent_cpu_time"] = result.stdout.decode().strip()
            if not self.worker.paused:
                try:
                    with urllib.request.urlopen(self.metrics, timeout=1) as response:
                        body = response.read(2 * 1024 * 1024).decode()
                    wanted = ("jobs_", "db_client_connection_", "postgres_transaction_")
                    sample["metrics"] = [line for line in body.splitlines() if line.startswith(wanted)]
                    for name in GAUGES:
                        value = metric_value(sample["metrics"], name)
                        if value is not None:
                            require(0 <= value <= 4, f"observed {name} outside admission bound")
                    pool = metric_value(sample["metrics"], "db_client_connection_max")
                    if pool is not None:
                        require(pool == 8, "worker measured pool maximum differs from eight")
                    counts = [float(line.split()[-1]) for line in sample["metrics"]
                              if line.startswith("db_client_connection_count{")]
                    if counts:
                        require(sum(counts) <= 8, "worker measured pool occupancy exceeded eight")
                except (OSError, ValueError) as error:
                    sample["metrics_unavailable"] = type(error).__name__
        self.receipt["samples"].append(sample)
        self.run.save()

    def effects(self, database: str, channel: str) -> list[dict]:
        return self.run.query(database, "SELECT coalesce(json_agg(t ORDER BY operation_id),'[]'::json) FROM "
            f"(SELECT operation_id,operation,read_count FROM reading_effects WHERE scope={sql_literal(self.scope)} "
            f"AND channel={sql_literal(channel)}) t")

    def confirm_effects(self, operations: list[dict], external: dict[str, list[dict]] | None = None) -> None:
        evidence = {}
        for channel, database in (("local", self.producer), ("outbox", self.receiver_db), ("webhook", self.receiver_db)):
            selected = operations if channel == "local" or external is None else external[channel]
            expected = {operation["operation_id"]: operation for operation in selected}
            effects = self.effects(database, channel)
            require({row["operation_id"] for row in effects} == set(expected), f"{channel}: effect identities differ")
            for row in effects:
                require(json.loads(row["operation"]) == expected[row["operation_id"]], f"{channel}: effect content differs")
            articles = self.run.query(database, "SELECT coalesce(json_agg(t),'[]'::json) FROM "
                f"(SELECT article_id,read_count FROM reading_articles WHERE scope={sql_literal(self.scope)} "
                f"AND channel={sql_literal(channel)}) t")
            counts = {}
            for operation in selected:
                counts[operation["article_id"]] = counts.get(operation["article_id"], 0) + 1
            require({row["article_id"]: row["read_count"] for row in articles} == counts,
                    f"{channel}: aggregate does not equal one effect per logical operation")
            evidence[channel] = {"effects": effects, "aggregates": articles}
        self.receipt["effect_readback"] = evidence
        self.mark("independent_effect_readback_confirmed")

    def drain(self, operations: list[dict], recovery_started: float, external: dict[str, list[dict]] | None = None) -> None:
        deadline = min(self.deadline, recovery_started + RECOVERY_SECONDS)
        next_progress = time.monotonic() + 15
        while time.monotonic() < deadline:
            self.sample()
            state = self.receipt["samples"][-1]["queue"]
            require(state["failed"] == 0, "positive scenario exhausted or permanently failed a job")
            require(self.worker is not None and self.worker.process.poll() is None, "worker exited during recovery")
            counts = [len(self.effects(db, channel)) for channel, db in
                      (("local", self.producer), ("outbox", self.receiver_db), ("webhook", self.receiver_db))]
            self.receipt["samples"][-1]["unknown_effect_readbacks"] = 0
            if time.monotonic() >= next_progress:
                self.run.progress("scenario", self.label, "recovery_wait")
                next_progress = time.monotonic() + 15
            expected_counts = [len(operations)] + [len(external[c]) if external else len(operations) for c in ("outbox", "webhook")]
            if state["available"] + state["scheduled"] + state["running"] == 0 and counts == expected_counts:
                self.confirm_effects(operations, external)
                self.mark("recovery_complete", recovery_seconds=time.monotonic() - recovery_started)
                return
            time.sleep(0.2)
        raise RuntimeError(f"{self.label}: recovery did not finish within the fixed 180 seconds")

    def assert_observation(self, *, ownership: bool) -> None:
        self.sample()
        metrics = self.receipt["samples"][-1].get("metrics", [])
        require(metric_value(metrics, "db_client_connection_max") == 8, "final pool observation unavailable")
        if ownership:
            # Ownership retires immediately, while the ordinary periodic pool
            # sampler may lag; this asks for the actual label-free gauges.
            require(all(metric_value(metrics, name) == 0 for name in GAUGES), "owned attempts/bookkeeping did not return idle")
        values = {name: [metric_value(sample.get("metrics", []), name) for sample in self.receipt["samples"]]
                  for name in GAUGES}
        self.receipt["ownership_observation"] = {
            "peaks": {name: max((value for value in vals if value is not None), default=None) for name, vals in values.items()},
            "idle": {name: metric_value(metrics, name) for name in GAUGES},
            "structural_proof": (
                "candidate gauges observed; structural bound belongs to separate T1 owner-local proof, not sparse peaks"
                if ownership else
                "old baseline has no ownership gauges; absent observations are null, not zero"
            )}
        self.run.save()

    def finish(self) -> None:
        self.stop_worker()
        self.stop_receiver()
        self.remaining()
        self.receipt.update(status="passed", elapsed_seconds=time.monotonic() - self.started)
        self.run.save()
        self.run.progress("scenario", self.label, "completed")


def metric_value(lines: list[str], name: str) -> float | None:
    values = [float(line.split()[-1]) for line in lines if line.startswith(name + " ") or line.startswith(name + "{")]
    return sum(values) if values else None


def wait_path(path: Path, deadline: float) -> None:
    while time.monotonic() < deadline:
        if path.is_file():
            return
        time.sleep(0.01)
    raise RuntimeError(f"fault milestone not observed: {path.name}")


def kill_at_checkpoint(scenario: Scenario, path: Path, *, receiver: bool = False) -> None:
    require(time.time() - path.stat().st_mtime < FAULT_SECONDS,
            "fault checkpoint already expired before the requested kill")
    scenario.stop_worker(kill=True)
    if receiver:
        scenario.receiver.stop(kill=True, deadline=scenario.deadline)
        scenario.receiver = None
    require(time.time() - path.stat().st_mtime < FAULT_SECONDS,
            "actual child kill/join missed the fixed checkpoint window")


def await_receiver_effect(scenario: Scenario, operation: dict, channel: str,
                          deadline: float, *, count: int = 1) -> None:
    require(scenario.receiver is not None, "receiver effect wait has no live owner")
    while time.monotonic() < min(deadline, scenario.deadline):
        found = [record for record in scenario.receiver.read()
                 if record.get("status") == "effect"
                 and record.get("effect", {}).get("channel") == channel
                 and record.get("effect", {}).get("operation", {}).get("operation_id") == operation["operation_id"]]
        if len(found) >= count:
            scenario.mark("receiver_delivery_observed", channel=channel,
                          operation_id=operation["operation_id"], deliveries=len(found))
            return
        require(scenario.receiver.process.poll() is None, "receiver exited before expected delivery")
        time.sleep(0.05)
    raise RuntimeError("actual receiver delivery was not observed before the recovery deadline")


def load_scenario(run: Run, binary: Path, fault: str) -> None:
    scenario = Scenario(run, binary, "source-" + fault)
    operations = [scenario.operation(i) for i in range(OPERATIONS)]
    scenario.start_receiver()
    control = run.directory / (scenario.label + "-pressure")
    extra = None
    if fault == "pool-pressure":
        control.mkdir()
        extra = {"READING_POOL_PRESSURE_DIR": str(control)}
    scenario.start_worker(extra)
    scenario.sample()
    if fault == "nats-unavailable":
        scenario.accept(operations[:1])
        scenario.mark("initial_admission_before_broker_fault")
    fault_started = time.monotonic()
    released = fault_started
    try:
        if fault == "worker-pause":
            scenario.worker.process.send_signal(signal.SIGSTOP)
            scenario.worker.paused = True
        elif fault == "nats-unavailable":
            # Retain restart custody even if the Docker client loses its answer
            # after the daemon has already stopped the recorded container.
            run.broker_stopped = True
            run.command(["docker", "stop", "--time", "1", run.nats_container], timeout=4)
        if fault == "pool-pressure":
            scenario.accept(operations[:1], timeout=3)
            wait_path(control / "held", fault_started + FAULT_SECONDS)
            scenario.mark("worker_shared_pool_held", evidence=(control / "held").read_text())
            scenario.accept(operations[1:], timeout=max(0.01, fault_started + FAULT_SECONDS - time.monotonic()))
        elif fault == "nats-unavailable":
            scenario.accept(operations[1:], timeout=max(0.01, fault_started + FAULT_SECONDS - time.monotonic()))
        else:
            scenario.accept(operations, timeout=40 if fault == "baseline" else max(0.01, fault_started + FAULT_SECONDS - time.monotonic()))
        scenario.sample()
        state = scenario.state()
        require(state["rows"] <= 384, "initial workload amplified beyond 384 rows")
        if fault != "baseline":
            require(state["available"] + state["scheduled"] + state["running"] > 0, "fault had no confirmed backlog")
            scenario.mark("fault_backlog_confirmed", fault=fault, queue=state)
    finally:
        recovery_started = time.monotonic()
        if fault == "worker-pause" and scenario.worker and scenario.worker.paused:
            scenario.worker.process.send_signal(signal.SIGCONT)
            scenario.worker.paused = False
        elif fault == "pool-pressure":
            (control / "release").touch()
        elif fault == "nats-unavailable" and run.broker_stopped:
            run.command(["docker", "start", run.nats_container], timeout=15)
            run.broker_stopped = False
        released = time.monotonic()
        scenario.mark("fault_released", fault=fault, duration_seconds=released - fault_started)
    if fault != "baseline":
        require(released - fault_started <= FAULT_SECONDS, "fault exceeded the fixed five-second ceiling")
    scenario.drain(operations, recovery_started)
    scenario.assert_observation(ownership=True)
    scenario.finish()


def queue_id(accepted: dict, channel: str) -> str:
    return accepted[channel + "_job_id"]


def inspect(scenario: Scenario, accepted: dict) -> dict:
    return {channel: scenario.cli("worker", "inspect", queue_id(accepted, channel))[0]
            for channel in ("local", "outbox", "webhook")}


def requests(scenario: Scenario) -> list[dict]:
    return scenario.run.query(scenario.producer,
        "SELECT coalesce(json_agg(t ORDER BY operation_id),'[]'::json) FROM "
        "(SELECT scope,operation_id,operation,event_id,event_time,local_job_id,outbox_job_id,webhook_job_id "
        f"FROM reading_requests WHERE scope={sql_literal(scenario.scope)}) t")


def read_channels(scenario: Scenario, operation: dict) -> dict:
    result = {}
    for channel, database in (("local", scenario.producer), ("outbox", scenario.receiver_db),
                              ("webhook", scenario.receiver_db)):
        result[channel] = scenario.cli("read", "--operation-json", encode(operation),
                                       "--channel", channel, database=database)[0]
    return result


def local_crash(run: Run, binary: Path, label: str, *, cwd: Path | None = None) -> None:
    scenario = Scenario(run, binary, label, cwd=cwd)
    operation = scenario.operation(0)
    scenario.start_receiver()
    accepted = scenario.accept([operation])[0]["accepted"]
    require(all(row["status"] == "absent" for row in read_channels(scenario, operation).values()),
            "committed-before-effect checkpoint was not empty")
    control = run.directory / (label + "-local")
    control.mkdir()
    scenario.start_worker({"READING_LOCAL_CHECKPOINT_DIR": str(control)})
    wait_path(control / "reached", min(scenario.deadline, time.monotonic() + 5))
    kill_at_checkpoint(scenario, control / "reached")
    scenario.mark("local_effect_uncommitted", control=(control / "reached").read_text(),
                  queue=inspect(scenario, accepted))
    require(scenario.cli("read", "--operation-json", encode(operation), "--channel", "local")[0]["status"] == "absent",
            "killed uncommitted marker/aggregate did not roll back")
    # Recovery starts before process startup and includes the real 90-second
    # reading lease. Neither claim_expires_at nor not_before is rewritten.
    recovery = time.monotonic()
    scenario.start_worker()
    scenario.drain([operation], recovery)
    scenario.assert_observation(ownership=True)
    scenario.finish()


def receiver_crash(run: Run, binary: Path, channel: str) -> None:
    scenario = Scenario(run, binary, "source-" + channel + "-crash")
    operation = scenario.operation(0)
    hold = run.directory / (scenario.label + "-hold")
    scenario.start_receiver((channel, operation, hold))
    accepted = scenario.accept([operation])[0]["accepted"]
    scenario.start_worker()
    wait_path(hold / "held", min(scenario.deadline, time.monotonic() + 20))
    kill_at_checkpoint(scenario, hold / "held", receiver=True)
    scenario.mark("receiver_committed_before_ack", evidence=json.loads((hold / "held").read_text()),
                  queue=inspect(scenario, accepted))
    scenario.mark("receiver_killed_before_ack")
    # Receiver truth is read from a separate database after both actual exits.
    observed = scenario.cli("read", "--operation-json", encode(operation),
                            "--channel", channel, database=scenario.receiver_db)[0]
    require(observed["status"] == "present", "independent committed receiver marker was lost")
    recovery = time.monotonic()
    scenario.start_receiver()
    scenario.start_worker()
    await_receiver_effect(scenario, operation, channel, recovery + RECOVERY_SECONDS)
    scenario.drain([operation], recovery)
    scenario.assert_observation(ownership=True)
    scenario.finish()


def snapshot(scenario: Scenario, path: Path) -> dict:
    require(scenario.worker is None and scenario.receiver is None, "snapshot requires all old writers joined")
    membership = requests(scenario)
    active = int(scenario.run.sql("app",
        "SELECT count(*) FROM pg_stat_activity WHERE datname=" + sql_literal(scenario.producer)))
    require(active == 0, "producer still has a session after writer shutdown")
    history = scenario.run.query(scenario.producer,
        "SELECT coalesce(json_agg(t ORDER BY version),'[]'::json) FROM "
        "(SELECT version,description,success,encode(checksum,'hex') AS checksum FROM _sqlx_migrations) t")
    sequence = scenario.run.sql(scenario.producer, "SELECT last_value FROM background_jobs_claim_generation")
    scenario.run.compose("exec", "-T", "postgres", "pg_dump", "-U", "app", "-d", scenario.producer,
                         "--format=custom", "--no-owner", "--no-acl", timeout=min(30, scenario.remaining()),
                         output_file=path)
    result = {"path": path.name, "sha256": digest(path), "request_membership": membership,
              "migration_history": history, "claim_generation_sequence": sequence,
              "excluded": ["receiver database", "NATS storage"], "producer_sessions_after_join": active}
    scenario.mark("actual_backup_completed", **result)
    return result


def restore(scenario: Scenario, path: Path, backup: dict) -> None:
    require(scenario.worker is None and scenario.receiver is None, "restore requires joined old writers/consumers")
    old_database = scenario.producer
    require(int(scenario.run.sql("app", "SELECT count(*) FROM pg_stat_activity WHERE datname="
                               + sql_literal(old_database))) == 0, "old producer sessions survived join")
    restored = scenario.run.create_database(scenario.label.replace("-", "_") + "_restored")
    scenario.run.compose("exec", "-T", "postgres", "pg_restore", "-U", "app", "-d", restored,
                         "--no-owner", "--no-acl", "--exit-on-error", data=path.read_bytes(),
                         timeout=min(30, scenario.remaining()))
    scenario.producer = restored
    require(requests(scenario) == backup["request_membership"], "restored snapshot request membership differs")
    history = scenario.run.query(restored,
        "SELECT coalesce(json_agg(t ORDER BY version),'[]'::json) FROM "
        "(SELECT version,description,success,encode(checksum,'hex') AS checksum FROM _sqlx_migrations) t")
    require(history == backup["migration_history"], "restored migration history differs")
    require(scenario.run.sql(restored, "SELECT last_value FROM background_jobs_claim_generation")
            == backup["claim_generation_sequence"], "restored claim-generation sequence differs")
    kinds = scenario.run.query(restored, "SELECT coalesce(json_agg(kind),'[]'::json) FROM "
                             "(SELECT DISTINCT kind FROM background_jobs ORDER BY kind) t")
    scenario.receipt["restored_producer_database"] = restored
    scenario.mark("actual_restore_completed", old_database=old_database, new_database=restored,
                  migration_history=history, registered_queue_kinds=kinds)


def restore_scenario(run: Run, binary: Path, label: str, *, cwd: Path | None = None,
                     producer: str | None = None, receiver: str | None = None,
                     scope: str | None = None, previous: list[dict] | None = None) -> Scenario:
    scenario = Scenario(run, binary, label, cwd=cwd, producer=producer, receiver=receiver, scope=scope)
    previous = previous or []
    known = scenario.operation(1000)
    pending = scenario.operation(1001)
    later = scenario.operation(1002)
    scenario.start_receiver()
    scenario.accept([known])
    scenario.start_worker()
    scenario.drain(previous + [known], time.monotonic())
    scenario.stop_worker()
    scenario.stop_receiver()
    before = scenario.accept([pending])[0]["accepted"]
    require(all(row["status"] == "absent" for row in read_channels(scenario, pending).values()),
            "snapshot's acknowledged pre-effect operation is not independently absent")
    path = run.directory / (label + ".dump")
    backup = snapshot(scenario, path)
    later_accepted = scenario.accept([later])[0]["accepted"]
    hold = run.directory / (label + "-receiver-hold")
    scenario.start_receiver(("webhook", later, hold))
    scenario.start_worker()
    wait_path(hold / "held", min(scenario.deadline, time.monotonic() + 20))
    # Observe the other independent effect while the matching HTTP response
    # remains held. This removes ambiguity about a surviving orphan broker
    # message when the later acceptance falls outside the snapshot.
    await_receiver_effect(scenario, later, "outbox", time.monotonic() + 3)
    # Capture real lost HTTP completion, then join both old owners. A receiver
    # SIGKILL removes an in-flight ACK/response, not its committed database.
    kill_at_checkpoint(scenario, hold / "held", receiver=True)
    pre_restore = inspect(scenario, before)
    pre_restore_later = inspect(scenario, later_accepted)
    later_truth = read_channels(scenario, later)
    require(later_truth["webhook"]["status"] == "present", "post-snapshot independent effect was not committed")
    scenario.mark("pre_restore_commands_discarded", prior_inspection=pre_restore,
                  later_inspection=pre_restore_later,
                  rule="all saved command tokens and receipts discarded even if numeric versions recur")
    # The saved objects are evidence only. Recovery below creates fresh
    # inspection documents against the newly restored target.
    del pre_restore, pre_restore_later
    restore(scenario, path, backup)
    lost = scenario.cli("read", "--operation-json", encode(later), "--channel", "request")[0]
    require(lost["status"] == "absent", "later acceptance was unexpectedly within snapshot RPO")
    scenario.receipt["snapshot_rpo"] = {
        "later_operation": later, "restored_acceptance": lost, "surviving_external_readback": later_truth,
        "action": "outside_backup; do_not_reconstruct_acceptance_from_expected_results",
    }
    expected_external = {}
    restored_operations = previous + [known, pending]
    for operation in restored_operations + [later]:
        effects = read_channels(scenario, operation)
        acceptance = scenario.cli("read", "--operation-json", encode(operation), "--channel", "request")[0]
        accepted_identity = later_accepted if operation == later else acceptance["accepted"]
        fresh = inspect(scenario, accepted_identity)
        action = "outside_backup_hold" if operation == later else "same_identity_replay_permitted"
        require(all(row["status"] in ("present", "absent") for row in effects.values()),
                "positive reconciliation has unavailable authoritative readback")
        scenario.receipt["reconciliation"].append({
            "operation": operation, "snapshot_member": operation != later, "acceptance": acceptance,
            "queue": fresh, "effect_readback": effects, "permitted_action": action,
            "receipt": "known; operator redrive not needed for restored pending rows",
        })
        for channel in ("outbox", "webhook"):
            if effects[channel]["status"] == "present":
                expected_external.setdefault(channel, []).append(operation)
    # Preserve the independently observed channel membership; the queue's
    # completed state does not establish an external effect.
    external = {channel: restored_operations + [later]
                if later in expected_external.get(channel, []) else restored_operations
                for channel in ("outbox", "webhook")}
    recovery = time.monotonic()
    scenario.start_receiver()
    scenario.start_worker()
    for channel in ("outbox", "webhook"):
        await_receiver_effect(scenario, pending, channel, recovery + RECOVERY_SECONDS)
    scenario.drain(restored_operations, recovery, external)
    scenario.assert_observation(ownership=True)
    scenario.finish()
    return scenario


def unknown_hold(run: Run, binary: Path) -> None:
    scenario = Scenario(run, binary, "source-unknown-hold")
    operation = scenario.operation(0)
    hold = run.directory / (scenario.label + "-hold")
    scenario.start_receiver(("webhook", operation, hold))
    accepted = scenario.accept([operation])[0]["accepted"]
    scenario.start_worker()
    wait_path(hold / "held", min(scenario.deadline, time.monotonic() + 20))
    kill_at_checkpoint(scenario, hold / "held", receiver=True)
    before = inspect(scenario, accepted)
    # A bound but non-listening local socket makes this one readback unavailable;
    # the authoritative receiver store is neither restored nor deleted.
    with socket.socket() as unavailable:
        unavailable.bind(("127.0.0.1", 0))
        port = unavailable.getsockname()[1]
        parsed = urllib.parse.urlsplit(run.dsn(scenario.receiver_db))
        unavailable_dsn = urllib.parse.urlunsplit(parsed._replace(
            netloc=f"{parsed.username}:{parsed.password}@127.0.0.1:{port}"))
        observed = scenario.cli("read", "--operation-json", encode(operation), "--channel", "webhook",
                                database=scenario.receiver_db, extra={"DATABASE_URL": unavailable_dsn},
                                okay=(2,), timeout=10)[0]
    require(observed["status"] == "unknown", "unavailable receiver readback was treated as absence")
    after = inspect(scenario, accepted)
    require({key: value.get("item") for key, value in before.items()}
            == {key: value.get("item") for key, value in after.items()},
            "unknown hold mutated queue state")
    scenario.receipt["reconciliation"].append({
        "operation": operation, "snapshot_member": False, "acceptance": "acknowledged",
        "queue": after, "effect_readback": {"webhook": observed},
        "permitted_action": "pending_manual_reconciliation", "new_delivery_count": 0,
        "redrive_count": 0, "worker_restart_count": 0,
    })
    scenario.mark("unknown_external_effect_held", receiver_store="retained unchanged through decision",
                  producer="isolated and stopped; no replay")
    scenario.finish()
    scenario.receipt["status"] = "expected_manual_reconciliation_hold"
    run.save()


def transport_replay(run: Run, binary: Path) -> None:
    scenario = Scenario(run, binary, "source-transport-replay")
    operation = scenario.operation(0)
    scenario.start_receiver()
    original = scenario.accept([operation])[0]["accepted"]
    scenario.start_worker()
    scenario.drain([operation], time.monotonic())
    scenario.stop_worker()
    # Only retention aging is accelerated. Actual engine retention removes rows;
    # this SQL never simulates a crash, restore, lease expiry or failed result.
    run.sql(scenario.producer, "UPDATE background_jobs SET finished_at=statement_timestamp()-interval '25 hours' "
            "WHERE state='completed'")
    scenario.mark("transport_retention_age_accelerated", simulated_age_hours=25,
                  wall_clock_elapsed_claim=False)
    scenario.start_worker()
    deadline = min(scenario.deadline, time.monotonic() + 15)
    while scenario.state()["rows"]:
        require(time.monotonic() < deadline, "actual engine retention did not remove completed transports")
        time.sleep(0.1)
    scenario.stop_worker()
    scenario.mark("actual_transport_cleanup_observed", original=inspect(scenario, original))
    # Stream duplicate-window is one second; wait actual wall-clock time.
    time.sleep(1.1)
    replay = scenario.cli("replay", "--operation-json", encode(operation))[0]
    require(replay["status"] == "accepted", "transport replay was not acknowledged")
    fresh = replay["accepted"]
    require(all(queue_id(original, c) != queue_id(fresh, c) for c in ("local", "outbox", "webhook")),
            "replay did not create distinct transport identities")
    scenario.start_worker({"READING_FAIL_OPERATION": operation["operation_id"]})
    deadline = min(scenario.deadline, time.monotonic() + 20)
    failed = None
    while time.monotonic() < deadline:
        observed = scenario.cli("worker", "inspect", queue_id(fresh, "local"))[0]
        if observed.get("item", {}).get("state") == "failed":
            failed = observed["item"]
            break
        time.sleep(0.1)
    require(failed is not None, "fixture permanent failure was not durably observed")
    scenario.stop_worker()
    effects = read_channels(scenario, operation)
    require(all(r["status"] == "present" for r in effects.values()), "redrive lacked independent known effect")
    # Reinspect after stopping the incompatible producer. This exact fresh token
    # alone supplies redrive; no pre-restore/version receipt is reused.
    fresh_inspection = scenario.cli("worker", "inspect", queue_id(fresh, "local"))[0]["item"]
    recovery = scenario.cli("worker", "redrive", fresh_inspection["id"], "--kind", fresh_inspection["kind"],
                            "--version", fresh_inspection["version"])[0]
    require(recovery.get("outcome") == "redriven", "permitted operator redrive was not acknowledged")
    scenario.receipt["reconciliation"].append({
        "operation": operation, "snapshot_member": False, "acceptance": original,
        "queue": fresh_inspection, "effect_readback": effects,
        "permitted_action": "same_identity_redrive", "receipt": recovery,
        "new_transport_receipt": replay, "broker_dedup_wall_clock_wait_seconds": 1.1,
    })
    resumed = time.monotonic()
    scenario.start_worker()
    scenario.drain([operation], resumed)
    for channel in ("outbox", "webhook"):
        await_receiver_effect(scenario, operation, channel, resumed + RECOVERY_SECONDS, count=2)
    scenario.assert_observation(ownership=True)
    scenario.finish()


def block(text: str, marker: str) -> str:
    pattern = re.compile(rf"(?m)^([/#]+) template:begin {re.escape(marker)}\n.*?^\1 template:end {re.escape(marker)}\n",
                         re.DOTALL)
    found = pattern.findall(text)
    require(len(found) == 1, "expected exactly one scoped reference declaration: " + marker)
    return pattern.search(text).group(0)


def commit(run: Run, repo: Path, message: str) -> str:
    run.git(repo, "add", "-A")
    run.git(repo, "-c", "user.name=jobs-reference", "-c", "user.email=jobs-reference@example.invalid",
            "-c", "commit.gpgsign=false", "commit", "-qm", message)
    return run.git(repo, "rev-parse", "HEAD")


def business_image(scenario: Scenario) -> dict:
    result = {"requests": requests(scenario)}
    for database, label in ((scenario.producer, "producer"), (scenario.receiver_db, "receiver")):
        for table, order in (("reading_effects", "channel,operation_id"), ("reading_articles", "channel,article_id")):
            result[label + "_" + table] = scenario.run.query(database,
                f"SELECT coalesce(json_agg(t ORDER BY {order}),'[]'::json) FROM "
                f"(SELECT * FROM {table} WHERE scope={sql_literal(scenario.scope)}) t")
    return result


def upgrade(run: Run) -> None:
    start = time.monotonic()
    run.progress("setup", "derived-upgrade", "started")
    # Portable sync requires disjoint Git roots. Keep the owned checkout beside
    # the source, while receipts and binaries remain in the artifact directory.
    derived = Path(tempfile.mkdtemp(prefix="jobs-reference-derived-", dir=run.source.parent)).resolve()
    run.derived = derived
    run.receipt["derived_checkout"] = str(derived)
    run.save()
    run.command(["git", "clone", "--quiet", "--no-hardlinks", str(run.source), str(derived)], timeout=120)
    run.git(derived, "checkout", "--quiet", "--detach", BASELINE)
    # Initialization resolves the staged lockfile offline, including packages
    # for other targets that the source build does not download.
    run.command(["cargo", "fetch", "--locked"], cwd=derived, env=run.setup_env, timeout=180)
    run.progress("setup", "derived-baseline", "initialization_started")
    run.command(["bash", str(derived / "scripts/init-module.sh"), "--repo", str(derived),
                 "--service-name", "reading-reference", "--repository", "https://github.com/example/reading-reference",
                 "--description", "Local reading counter recovery reference", "--codeowner", "@example/platform",
                 "--database", "postgres", "--jobs", "postgres", "--messaging", "nats-jetstream",
                 "--outbox", "postgres", "--webhooks", "durable", "--outbound-http", "bounded",
                 "--agent-harness", "codex"], cwd=derived, env=run.setup_env, timeout=120)
    run.progress("setup", "derived-baseline", "initialization_completed")
    for path in REFERENCE_PATHS:
        destination = derived / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(run.source / path, destination)
    source_manifest = (run.source / "test/Cargo.toml").read_text()
    target_manifest = derived / "test/Cargo.toml"
    manifest = target_manifest.read_text()
    # Install only the standalone application declarations; template sync does
    # not own Cargo or business Rust. Dependencies already exist in the lock.
    for marker in ("jobs-reference:test-reading-counter-dependencies", "jobs-reference:test-reading-counter-fixture"):
        manifest += "\n" + block(source_manifest, marker)
    tokio = re.search(r"(?m)^tokio = .+$", source_manifest).group(0)
    manifest, replacements = re.subn(r"(?m)^tokio = .+$", lambda _: tokio, manifest, count=1)
    require(replacements == 1, "baseline fixture did not have its expected Tokio declaration")
    target_manifest.write_text(manifest)
    lib = derived / "test/src/lib.rs"
    lib.write_text(lib.read_text() + "\n" + block((run.source / "test/src/lib.rs").read_text(),
                                                  "jobs-reference:test-lib-reading-counter"))
    architecture = derived / "docs/repo-architecture.md"
    architecture.write_text(architecture.read_text() + "\nLocal reading-counter feature owns its operation identity, "
                           "non-expiring effect markers and independent receiver projections.\n")
    skill = derived / ".agents/skills/reading-reference"
    skill.mkdir(parents=True)
    (skill / ".service-owned").write_text("")
    (skill / "SKILL.md").write_text("---\nname: reading-reference\ndescription: Operate this service's local reading fixture.\n---\n\n"
                                   "Read the service-owned reading_counter fixture before changing its identities.\n")
    owned_paths = [*REFERENCE_PATHS, "test/Cargo.toml", "test/src/lib.rs", "docs/repo-architecture.md",
                   ".agents/skills/reading-reference/.service-owned", ".agents/skills/reading-reference/SKILL.md",
                   "Cargo.lock", BASELINE_RETENTION_METADATA]
    preserved = {path: digest(derived / path) for path in owned_paths}
    before_commit = commit(run, derived, "initialize baseline service with owned reading feature")
    baseline_binary = run.build(derived, "derived-baseline")
    run.receipt["setup"].append({"label": "initialized_baseline_setup", "seconds": time.monotonic() - start})
    before = Scenario(run, baseline_binary, "derived-before", cwd=derived)
    operations = [before.operation(i) for i in range(8)]
    before.start_receiver()
    accepted = before.accept(operations)
    before.start_worker()
    before.drain(operations, time.monotonic())
    before.assert_observation(ownership=False)
    before.finish()
    data_before = business_image(before)

    # Normal current portable sync, then the separately identified historical
    # runtime patch. Any conflict or extra runtime path remains a refusal.
    run.progress("setup", "derived-candidate", "portable_sync_started")
    check = run.command(["bash", str(run.source / "scripts/template-sync.sh"), "--check", "--from",
                         str(run.source), "--repo", str(derived)], cwd=run.source, okay=(0, 1), timeout=120,
                        env=run.setup_env, output_file=run.directory / "portable-sync.diff")
    run.command(["bash", str(run.source / "scripts/template-sync.sh"), "--apply", "--from", str(run.source),
                 "--repo", str(derived)], cwd=run.source, env=run.setup_env, timeout=120,
                output_file=run.directory / "portable-sync-apply.txt")
    run.progress("setup", "derived-candidate", "portable_sync_completed")
    require({path: digest(derived / path) for path in owned_paths} == preserved,
            "portable sync changed service-owned source/schema/customization or locked graph")
    changed = run.git(run.source, "diff", "--name-only", BASELINE, RUNTIME_ADOPTION_SOURCE, "--",
                      "crates/infra-jobs", "crates/jobs-worker").splitlines()
    require(bool(changed) and set(changed) <= set(RUNTIME_PATHS), "runtime adoption exceeds accepted source allowlist")
    messaging_changed = run.git(run.source, "diff", "--name-only", BASELINE, RUNTIME_ADOPTION_SOURCE, "--",
                                "crates/infra-messaging/src").splitlines()
    require(messaging_changed == [MESSAGING_RUNTIME_PATH],
            "messaging adoption must contain only the reviewed terminal-callback owner")
    changed += messaging_changed
    # The known main78 retention change uses this checked query. Add its exact
    # metadata, retaining the old query metadata for untouched baseline callers.
    patch_paths = [*changed, RETENTION_METADATA]
    patch = run.directory / "runtime-adoption.patch"
    run.command(["git", "diff", "--binary", BASELINE, RUNTIME_ADOPTION_SOURCE, "--", *patch_paths],
                cwd=run.source, output_file=patch)
    run.command(["git", "apply", "--check", str(patch)], cwd=derived)
    run.command(["git", "apply", str(patch)], cwd=derived)
    adopted_hashes = {path: digest(derived / path) for path in patch_paths}
    expected_hashes = {
        path: hashlib.sha256(run.command(
            ["git", "show", f"{RUNTIME_ADOPTION_SOURCE}:{path}"], cwd=run.source,
        ).stdout).hexdigest()
        for path in patch_paths
    }
    require(adopted_hashes == expected_hashes,
            "adopted runtime/metadata bytes differ from the pinned scoped source")
    runtime_hashes = {path: adopted_hashes[path] for path in changed}
    require({path: digest(derived / path) for path in owned_paths} == preserved,
            "runtime adoption changed service-owned feature or customization")
    after_commit = commit(run, derived, "adopt portable candidate and pinned historical jobs runtime patch")
    run.progress("setup", "derived-candidate", "runtime_adoption_completed")
    require(before_commit != after_commit, "derived upgrade did not produce distinct commits")
    after_binary = run.build(derived, "derived-candidate")
    require(business_image(before) == data_before, "stopped derived data changed during source adoption/build")
    adoption = {
        "baseline_template": BASELINE, "candidate_template": run.args.candidate,
        "runtime_adoption_source": RUNTIME_ADOPTION_SOURCE,
        "runtime_adoption_scope": "historical-scoped-runtime-patch",
        "derived_before": before_commit, "derived_after": after_commit,
        "portable_check_exit": check.returncode, "patch": patch.name, "patch_sha256": digest(patch),
        "runtime_paths": changed, "runtime_source_hashes": runtime_hashes,
        "upstream_compatibility_commit": UPSTREAM_COMPATIBILITY,
        "additive_query_metadata": {RETENTION_METADATA: adopted_hashes[RETENTION_METADATA]},
        "retained_baseline_query_metadata": BASELINE_RETENTION_METADATA,
        "preserved_owned_hashes": preserved, "preserved_data": data_before,
        "before_binary_sha256": digest(baseline_binary), "after_binary_sha256": digest(after_binary),
    }
    run.receipt["upgrade"] = adoption
    run.save()

    after = Scenario(run, after_binary, "derived-after", cwd=derived, producer=before.producer,
                     receiver=before.receiver_db, scope=before.scope)
    after.start_receiver()
    duplicate = after.accept(operations)
    require([entry["accepted"] for entry in duplicate] == [entry["accepted"] for entry in accepted],
            "equal request changed accepted identities after upgrade")
    conflict = dict(operations[0], content="conflicting content")
    conflict_receipt = after.cli("accept", "--operation-json", encode(conflict), "--subject", after.subject,
                                okay=(2,))[0]
    require(conflict_receipt["status"] == "conflict", "upgraded feature accepted conflicting immutable fields")
    extra = [after.operation(i) for i in range(8, 16)]
    after.accept(extra)
    after.start_worker()
    after.drain(operations + extra, time.monotonic())
    after.assert_observation(ownership=True)
    after.finish()
    adoption["feature_after"] = business_image(after)
    adoption["duplicate_readback"] = duplicate
    adoption["conflict_receipt"] = conflict_receipt
    require({path: digest(derived / path) for path in owned_paths} == preserved,
            "feature execution altered source/schema/customization")
    # Run R2 using the same upgraded feature and preserved data. Restore keeps
    # the independent receiver store and demonstrates the post-snapshot RPO gap.
    recovery = restore_scenario(run, after_binary, "derived-restore", cwd=derived,
                               producer=after.producer, receiver=after.receiver_db, scope=after.scope,
                               previous=operations + extra)
    adoption["restored_feature_data"] = business_image(recovery)
    adoption["preservation_after_execution"] = {path: digest(derived / path) for path in owned_paths}
    run.save()
    run.progress("setup", "derived-upgrade", "completed")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", required=True, help="exact committed candidate SHA, distinct from fixed baseline")
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--compose-file", type=Path)
    parser.add_argument("--artifacts", type=Path)
    args = parser.parse_args()
    if args.artifacts is None:
        args.artifacts = args.source / "target/jobs-reliability-reference"
    run = Run(args)
    def interrupted(signum, _frame):
        raise RuntimeError(f"reference interrupted by signal {signum}")
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, interrupted)
    try:
        run.preflight()
        binary = run.build(run.source, "source-candidate")
        # Surface upgrade setup failures before the long lease-recovery runs;
        # every source scenario still runs once.
        upgrade(run)
        for fault in ("baseline", "worker-pause", "pool-pressure", "nats-unavailable"):
            load_scenario(run, binary, fault)
        local_crash(run, binary, "source-local-crash")
        receiver_crash(run, binary, "outbox")
        restore_scenario(run, binary, "source-restore")
        unknown_hold(run, binary)
        transport_replay(run, binary)
        run.receipt["status"] = "passed"
    except Exception as error:
        run.receipt["status"] = "failed"
        run.receipt["failure"] = str(error)
        run.progress("run", "reference", "failed")
        for scenario in run.receipt["scenarios"]:
            if scenario["status"] == "running":
                scenario["status"] = "failed"
                scenario["failure"] = str(error)
    finally:
        for signum in (signal.SIGINT, signal.SIGTERM):
            signal.signal(signum, signal.SIG_IGN)
        run.save()
        run.progress("run", "reference", "cleanup_started")
        run.cleanup()
        run.progress("run", "reference", "cleanup_completed")
        if any("error" in entry for entry in run.receipt["cleanup"]):
            run.receipt["status"] = "failed"
            run.receipt["cleanup_gap"] = "one or more owned resources did not acknowledge cleanup"
        run.save()
        (args.artifacts / "receipt.json").write_text(json.dumps(run.receipt, indent=2) + "\n")
    print(encode({"status": run.receipt["status"], "receipt": str(run.directory / "receipt.json")}))
    return 0 if run.receipt["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
