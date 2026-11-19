#!/usr/bin/env python3
import argparse
from collections import Counter
from contextlib import contextmanager, suppress
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time


@dataclass(frozen=True)
class Cell:
    scenario: str
    shape: str = "default"
    concurrency: int = 1
    telemetry: str = "absent"
    group: str = "final"
    bind: str = "127.0.0.1"


CELLS = {
    f"{scenario}-{concurrency}": Cell(scenario, concurrency=concurrency)
    for scenario in ("service-before", "service-refresh")
    for concurrency in (1, 64)
}
CELLS.update({
    "service-demand": Cell("service-demand", concurrency=64),
    "exchange": Cell("exchange"),
})
for scenario, final, diagnostics in (
    ("http-buffered", ("empty", "empty-post", "empty-put", "empty-patch", "one", "multi"), (
        "large", "tiny", "canonical", "alternate", "mixed", "caller-host", "late-error",
        "bad-scheme", "bad-port", "bad-origin", "userinfo", "retry", "unstarted-retry",
    )),
    ("http-headers", ("empty", "one", "late-error"), ()),
    ("http-retain", (), ("one", "tiny", "large")),
):
    for shape in (*final, *diagnostics):
        group = "final" if shape in final else "diagnostic"
        CELLS[f"{scenario}-{shape}"] = Cell(scenario, shape, group=group)

CELLS["http-buffered-ipv6"] = Cell("http-buffered", "one", group="diagnostic", bind="::1")

CELLS.update({
    f"{scenario}-{shape}-{telemetry}": Cell(scenario, shape, telemetry=telemetry)
    for scenario in ("grpc-transport", "grpc-decoded")
    for shape in ("immediate", "unary", "stream", "fallback-immediate", "fallback-unary", "fallback-stream")
    for telemetry in ("absent", "filtered", "enabled")
})


def utc_now():
    return datetime.now(timezone.utc).isoformat()


def write_json(path, value):
    draft = path.with_suffix(path.suffix + ".tmp")
    with draft.open("w", encoding="utf-8") as output:
        json.dump(value, output, indent=2, sort_keys=True, allow_nan=False)
        output.write("\n")
    draft.replace(path)


def read_json(path):
    with path.open(encoding="utf-8") as source:
        return json.load(source)


def sha256(path):
    with path.open("rb") as source:
        digest = hashlib.sha256()
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
        return digest.hexdigest()


def positive_int(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("expected a positive integer")
    return number


def cpu_set(value):
    if not re.fullmatch(r"[0-9]+(?:-[0-9]+)?(?:,[0-9]+(?:-[0-9]+)?)*", value):
        raise argparse.ArgumentTypeError("expected Linux CPU-list syntax, for example 0,2-3")
    cpus = set()
    for part in value.split(","):
        first, _, last = part.partition("-")
        start = int(first)
        end = int(last or first)
        if end < start:
            raise argparse.ArgumentTypeError("CPU ranges must be ascending")
        cpus.update(range(start, end + 1))
    return sorted(cpus)


def labelled_path(value):
    label, separator, path = value.partition("=")
    if not separator or not path or not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_-]*", label):
        raise argparse.ArgumentTypeError("expected LABEL=PATH")
    return label, Path(path).resolve()


def deadline_utc(value):
    try:
        deadline = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected an ISO 8601 time with UTC offset") from error
    if deadline.utcoffset() != timezone.utc.utcoffset(None):
        raise argparse.ArgumentTypeError("use a UTC timestamp ending in Z or +00:00")
    return deadline


def time_remaining(deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError("the comparison deadline passed")
    return remaining


def synthesize_identity(openssl, directory, deadline):
    (directory / "server.ext").write_text(
        "basicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature\n"
        "extendedKeyUsage=serverAuth\n"
        "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1\n",
        encoding="ascii",
    )
    commands = (
        ("req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256",
         "-nodes", "-subj", "/CN=Outbound comparison CA", "-days", "2", "-keyout", "ca.key",
         "-out", "ca.pem", "-addext", "basicConstraints=critical,CA:TRUE",
         "-addext", "keyUsage=critical,keyCertSign,cRLSign"),
        ("req", "-new", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
         "-subj", "/CN=localhost", "-keyout", "server.key", "-out", "server.csr"),
        ("x509", "-req", "-in", "server.csr", "-CA", "ca.pem", "-CAkey", "ca.key",
         "-CAcreateserial", "-days", "2", "-extfile", "server.ext", "-out", "server.pem"),
        ("genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", "client.key"),
    )
    for command in commands:
        result = subprocess.run([openssl, *command], cwd=directory, capture_output=True,
                                timeout=min(20, time_remaining(deadline)), check=False)
        if result.returncode:
            raise RuntimeError(result.stderr.decode(errors="replace").strip())


def terminate(process):
    with suppress(ProcessLookupError):
        os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        with suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=3)


@contextmanager
def child(command, directory, stdout_name, stderr_name, env=None):
    with (directory / stdout_name).open("wb") as stdout, (directory / stderr_name).open("wb") as stderr:
        process = subprocess.Popen(command, cwd=directory, stdout=stdout, stderr=stderr, env=env,
                                   stdin=subprocess.DEVNULL, start_new_session=True)
        try:
            yield process
        finally:
            if process.poll() is None:
                terminate(process)


def ready_notice(process, stdout, deadline):
    with stdout.open(encoding="utf-8") as output:
        while True:
            time_remaining(deadline)
            output.seek(0)
            ready = output.readline()
            if ready.endswith("\n"):
                return json.loads(ready)
            if process.poll() is not None:
                raise RuntimeError(f"fixture exited before reaching readiness ({process.returncode})")
            time.sleep(0.05)


def fixture_state(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return {
        "user_ticks": int(fields[11]),
        "system_ticks": int(fields[12]),
        "ticks_per_second": os.sysconf("SC_CLK_TCK"),
        "effective_cpus": sorted(os.sched_getaffinity(pid)),
    }


def fixture_stats(host, port, timeout):
    connection = http.client.HTTPConnection(host, port, timeout=timeout)
    try:
        connection.request("GET", "/stats")
        response = connection.getresponse()
        if response.status != 200:
            raise RuntimeError(f"fixture /stats returned HTTP {response.status}")
        return json.load(response)
    finally:
        connection.close()


def distribution(values):
    if not values:
        return {"samples": 0}
    ordered = sorted(values)
    return {
        "samples": len(ordered), "min": ordered[0], "max": ordered[-1],
        "p50": ordered[math.ceil(len(ordered) * 0.50) - 1],
        "p95": ordered[math.ceil(len(ordered) * 0.95) - 1],
        "p99": ordered[math.ceil(len(ordered) * 0.99) - 1],
    }


def summarize_windows(raw, behaviour_only):
    report = []
    for window in raw["windows"]:
        outcomes = {}
        for sample in window["samples"]:
            outcomes.setdefault(sample["outcome"], []).append(sample)
        observation = {
            name: {
                "latency_ns": distribution([item["latency_ns"] for item in samples]),
                "response_bytes": sum(item["bytes"] for item in samples),
                "statuses": dict(Counter(str(item["status"]) for item in samples)),
                "checksums": dict(Counter(str(item["checksum"]) for item in samples)),
            }
            for name, samples in outcomes.items()
        }
        completed = window["completed"]
        if len(window["samples"]) != completed or completed > window["offered"]:
            raise ValueError("caller receipt has inconsistent sample, completion or offered counts")
        counts = {name: len(samples) for name, samples in outcomes.items()}
        outcome_counts = {name: counts.get(name, 0) for name in ("successful", "refused", "timed_out", "cancelled")}
        outcome_counts["failed"] = completed - sum(outcome_counts.values())
        report.append({
            **{key: value for key, value in window.items() if key != "samples"},
            "cpu_ns_per_completion": window["cpu_ns"] / completed if completed else None,
            "latency_ns": distribution([sample["latency_ns"] for sample in window["samples"]]),
            "outcome_counts": outcome_counts,
            "outcomes": observation,
        })
    if not report and not behaviour_only:
        raise ValueError("caller receipt has no observation windows")
    return report


def run_cell(args, tools, cell_dir, cell, artifact, identities, deadline):
    cell_dir.mkdir()
    invocation = {
        "started_utc": utc_now(), "status": "running", "workload": asdict(cell),
        "artifact": artifact, "count": args.calls, "warmup": args.warmup_calls,
        "workers": args.worker_threads, "hold_ms": args.hold_ms, "token_ttl_seconds": args.ttl,
        "caller_cpus": args.caller_cpus, "fixture_cpus": args.fixture_cpus,
        "instrumentation": args.instrument,
    }
    receipt = cell_dir / "cell.json"
    write_json(receipt, invocation)
    caller_affinity = ",".join(map(str, args.caller_cpus))
    fixture_affinity = ",".join(map(str, args.fixture_cpus))
    fixture_command = [
        tools["taskset"], "--cpu-list", fixture_affinity, sys.executable, "-u", str(args.fixture),
        "--bind", cell.bind, "--cert", str(identities / "server.pem"), "--key", str(identities / "server.key"),
        "--http-port", "0", "--grpc-port", "0", "--control-port", "0", "--token-ttl", str(args.ttl),
    ]
    try:
        cell_deadline = min(deadline, time.monotonic() + args.cell_timeout)
        with child(fixture_command, cell_dir, "fixture.stdout", "fixture.stderr") as fixture:
            ready = ready_notice(fixture, cell_dir / "fixture.stdout", cell_deadline)
            fixture_port = ready["control_port"]
            invocation["fixture_ready"] = ready
            invocation["fixture_before_caller"] = fixture_state(fixture.pid)
            if invocation["fixture_before_caller"]["effective_cpus"] != args.fixture_cpus:
                raise RuntimeError("fixture did not start with the requested CPU affinity")
            write_json(receipt, invocation)
            command = [
                artifact["path"], "--scenario", cell.scenario, "--count", str(args.calls),
                "--concurrency", str(cell.concurrency), "--warmup", str(args.warmup_calls),
                "--http-port", str(ready["http_port"]), "--control-port", str(fixture_port),
                "--grpc-port", str(ready["grpc_port"]), "--ca", str(identities / "ca.pem"),
                "--ttl", str(args.ttl), "--telemetry", cell.telemetry, "--shape", cell.shape,
                "--hold-ms", str(args.hold_ms), "--bind", cell.bind, "--workers", str(args.worker_threads),
            ]
            if args.instrument == "heaptrack":
                command = [tools["heaptrack"], "-q", "--record-only", "-o", str(cell_dir / "heaptrack"), *command]
            environment = os.environ.copy()
            environment.pop("SSL_CERT_DIR", None)
            environment.pop("LD_PRELOAD", None)
            environment.pop("LD_AUDIT", None)
            environment["SSL_CERT_FILE"] = str(identities / "ca.pem")
            try:
                command = [tools["taskset"], "--cpu-list", caller_affinity, *command]
                with child(command, cell_dir, "caller.json", "caller.stderr", environment) as caller:
                    invocation["caller_launcher_pid"] = caller.pid
                    invocation["caller_exit"] = caller.wait(timeout=time_remaining(cell_deadline))
                    if invocation["caller_exit"] != 0:
                        raise RuntimeError(f"caller exited with status {invocation['caller_exit']}")
            finally:
                try:
                    invocation["fixture_after_caller"] = fixture_state(fixture.pid)
                    final_stats = fixture_stats(cell.bind, fixture_port, 1)
                    write_json(cell_dir / "fixture-final.json", final_stats)
                except (OSError, ValueError, RuntimeError, http.client.HTTPException) as error:
                    invocation["fixture_capture_error"] = str(error)
            if "fixture_capture_error" in invocation:
                raise RuntimeError("could not capture final fixture state")
        invocation["fixture_exit"] = fixture.returncode
        if fixture.returncode != 0:
            raise RuntimeError(f"fixture shut down with status {fixture.returncode}")
        if args.instrument == "heaptrack":
            traces = sorted(path.name for path in cell_dir.glob("heaptrack*.zst"))
            traces.extend(sorted(path.name for path in cell_dir.glob("heaptrack*.gz")))
            if not traces or any((cell_dir / name).stat().st_size == 0 for name in traces):
                raise RuntimeError("heaptrack produced no usable profile file")
            invocation["heap_profiles"] = traces
        raw = read_json(cell_dir / "caller.json")
        expected = {"scenario": cell.scenario, "shape": cell.shape, "telemetry": cell.telemetry,
                    "warmup": args.warmup_calls, "concurrency": cell.concurrency, "worker_threads": args.worker_threads}
        if any(raw.get(key) != value for key, value in expected.items()):
            raise RuntimeError("caller receipt did not describe the requested cell")
        if cpu_set(raw["caller_effective_cpus"]) != args.caller_cpus:
            raise RuntimeError("caller did not run with the requested CPU affinity")
        valid = raw.get("valid", True) is True
        if cell.shape == "unstarted-retry":
            marker = "outbound-perf: injected unstarted retry on a reused connection"
            probes = (cell_dir / "caller.stderr").read_text().count(marker)
            invocation["unstarted_retry_probe_count"] = probes
            valid = valid and probes == 1
        if valid and cell.scenario != "service-demand":
            connection_error = validate_peer_connections(cell, raw)
            if connection_error:
                invocation["connection_validation_error"] = connection_error
                valid = False
        no_timing = cell.scenario in {"service-demand", "http-retain"} or not valid
        invocation["windows"] = summarize_windows(raw, no_timing)
        if valid and any(window["offered"] != args.calls for window in invocation["windows"]):
            raise RuntimeError("caller receipt offered a different population than requested")
        invocation["caller_evidence"] = {key: value for key, value in raw.items() if key != "windows"}
        invocation["status"] = "complete" if valid else "invalid"
        return invocation
    except Exception as error:
        invocation["status"] = "incomplete"
        invocation["error"] = f"{type(error).__name__}: {error}"
        raise RuntimeError(f"{cell_dir.name}: {invocation['error']}") from error
    finally:
        invocation["finished_utc"] = utc_now()
        write_json(receipt, invocation)


def caller_work(window):
    return (
        window["offered"], window["completed"],
        {name: {key: value for key, value in outcomes.items() if key != "latency_ns"}
         for name, outcomes in window["outcomes"].items()},
    )


def measured_peer_work(receipt):
    before = receipt.get("peer_before", receipt.get("before"))
    after = receipt.get("peer_after", receipt.get("after"))
    connections = ("http_connections", "grpc_connections")
    if not all(isinstance(snapshot, dict) and all(key in snapshot for key in connections)
               for snapshot in (before, after)):
        return None
    counters = (*connections, "service_grants", "service_responses", "exchange_grants", "exchange_responses",
                "invalid_grants", "resources", "old_token_requests", "replacement_token_requests",
                "exchange_token_requests", "unauthorized", "anonymous_requests", "http_request_bytes",
                "http_response_bytes", "http_response_aborts", "http_late_errors", "grpc_requests",
                "grpc_messages", "grpc_traceparent", "grpc_request_bytes", "grpc_old_token_requests",
                "grpc_replacement_token_requests", "grpc_exchange_token_requests", "grpc_unauthorized",
                "grpc_anonymous_requests")
    return {key: after[key] - before[key] for key in counters if key in before and key in after}


def validate_peer_connections(cell, receipt):
    work = measured_peer_work(receipt)
    if work is None or work["http_connections"] < 0 or work["grpc_connections"] < 0:
        return "missing or decreasing peer connection counts"
    warm_http = cell.scenario in {"service-before", "service-refresh", "exchange", "http-retain"} or (
        cell.scenario == "http-buffered" and cell.shape not in {
            "caller-host", "bad-scheme", "bad-port", "bad-origin", "userinfo",
            "late-error", "retry", "unstarted-retry",
        }
    )
    if warm_http:
        before = receipt.get("peer_before", receipt.get("before"))
        if before["http_connections"] < 1 or work["http_connections"] != 0:
            return "warm HTTP window did not reuse established connections throughout"
    return None


def matched_deltas(reference_cell, candidate_cell, instrumented):
    reference = reference_cell["windows"]
    candidate = candidate_cell["windows"]
    reference_peer = measured_peer_work(reference_cell["caller_evidence"])
    candidate_peer = measured_peer_work(candidate_cell["caller_evidence"])
    peer_match = reference_peer == candidate_peer if reference_peer is not None and candidate_peer is not None else None
    valid = reference_cell["status"] == candidate_cell["status"] == "complete"
    peer_work = {"both_cells_valid": valid, "peer_work_matches": peer_match, "reference_peer_deltas": reference_peer,
                 "candidate_peer_deltas": candidate_peer}
    if len(reference) != len(candidate):
        return {**peer_work, "window_count_matches": False, "windows": []}
    differences = []
    for left, right in zip(reference, candidate):
        same_work = caller_work(left) == caller_work(right)
        difference = {"caller_work_matches": same_work}
        if valid and same_work and peer_match is True and left["completed"] and not instrumented:
            difference.update({
                "cpu_ns_per_completion": right["cpu_ns_per_completion"] - left["cpu_ns_per_completion"],
                "p99_ns": right["latency_ns"]["p99"] - left["latency_ns"]["p99"],
            })
        differences.append(difference)
    return {**peer_work, "window_count_matches": True, "windows": differences}


def paired_variation(pairs):
    variation = {}
    for pair in pairs:
        for index, window in enumerate(pair.get("comparison", {}).get("windows", [])):
            if "p99_ns" in window:
                deltas = variation.setdefault(pair["scenario"], {}).setdefault(index, {"cpu_ns_per_completion": [], "p99_ns": []})
                for metric in deltas:
                    deltas[metric].append(window[metric])
    return {
        cell: {index: {metric: {"paired_deltas": values, "median": statistics.median(values),
                                "min": min(values), "max": max(values)}
                       for metric, values in metrics.items()}
               for index, metrics in windows.items()}
        for cell, windows in variation.items()
    }


def host_metadata(cpus):
    def read_host_file(path):
        with suppress(OSError):
            return Path(path).read_text().strip()
        return None

    cpuinfo = read_host_file("/proc/cpuinfo") or ""
    models = sorted({line.partition(":")[2].strip() for line in cpuinfo.splitlines()
                     if line.split(":", 1)[0].strip() in {"model name", "Hardware"}})
    return {
        "hostname": platform.node(), "kernel": platform.uname()._asdict(), "cpu_models": models,
        "os_release": read_host_file("/etc/os-release"), "clocksource": read_host_file(
            "/sys/devices/system/clocksource/clocksource0/current_clocksource"),
        "available_cpus": sorted(os.sched_getaffinity(0)),
        "thread_siblings": {cpu: read_host_file(
            f"/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list") for cpu in cpus},
        "python": sys.version, "runner_pid": os.getpid(),
    }


def command_version(path, *arguments):
    return subprocess.check_output([path, *arguments], text=True, stderr=subprocess.STDOUT,
                                   timeout=10).strip()


def build_identity(binary, receipt_path, runner_hash, fixture_hash, instrument):
    binary_hash = sha256(binary)
    receipt = read_json(receipt_path)
    if receipt.get("build_succeeded") is not True or receipt.get("binary_sha256") != binary_hash:
        raise ValueError(f"binary does not match a successful build receipt: {binary}")
    if receipt["cargo_args"][1:4] != ["build", "--release", "--locked"]:
        raise ValueError(f"expected a locked release artifact: {binary}")
    overlay = receipt["overlay_sha256"]
    if overlay.get("run.py") != runner_hash or overlay.get("fixture.py") != fixture_hash:
        raise ValueError(f"runner or fixture differs from the build overlay: {binary}")
    if instrument == "heaptrack" and receipt.get("heap_symbols") is not True:
        raise ValueError(f"heaptrack requires the corresponding --heap-symbols build: {binary}")
    if not re.fullmatch(r"--remap-path-prefix=/.+=/experiment-source", receipt["rustflags"]):
        raise ValueError(f"unexpected build flags in {receipt_path}")
    return {"path": str(binary), "sha256": binary_hash, "build": receipt}


def interrupted(signum, _frame):
    for caught in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(caught, signal.SIG_IGN)
    raise InterruptedError(f"received {signal.Signals(signum).name}")


def main():
    parser = argparse.ArgumentParser(description="Run paired Linux outbound-call experiment cells")
    parser.add_argument("--list-scenarios", action="store_true")
    parser.add_argument("--artifact", type=labelled_path, action="append", metavar="LABEL=BINARY")
    parser.add_argument("--artifact-receipt", type=labelled_path, action="append", metavar="LABEL=JSON")
    parser.add_argument("--overlay-receipt", type=Path)
    parser.add_argument("--host-receipt", type=Path)
    parser.add_argument("--fixture", type=Path, default=Path(__file__).with_name("fixture.py"))
    parser.add_argument("--output", type=Path)
    parser.add_argument("--caller-cpus", type=cpu_set)
    parser.add_argument("--fixture-cpus", type=cpu_set)
    parser.add_argument("--scenario", choices=tuple(CELLS), action="append", metavar="CELL")
    parser.add_argument("--pairs", type=positive_int, default=4)
    parser.add_argument("--calls", type=positive_int, default=2048)
    parser.add_argument("--warmup-calls", type=positive_int, default=256)
    parser.add_argument("--worker-threads", type=positive_int)
    parser.add_argument("--cell-timeout", type=positive_int, default=90)
    parser.add_argument("--ttl", type=positive_int, default=30)
    parser.add_argument("--hold-ms", type=int, default=1000)
    parser.add_argument("--deadline-utc", type=deadline_utc)
    parser.add_argument("--instrument", choices=("none", "heaptrack"), default="none")
    args = parser.parse_args()
    if args.list_scenarios:
        for name, cell in CELLS.items():
            print(f"{name}\t{cell.group}")
        return 0
    if sys.platform != "linux":
        parser.error("performance execution requires the authorized Linux host")
    if not all((args.artifact, args.output, args.caller_cpus, args.fixture_cpus, args.deadline_utc)):
        parser.error("two --artifact arguments, --output, CPU sets and --deadline-utc are required")
    artifacts = dict(args.artifact)
    if len(args.artifact) != 2 or len(artifacts) != 2:
        parser.error("specify two distinctly labelled artifacts, reference first and candidate second")
    if args.pairs % 2 != 0:
        parser.error("--pairs must be even to balance reference-first and candidate-first ordering")
    if args.hold_ms < 0:
        parser.error("--hold-ms must be non-negative")
    caller_cpus, fixture_cpus = set(args.caller_cpus), set(args.fixture_cpus)
    available_cpus = os.sched_getaffinity(0)
    if caller_cpus & fixture_cpus or not (caller_cpus | fixture_cpus) <= available_cpus:
        parser.error("caller and fixture CPU sets must be disjoint and available to the runner")
    args.worker_threads = args.worker_threads or len(caller_cpus)
    selected = args.scenario or [name for name, cell in CELLS.items() if cell.group == "final"]
    if len(selected) != len(set(selected)):
        parser.error("select each scenario only once per run")
    retained = [CELLS[name].scenario == "http-retain" for name in selected]
    if args.instrument == "heaptrack" and not all(retained):
        parser.error("select retained-response cells only for a heaptrack run")
    if any(retained) and not all(retained):
        parser.error("select retained-response cells in a separate run")
    concurrency = max(CELLS[name].concurrency for name in selected)
    if min(args.calls, args.warmup_calls) < concurrency:
        parser.error("call and warm-up counts must each admit the selected concurrency")
    if any(CELLS[name].scenario in {"service-before", "service-refresh", "service-demand", "exchange"}
           for name in selected) and not 18 <= args.ttl <= 120:
        parser.error("credential cells require a --ttl from 18 through 120 seconds")
    seconds_remaining = (args.deadline_utc - datetime.now(timezone.utc)).total_seconds()
    if seconds_remaining <= 0:
        parser.error("--deadline-utc has already passed")
    deadline = time.monotonic() + seconds_remaining

    required_tools = ["taskset", "openssl", "curl"]
    if args.instrument == "heaptrack":
        required_tools.append("heaptrack")
    tools = {tool: shutil.which(tool) for tool in required_tools}
    for name, path in tools.items():
        if not path:
            parser.error(f"required executable is missing: {name}")
    args.fixture = args.fixture.resolve(strict=True)
    receipts = dict(args.artifact_receipt or ())
    if set(receipts) - set(artifacts) or len(receipts) != len(args.artifact_receipt or ()):
        parser.error("artifact receipt labels must refer to distinct supplied artifacts")
    runner_hash = sha256(Path(__file__))
    fixture_hash = sha256(args.fixture)
    identity = {}
    for label, binary in artifacts.items():
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"artifact is not an executable file: {binary}")
        build_receipt = receipts.get(label, binary.with_name("build.json"))
        try:
            identity[label] = build_identity(binary, build_receipt, runner_hash, fixture_hash, args.instrument)
        except (OSError, ValueError, KeyError, TypeError) as error:
            parser.error(f"invalid build receipt {build_receipt}: {error}")

    labels = list(identity)
    reference, candidate = (identity[label]["build"] for label in labels)
    comparable_inputs = ("original_lock_sha256", "mounted_lock_sha256", "overlay_sha256", "rustc", "cargo", "retry_probe_sha256",
                         "host_platform", "cargo_args", "heap_symbols", "profile_overrides", "target", "allocator")
    mismatches = [key for key in comparable_inputs if key not in reference or key not in candidate
                  or reference[key] != candidate[key]]
    if mismatches:
        parser.error(f"artifacts differ in comparison build inputs: {', '.join(mismatches)}")
    retry_cell = "http-buffered-unstarted-retry"
    if (retry_cell in selected or reference["retry_probe_sha256"]) and (
        selected != [retry_cell] or not reference["retry_probe_sha256"]
    ):
        parser.error("the unstarted retry probe requires its sole diagnostic cell and matching --retry-probe artifacts")
    args.output = args.output.resolve()
    if args.output.is_relative_to(Path(__file__).resolve().parent):
        parser.error("save comparison evidence outside the frozen experiment overlay")
    args.output.mkdir(parents=True, exist_ok=False)
    manifest = {
        "started_utc": utc_now(), "deadline_utc": args.deadline_utc.isoformat(), "status": "running",
        "reference": labels[0], "candidate": labels[1], "artifacts": identity,
        "runner_sha256": runner_hash, "fixture_sha256": fixture_hash,
        "host": host_metadata(sorted(caller_cpus | fixture_cpus)),
        "host_receipt": read_json(args.host_receipt) if args.host_receipt else None,
        "overlay_receipt": read_json(args.overlay_receipt) if args.overlay_receipt else None,
        "tools": {"openssl": command_version(tools["openssl"], "version"),
                  "taskset": command_version(tools["taskset"], "--version"),
                  "curl": command_version(tools["curl"], "--version")},
        "protocol": {
            "scenarios": {name: asdict(CELLS[name]) for name in selected}, "scenario_order": selected,
            "pairs_per_scenario": args.pairs,
            "calls": args.calls, "warmup_calls": args.warmup_calls, "worker_threads": args.worker_threads,
            "cell_timeout_seconds": args.cell_timeout, "ttl_seconds": args.ttl, "hold_ms": args.hold_ms,
            "caller_cpus": args.caller_cpus, "fixture_cpus": args.fixture_cpus, "instrumentation": args.instrument,
        },
    }
    if args.instrument == "heaptrack":
        manifest["tools"]["heaptrack"] = command_version(tools["heaptrack"], "--version")
    manifest_path = args.output / "run.json"
    write_json(manifest_path, manifest)
    for caught in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(caught, interrupted)

    pairs = []
    summary = args.output / "summary.json"
    try:
        runner_cpus = available_cpus - caller_cpus - fixture_cpus or fixture_cpus
        os.sched_setaffinity(0, runner_cpus)
        manifest["host"]["runner_cpus"] = sorted(os.sched_getaffinity(0))
        write_json(manifest_path, manifest)
        with tempfile.TemporaryDirectory(prefix="outbound-perf-identity-") as temporary:
            identities = Path(temporary)
            synthesize_identity(tools["openssl"], identities, deadline)
            manifest["tls"] = {"ca_pem_sha256": sha256(identities / "ca.pem"),
                               "server_pem_sha256": sha256(identities / "server.pem"),
                               "sans": ["localhost", "127.0.0.1", "::1"]}
            write_json(manifest_path, manifest)
            for repetition in range(args.pairs):
                for index, scenario in enumerate(selected):
                    order = labels if (repetition + index) % 2 == 0 else labels[::-1]
                    pair = {"scenario": scenario, "repetition": repetition + 1, "order": order, "cells": {}}
                    pairs.append(pair)
                    write_json(summary, {"pairs": pairs, "observed_paired_deltas": paired_variation(pairs)})
                    results = {}
                    for label in order:
                        time_remaining(deadline)
                        name = f"{repetition + 1:03d}-{scenario}-{label}"
                        pair["cells"][label] = f"{name}/cell.json"
                        results[label] = run_cell(args, tools, args.output / name, CELLS[scenario],
                                                  identity[label], identities, deadline)
                    pair["comparison"] = matched_deltas(results[labels[0]], results[labels[1]],
                                                        args.instrument != "none" or all(retained) or scenario == retry_cell)
                    write_json(summary, {"pairs": pairs, "observed_paired_deltas": paired_variation(pairs)})
        invalid = [pair for pair in pairs if not pair["comparison"]["both_cells_valid"]]
        manifest["invalid_pair_count"] = len(invalid)
        manifest["unmatched_pair_count"] = sum(
            not pair["comparison"]["window_count_matches"]
            or pair["comparison"]["peer_work_matches"] is False
            or any(not window["caller_work_matches"] for window in pair["comparison"]["windows"])
            for pair in pairs
        )
        manifest["status"] = "complete-with-invalid-cells" if invalid else "complete"
        return 1 if invalid else 0
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        manifest["status"] = "incomplete"
        manifest["error"] = str(error)
        print(error, file=sys.stderr)
        return 1
    finally:
        write_json(summary, {"pairs": pairs, "observed_paired_deltas": paired_variation(pairs)})
        manifest["finished_utc"] = utc_now()
        write_json(manifest_path, manifest)


if __name__ == "__main__":
    raise SystemExit(main())
