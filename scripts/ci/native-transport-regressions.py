#!/usr/bin/env python3
"""Locked native transport graphs, selected tests, and source-bound receipts.

Excluded manifests retain their published test harnesses. Cargo owns resolution
and execution; this entry checks applicability and runs exact filters serially.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


@dataclass(frozen=True)
class NativeTarget:
    package: str
    version: str
    features: tuple[str, ...]
    filters: tuple[str, ...]

    @property
    def manifest(self) -> Path:
        return ROOT / "vendor" / self.package / "Cargo.toml"


TARGETS = (
    NativeTarget("sqlx-core", "0.9.0", ("_rt-tokio", "_tls-rustls-aws-lc-rs"), (
        "net::socket::tests::pending_first_candidate_does_not_starve_a_healthy_socket",
        "net::socket::tests::empty_resolution_preserves_tokio_invalid_input",
        "net::socket::tests::all_failure_reports_last_resolver_address_even_when_it_fails_first",
    )),
    NativeTarget("async-nats", "0.50.0", ("aws-lc-rs", "jetstream", "nkeys", "tokio-rustls/tls12"), (
        "tests::initial_connect_deadline_refuses_new_attempts_after_expiry",
        "tests::transport_resilience::pending_system_dns_spends_the_server_attempt_budget",
        "tests::transport_resilience::stalled_first_address_leaves_time_for_the_next_address",
        "tests::transport_resilience::pending_tls_handshake_spends_the_server_attempt_budget",
        "tests::transport_resilience::same_subscriber_recovers_after_pending_system_dns",
        "tests::transport_resilience::forced_close_finishes_recovery_and_drops_queued_work_before_receipt",
        "tests::transport_resilience::raw_subscriber_retains_runner_after_last_client_is_dropped",
        "tests::transport_resilience::last_subscriber_drop_closes_full_queue_during_unavailable_reconnect",
        "tests::transport_resilience::last_owner_drop_terminates_background_initial_recovery",
        "tests::transport_resilience::graceful_close_reports_completion_and_one_closed_event",
        "tests::transport_resilience::lost_runner_does_not_report_observed_completion",
        "tests::transport_resilience::initial_deadline_is_cleared_before_background_reconnect",
        "tests::transport_resilience::initial_deadline_terminates_background_initial_retry",
        "tests::multiplexer_prunes_abandoned_requests",
        "tests::multiplexer_keeps_pending_requests",
        "tests::multiplexer_prunes_abandoned_requests_after_burst",
        "tests::multiplexer_releases_capacity_after_burst",
        "jetstream::context::publish_ack_tests::passive_publication_observation_preserves_native_admission_and_ack",
        "jetstream::context::publish_ack_tests::polled_ack_cancellation_retains_capacity_until_ack_or_cleanup_expiry",
        "jetstream::context::publish_ack_tests::native_ack_timeout_closes_receiver_without_another_cleanup_wait",
        "jetstream::context::publish_ack_tests::terminal_ack_results_release_capacity_without_cleanup_handoff",
        "jetstream::consumer::pull::batch_completion_tests::completion_before_or_between_buffered_data_keeps_every_delivery",
        "jetstream::consumer::pull::batch_completion_tests::completion_without_expiry_waits_for_delayed_allocated_data",
        "jetstream::consumer::pull::batch_completion_tests::partial_and_empty_batches_keep_their_existing_termination",
        "jetstream::consumer::pull::batch_completion_tests::completion_preserves_the_existing_watchdog",
    )),
    NativeTarget("aws-smithy-http-client", "1.4.2", ("rustls-aws-lc", "hyper-rustls/aws-lc-rs"), (
        "client::test::same_family_candidate_fallback_and_inner_timeout_classification",
    )),
    NativeTarget("hyper-util", "0.1.21", ("client-legacy", "tokio"), (
        "client::legacy::connect::http::tests::same_family_candidate_race_preserves_late_dns_and_caller_budget",
    )),
)

# These features select the exercised runtime, resolver, TLS, and connector.
# Other feature differences are recorded, including test utilities and protocol
# modules not exercised by the selected private transport filters.
MECHANISM_FEATURES = {
    "sqlx-core": {"_rt-tokio", "_rt-async-io", "_tls-native-tls", "_tls-rustls", "_tls-rustls-aws-lc-rs", "_tls-rustls-ring-webpki", "_tls-rustls-ring-native-roots"},
    "async-nats": {"aws-lc-rs", "ring", "jetstream", "nkeys", "websockets"},
    "aws-smithy-http-client": {"default-client", "hyper-014", "rustls-aws-lc", "rustls-aws-lc-fips", "rustls-ring", "s2n-tls"},
    "hyper-util": {"client", "client-legacy", "tokio"},
    "tokio": {"rt", "net", "time"},
    "rustls": {"aws_lc_rs", "ring", "fips", "std", "tls12", "custom-provider"},
    "tokio-rustls": {"aws_lc_rs", "ring", "fips", "tls12"},
    "hyper-rustls": {"aws-lc-rs", "ring", "rustls-native-certs", "webpki-roots", "native-tokio", "webpki-tokio", "tls12"},
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def run(args: list[str], *, checked: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(args, cwd=ROOT, text=True, capture_output=True, check=False)
    if checked and result.returncode:
        raise RuntimeError(f"{' '.join(args)} exited {result.returncode}\n{result.stderr}")
    return result


def sha256(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def source_identity(directory: Path) -> dict[str, str]:
    paths = sorted(path for path in directory.rglob("*") if path.is_file()
                   and "target" not in path.relative_to(directory).parts
                   and "__pycache__" not in path.relative_to(directory).parts)
    return {str(path.relative_to(ROOT)): sha256(path) for path in paths}


def flags(target: NativeTarget) -> list[str]:
    return ["--manifest-path", str(target.manifest), "--no-default-features", "--features", ",".join(target.features)]


def metadata(manifest: Path, triple: str, feature_flags: list[str] = ()) -> dict:
    return json.loads(run(["cargo", "metadata", "--locked", "--format-version=1", "--filter-platform", triple,
                           "--manifest-path", str(manifest), *feature_flags]).stdout)


def graph(manifest: Path, triple: str, selection: list[str], edges: str) -> dict[tuple[str, str], set[str]]:
    result = run(["cargo", "tree", "--locked", "--manifest-path", str(manifest), "--target", triple,
                  "--prefix", "none", "--format", "{p}|{f}", "--no-dedupe", "-e", edges, *selection])
    nodes: dict[tuple[str, str], set[str]] = {}
    for line in result.stdout.splitlines():
        match = re.fullmatch(r"([A-Za-z0-9_-]+) v([^ ]+)(?: .*?)?\|(.*)", line.strip())
        if match:
            nodes.setdefault((match[1], match[2]), set()).update(filter(None, match[3].split(",")))
    require(bool(nodes), f"empty Cargo {edges} graph for {manifest}")
    return nodes


def identities(document: dict, lock_path: Path) -> dict[tuple[str, str], dict[str, object]]:
    locks = tomllib.loads(lock_path.read_text())["package"]
    checksums = {(entry["name"], entry["version"], entry.get("source")): entry.get("checksum") for entry in locks}
    result = {}
    for package in document["packages"]:
        key = (package["name"], package["version"])
        lock_key = (*key, package["source"])
        require(lock_key in checksums, f"metadata package has no locked identity: {lock_key}")
        checksum = checksums[lock_key]
        require(not str(package["source"]).startswith("registry+") or bool(checksum),
                f"registry package has no locked checksum: {key}")
        identity = {"source": package["source"], "checksum": checksum}
        if package["source"] is None:
            identity["source_files"] = source_identity(Path(package["manifest_path"]).parent)
        if key in result:
            require(result[key] == identity, f"ambiguous source for {key}")
        result[key] = identity
    return result


def graph_witness(target: NativeTarget, triple: str, root_ids: dict) -> dict:
    root = graph(ROOT / "Cargo.toml", triple, ["-p", target.package], "normal,build")
    selection = ["--no-default-features", "--features", ",".join(target.features)]
    normal = graph(target.manifest, triple, selection, "normal,build")
    tested = graph(target.manifest, triple, selection, "normal,build,dev")
    native_ids = identities(metadata(target.manifest, triple, selection), target.manifest.with_name("Cargo.lock"))
    differences = {}
    for key, normal_features in normal.items():
        require(key in root, f"{target.package}: normal/build package {key} absent from root native closure")
        require(native_ids[key] == root_ids[key], f"{target.package}: source/checksum mismatch for {key}")
        actual = tested[key]
        relevant = MECHANISM_FEATURES.get(key[0], set())
        require(actual & relevant == root[key] & relevant,
                f"{target.package}: mechanism feature mismatch for {key}: native={sorted(actual & relevant)}, root={sorted(root[key] & relevant)}")
        if actual != root[key]:
            differences[f"{key[0]}@{key[1]}"] = {
                "root": sorted(root[key]), "native_normal_build": sorted(normal_features),
                "native_test": sorted(actual), "mechanism_features": sorted(relevant),
            }
    return {
        "target": triple,
        "normal_build": {f"{name}@{version}": {**native_ids[(name, version)], "features": sorted(features)}
                         for (name, version), features in sorted(normal.items())},
        "test_only_packages": {f"{name}@{version}": {**native_ids[(name, version)], "features": sorted(features)}
                               for (name, version), features in sorted(tested.items()) if (name, version) not in normal},
        "other_feature_differences": differences,
    }


def test_command(target: NativeTarget, triple: str) -> list[str]:
    return ["cargo", "test", "--locked", *flags(target), "--target", triple, "--lib"]


def target_receipt(target: NativeTarget, triple: str, root_ids: dict, *, plan: bool) -> dict:
    receipt = {"package": target.package, "features": list(target.features),
               "source_files": source_identity(target.manifest.parent),
               "graph": graph_witness(target, triple, root_ids),
               "expected_tests": list(target.filters), "executions": []}
    if target.package == "sqlx-core":
        receipt["separate_return_finality_proof"] = {
            "command": "make test-integration-db",
            "executed_by_this_helper": False,
            "sources": {path: sha256(ROOT / path) for path in (
                "test/tests/postgres.rs", "test/tests/support/commit_proxy.rs")},
            "tests": [
                "cancelled_operations_release_capacity_while_the_old_socket_stays_silent",
                "a_successful_statement_still_has_a_bounded_silent_return",
                "a_cancelled_readiness_ping_releases_its_silent_connection",
            ],
        }
    if plan:
        receipt["commands"] = [test_command(target, triple) + [name, "--", "--exact"] for name in target.filters]
        return receipt
    listed = run(test_command(target, triple) + ["--", "--list"])
    names = {line.removesuffix(": test") for line in listed.stdout.splitlines() if line.endswith(": test")}
    require(bool(target.filters), f"{target.package}: no selected tests")
    require(set(target.filters) <= names, f"{target.package}: missing exact native tests {sorted(set(target.filters) - names)}")
    receipt["listed_count"] = len(names)
    receipt["selected_count"] = len(target.filters)
    for name in target.filters:
        command = test_command(target, triple) + [name, "--", "--exact"]
        result = run(command, checked=False)
        match = re.search(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;", result.stdout)
        counts = {"passed": int(match[1]), "failed": int(match[2]), "ignored": int(match[3])} if match else None
        receipt["executions"].append({"name": name, "command": command, "returncode": result.returncode,
                                      "counts": counts, "stdout": result.stdout, "stderr": result.stderr})
        if result.returncode or counts != {"passed": 1, "failed": 0, "ignored": 0}:
            receipt["failure"] = f"{target.package}: exact filter did not pass one nonignored test: {name}"
            break
    return receipt


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--receipt-dir", type=Path, default=ROOT / ".ci/native-transport")
    parser.add_argument("--target", default=os.environ.get("CARGO_BUILD_TARGET") or os.environ.get("TARGET"))
    parser.add_argument("--plan", action="store_true", help="locked graph/feature diagnostics only; do not build or execute tests")
    args = parser.parse_args()
    rustc = run(["rustc", "-vV"]).stdout
    triple = args.target or next(line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: "))
    root_metadata = metadata(ROOT / "Cargo.toml", triple)
    root_ids = identities(root_metadata, ROOT / "Cargo.lock")
    receipt = {"candidate": run(["git", "rev-parse", "HEAD"]).stdout.strip(), "rustc": rustc,
               "target": triple, "toolchain_file_sha256": sha256(ROOT / "rust-toolchain.toml"),
               "root_manifest_sha256": sha256(ROOT / "Cargo.toml"), "root_lock_sha256": sha256(ROOT / "Cargo.lock"),
               "helper_sha256": sha256(Path(__file__)), "plan_only": args.plan, "targets": {}}
    require(("hyper-util", "0.1.21") in root_ids, "unconditional Hyper source is absent")
    for target in TARGETS:
        retained = (target.package, target.version) in root_ids
        require(retained == target.manifest.exists(), f"{target.package}: root/profile source containment differs")
        if retained:
            receipt["targets"][target.package] = target_receipt(target, triple, root_ids, plan=args.plan)
    args.receipt_dir.mkdir(parents=True, exist_ok=True)
    path = args.receipt_dir / "receipt.json"
    path.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    print(path)
    failures = [entry["failure"] for entry in receipt["targets"].values() if "failure" in entry]
    require(not failures, "; ".join(failures))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, ValueError, KeyError, StopIteration) as error:
        print(f"native transport regression refusal: {error}", file=sys.stderr)
        raise SystemExit(1) from error
