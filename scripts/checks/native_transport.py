#!/usr/bin/env python3
"""Run locked serial native transport regressions across retained vendor crates."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[2]


@dataclass(frozen=True)
class NativeTarget:
    package: str
    version: str
    features: tuple[str, ...]
    filters: tuple[str, ...]
    target_args: tuple[str, ...] = ("--lib",)

    @property
    def manifest(self) -> Path:
        return ROOT / "vendor" / self.package / "Cargo.toml"


TARGETS = (
    NativeTarget(
        "sqlx-core",
        "0.9.0",
        ("_rt-tokio", "_tls-rustls-aws-lc-rs"),
        (
            "net::socket::tests::pending_first_candidate_does_not_starve_a_healthy_socket",
            "net::socket::tests::empty_resolution_preserves_tokio_invalid_input",
            "net::socket::tests::all_failure_reports_last_resolver_address_even_when_it_fails_first",
        ),
    ),
    NativeTarget(
        "async-nats",
        "0.50.0",
        ("aws-lc-rs", "jetstream", "nkeys", "tokio-rustls/tls12"),
        (
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
            "tests::connect_info_debug_redacts_secrets",
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
        ),
    ),
    NativeTarget(
        "aws-smithy-http-client",
        "1.5.1",
        ("rustls-aws-lc", "hyper-rustls/aws-lc-rs"),
        ("client::test::same_family_candidate_fallback_and_inner_timeout_classification",),
    ),
    NativeTarget(
        "hyper-util",
        "0.1.21",
        ("client-legacy", "tokio"),
        ("client::legacy::connect::http::tests::same_family_candidate_race_preserves_late_dns_and_caller_budget",),
    ),
)

_RESULT_RE = re.compile(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def run(args: list[str], *, checked: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(args, cwd=ROOT, text=True, capture_output=True, check=False)
    if checked and result.returncode:
        raise RuntimeError(f"{' '.join(args)} exited {result.returncode}\n{result.stderr}")
    return result


def root_packages(triple: str) -> set[tuple[str, str]]:
    meta = json.loads(
        run(
            [
                "cargo",
                "metadata",
                "--locked",
                "--format-version=1",
                "--filter-platform",
                triple,
                "--manifest-path",
                str(ROOT / "Cargo.toml"),
            ]
        ).stdout
    )
    lock_entries = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))["package"]
    locked = {(entry["name"], entry["version"]) for entry in lock_entries}
    resolved = {(pkg["name"], pkg["version"]) for pkg in meta["packages"]}
    require(resolved <= locked, "Cargo metadata packages missing from Cargo.lock")
    return resolved


def test_command(target: NativeTarget, triple: str) -> list[str]:
    return [
        "cargo",
        "test",
        "--locked",
        "--manifest-path",
        str(target.manifest),
        "--no-default-features",
        "--features",
        ",".join(target.features),
        "--target",
        triple,
        *target.target_args,
    ]


def run_target(target: NativeTarget, triple: str, *, plan: bool) -> None:
    require(bool(target.filters), f"{target.package}: no selected tests")
    require(len(set(target.filters)) == len(target.filters), f"{target.package}: duplicate selected tests")
    base = test_command(target, triple)
    batched = [*base, "--", "--exact", "--test-threads=1", *target.filters]
    if plan:
        print(" ".join(batched))
        return
    listed = run([*base, "--", "--list"])
    names = {line.removesuffix(": test") for line in listed.stdout.splitlines() if line.endswith(": test")}
    missing = sorted(set(target.filters) - names)
    require(not missing, f"{target.package}: missing exact native tests {missing}")

    result = run(batched, checked=False)
    match = _RESULT_RE.search(result.stdout)
    counts = (int(match[1]), int(match[2]), int(match[3])) if match else None
    passed_names = {
        line.removeprefix("test ").removesuffix(" ... ok")
        for line in result.stdout.splitlines()
        if line.startswith("test ") and line.endswith(" ... ok")
    }
    expected_counts = (len(target.filters), 0, 0)
    if result.returncode or counts != expected_counts or passed_names != set(target.filters):
        raise RuntimeError(
            f"{target.package}: batched exact filter did not pass {len(target.filters)} nonignored tests "
            f"(counts={counts}, missing_ok={sorted(set(target.filters) - passed_names)})\n"
            f"{result.stdout}\n{result.stderr}"
        )
    print(f"{target.package}: {len(target.filters)} native transport regressions passed")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default=os.environ.get("CARGO_BUILD_TARGET") or os.environ.get("TARGET"))
    parser.add_argument("--plan", action="store_true", help="print locked commands without executing tests")
    args = parser.parse_args()
    rustc = run(["rustc", "-vV"]).stdout
    triple = args.target or next(line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: "))
    retained_packages = root_packages(triple)
    require(("hyper-util", "0.1.21") in retained_packages, "unconditional Hyper source is absent")
    for target in TARGETS:
        retained = (target.package, target.version) in retained_packages
        require(retained == target.manifest.exists(), f"{target.package}: root/profile source containment differs")
        if retained:
            run_target(target, triple, plan=args.plan)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, ValueError, KeyError, StopIteration) as error:
        print(f"native transport regression refusal: {error}", file=sys.stderr)
        raise SystemExit(1) from error
