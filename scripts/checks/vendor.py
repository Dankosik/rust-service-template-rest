#!/usr/bin/env python3
"""Verify vendored [patch.crates-io] integrity, upstream OSV/yank drift, and serial native transport regressions."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import tomllib
import urllib.error
import urllib.request
from collections.abc import Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from scripts import lib

COOLDOWN_DAYS = 7
USER_AGENT = "rust-service-template vendor-check"
VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+([.+_-][0-9A-Za-z.+_-]+)?$")
ISO_UTC_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.\d+)?Z$")
_RESULT_RE = re.compile(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;")


@dataclass(frozen=True)
class TransportTarget:
    package: str
    features: tuple[str, ...]
    filters: tuple[str, ...]


TRANSPORT_TARGETS: tuple[TransportTarget, ...] = (
    TransportTarget(
        "sqlx-core",
        ("_rt-tokio", "_tls-rustls-aws-lc-rs"),
        (
            "net::socket::tests::pending_first_candidate_does_not_starve_a_healthy_socket",
            "net::socket::tests::empty_resolution_preserves_tokio_invalid_input",
            "net::socket::tests::all_failure_reports_last_resolver_address_even_when_it_fails_first",
        ),
    ),
    TransportTarget(
        "async-nats",
        ("aws-lc-rs", "jetstream", "nkeys", "tokio-rustls/tls12"),
        (
            "tests::initial_connect_deadline_refuses_new_attempts_after_expiry",
            "connector::tests::tls_is_asked_for_by_every_tls_setting",
            "connector::tests::waits_after_the_first_pong_only_for_2_12_0_and_later",
            "connector::tests::connect_urls_entries_are_host_port",
            "connector::tests::discovered_servers_keep_the_transport_and_tls_they_were_learned_on",
            "connector::tests::a_server_learned_over_tls_is_verified_against_the_name_it_was_learned_under",
            "connector::tests::a_server_learned_without_tls_follows_the_configured_name",
            "connector::tests::a_server_that_drops_out_of_the_list_is_learned_again",
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
    TransportTarget(
        "aws-smithy-http-client",
        ("rustls-aws-lc", "hyper-rustls/aws-lc-rs"),
        (
            "client::request_connect_tests::varying_budgets_share_checkout_and_background_dial_keeps_its_timer",
            "client::request_connect_tests::cold_tls_timeout_and_abandonment_close_the_socket",
            "client::test::same_family_candidate_fallback_and_inner_timeout_classification",
        ),
    ),
    TransportTarget(
        "aws-runtime",
        ("http-1x",),
        (
            "content_encoding::body::http_body_1_x::tests::test_poll_frame_with_default_chunk_size",
            "content_encoding::body::http_body_1_x::tests::test_poll_frame_with_trailers",
            "content_encoding::body::http_body_1_x::tests::test_poll_frame_with_fragmented_data_and_trailers",
            "content_encoding::body::http_body_1_x::tests::test_aws_chunked_body_poll_frame_with_signer",
            "content_encoding::body::http_body_1_x::tests::test_aws_chunked_body_poll_frame_with_signer_and_trailers",
        ),
    ),
    TransportTarget(
        "hyper-util",
        ("client-legacy", "tokio"),
        ("client::legacy::connect::http::tests::same_family_candidate_race_preserves_late_dns_and_caller_budget",),
    ),
)


_MISE_ENV_PINS: tuple[tuple[str, str, str, str], ...] = (
    ("BUILDX_VERSION", "github", "docker/buildx", ""),
    ("BUILDKIT_IMAGE", "github", "moby/buildkit", ""),
    ("TRIVY_IMAGE", "github", "aquasecurity/trivy", ""),
    ("CARGO_MUTANTS_VERSION", "crates", "cargo-mutants", ""),
)

_DOCKERFILE_ARG_PINS: tuple[tuple[str, str, str, str], ...] = (
    ("CARGO_CHEF_VERSION", "crates", "cargo-chef", ""),
    ("CARGO_AUDITABLE_VERSION", "crates", "cargo-auditable", ""),
    ("CARGO_ABOUT_VERSION", "crates", "cargo-about", ""),
)


def _fetch_json(url: str, *, payload: dict[str, Any] | None = None) -> Any:
    headers = {"User-Agent": USER_AGENT}
    if url.startswith("https://api.github.com/"):
        headers["Accept"] = "application/vnd.github+json"
        if token := (os.environ.get("MISE_GITHUB_TOKEN") or os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")):
            headers["Authorization"] = f"Bearer {token}"
    raw = None
    if payload is not None:
        headers["Content-Type"] = "application/json"
        raw = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=raw, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except (urllib.error.URLError, TimeoutError, OSError, json.JSONDecodeError) as exc:
        raise lib.ScriptError(f"request failed for {url}: {exc}", exit_code=2)


def _release_age_days(published: str) -> int:
    match = ISO_UTC_RE.match(published)
    if not match:
        raise lib.ScriptError(f"invalid release publish timestamp: {published!r}", exit_code=2)
    dt = datetime.strptime(match.group(1), "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc)
    return int((datetime.now(timezone.utc) - dt).total_seconds() // 86400)


def _crate_versions(name: str) -> list[dict[str, Any]]:
    data = _fetch_json(f"https://crates.io/api/v1/crates/{name}")
    versions = data.get("versions")
    if data.get("crate", {}).get("name") != name or not isinstance(versions, list) or not versions:
        raise lib.ScriptError(f"invalid crates.io response for {name}", exit_code=2)
    return versions


def _latest_release(registry: str, package: str, prefix: str = "") -> tuple[str, int]:
    if registry == "crates":
        for entry in _crate_versions(package):
            num = entry.get("num", "")
            if not entry.get("yanked") and "-" not in num and num.startswith(prefix):
                return num, _release_age_days(entry["created_at"])
        raise lib.ScriptError(f"no matching stable crates.io release for {package} ({prefix or '*'})", exit_code=2)
    if registry == "github":
        releases = _fetch_json(f"https://api.github.com/repos/{package}/releases?per_page=30")
        if not isinstance(releases, list):
            raise lib.ScriptError(f"invalid GitHub releases response for {package}", exit_code=2)
        for entry in releases:
            if entry.get("draft") or entry.get("prerelease"):
                continue
            tag = str(entry.get("tag_name") or "").lstrip("v")
            if tag and "-" not in tag and tag.startswith(prefix):
                return tag, _release_age_days(entry["published_at"])
        raise lib.ScriptError(f"no matching stable GitHub release for {package} ({prefix or '*'})", exit_code=2)
    raise lib.ScriptError(f"unknown pin registry: {registry}", exit_code=2)


def _extract_version(raw: str) -> str:
    return raw.split(":", 1)[-1].split("@", 1)[0].lstrip("v")


def _check_pin_drift(root: Path) -> list[str]:
    findings: list[str] = []
    mise_cfg = lib.read_mise_config(root)
    mise_env = mise_cfg.get("env", {})
    for key, registry, package, prefix in _MISE_ENV_PINS:
        raw = mise_env.get(key)
        if not isinstance(raw, str) or not raw:
            continue
        current = _extract_version(raw)
        newest, age = _latest_release(registry, package, prefix)
        if newest != current and age >= COOLDOWN_DAYS:
            findings.append(f"mise.toml [env] {key}: {current} -> {newest} ({age} days old)")

    dockerfile = root / "build/docker/Dockerfile"
    if dockerfile.is_file():
        df_text = dockerfile.read_text(encoding="utf-8")
        for key, registry, package, prefix in _DOCKERFILE_ARG_PINS:
            match = re.search(rf"^ARG\s+{re.escape(key)}=([^\s]+)$", df_text, re.M)
            if not match:
                continue
            current = _extract_version(match.group(1))
            newest, age = _latest_release(registry, package, prefix)
            if newest != current and age >= COOLDOWN_DAYS:
                findings.append(f"build/docker/Dockerfile {key}: {current} -> {newest} ({age} days old)")

    mise_tools = mise_cfg.get("tools", {})
    if mise_tools:
        if "promtool" in mise_tools:
            raw_prom = mise_tools["promtool"]
            cur_prom = raw_prom if isinstance(raw_prom, str) else str(raw_prom.get("version", ""))
            if cur_prom:
                newest, age = _latest_release("github", "prometheus/prometheus", "3.5.")
                if newest != cur_prom and age >= COOLDOWN_DAYS:
                    findings.append(f"mise.toml [tools] promtool: {cur_prom} -> {newest} ({age} days old)")
        if shutil.which("mise") is None:
            raise lib.ScriptError("mise is required for --upstream tool pin checks", exit_code=2)
        env = dict(os.environ)
        if not env.get("MISE_GITHUB_TOKEN"):
            if tok := (env.get("GH_TOKEN") or env.get("GITHUB_TOKEN")):
                env["MISE_GITHUB_TOKEN"] = tok
        outdated = json.loads(lib.run(["mise", "outdated", "--bump", "--json"], cwd=root, env=env, capture=True).stdout or "{}")
        for tool, info in sorted(outdated.items()):
            if tool == "promtool" or not isinstance(info, dict):
                continue
            requested, bump = info.get("requested"), info.get("bump")
            if bump and requested and bump != requested:
                findings.append(f"mise.toml [tools] {tool}: {requested} -> {bump}")
    return findings


def check_drift(root: Path, *, online: bool, upstream: bool) -> list[str]:
    root = root.resolve()
    findings: list[str] = []
    workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))

    by_name: dict[str, list[dict[str, Any]]] = {}
    for pkg in lock.get("package", []):
        by_name.setdefault(pkg["name"], []).append(pkg)

    if lock.get("patch", {}).get("unused"):
        findings.append("Cargo.lock lists an unused [patch.crates-io] entry")

    codegen_manifest = root / "tools/grpc-codegen/Cargo.toml"
    if codegen_manifest.is_file():
        ws_deps = workspace.get("workspace", {}).get("dependencies", {})
        gen_deps = tomllib.loads(codegen_manifest.read_text(encoding="utf-8")).get("dependencies", {})
        for gen_name, ws_name in (("prost", "prost"), ("prost-types", "prost-types"), ("tonic-prost-build", "tonic-prost")):
            gen_entry, ws_entry = gen_deps.get(gen_name), ws_deps.get(ws_name)
            gen_ver = gen_entry.get("version") if isinstance(gen_entry, dict) else gen_entry
            ws_ver = ws_entry.get("version") if isinstance(ws_entry, dict) else ws_entry
            if ws_ver is not None and gen_ver != ws_ver:
                findings.append(f"tools/grpc-codegen/Cargo.toml pins {gen_name}={gen_ver}, Cargo.toml pins {ws_name}={ws_ver}")

    for key, patch in sorted(workspace.get("patch", {}).get("crates-io", {}).items()):
        rel_path = patch.get("path") if isinstance(patch, dict) else None
        if not isinstance(rel_path, str) or not rel_path:
            raise lib.ScriptError(f"[patch.crates-io] {key} must specify a local path", exit_code=2)
        directory = (root / rel_path).resolve()
        if not directory.is_relative_to(root.resolve()):
            raise lib.ScriptError(f"[patch.crates-io] {key} points outside repository: {rel_path}", exit_code=2)
        manifest, patches = directory / "Cargo.toml", directory / "PATCHES.md"
        for req_file in (manifest, patches):
            if not req_file.is_file():
                raise lib.ScriptError(f"[patch.crates-io] {key} requires {req_file.relative_to(root)}", exit_code=2)
        pkg_meta = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
        name, version = pkg_meta.get("name", ""), pkg_meta.get("version", "")
        if name != patch.get("package", key) or not VERSION_RE.fullmatch(version):
            raise lib.ScriptError(f"invalid patched package metadata in {manifest.relative_to(root)}", exit_code=2)

        locked = by_name.get(name, [])
        if not any(p["version"] == version and "source" not in p for p in locked):
            findings.append(f"{name}: Cargo.lock does not resolve vendored version {version}")
        if any("source" in p for p in locked):
            findings.append(f"{name}: Cargo.lock also resolves another source, bypassing the patch")

        reviewed_match = re.search(r"^Upstream reviewed:\s+([0-9][^\s]*)$", patches.read_text(encoding="utf-8"), re.M)
        reviewed = reviewed_match.group(1) if reviewed_match else ""
        if not reviewed:
            findings.append(f"{name}: {patches.relative_to(root)} has no `Upstream reviewed: <version>` line")

        if online or upstream:
            osv = _fetch_json(
                "https://api.osv.dev/v1/query",
                payload={"package": {"name": name, "ecosystem": "crates.io"}, "version": version},
            )
            vulns = osv.get("vulns", [])
            if vulns:
                findings.append(f"{name} {version}: fork base affected by {', '.join(v['id'] for v in vulns)}")

            versions = _crate_versions(name)
            matches = [v for v in versions if v.get("num") == version]
            if not matches:
                findings.append(f"{name} {version}: fork base version is absent on crates.io")
            elif matches[0].get("yanked"):
                findings.append(f"{name} {version}: fork base is yanked on crates.io")

            if upstream:
                stable = next((v for v in versions if not v.get("yanked") and "-" not in v.get("num", "")), None)
                if stable is None:
                    raise lib.ScriptError(f"no stable crates.io release for {name}", exit_code=2)
                newest, age = stable["num"], _release_age_days(stable["created_at"])
                if newest != version and newest != reviewed and age >= COOLDOWN_DAYS:
                    findings.append(
                        f"{name}: upstream {newest} ({age} days old) is unreviewed; fork base {version}, reviewed {reviewed or 'none'}"
                    )

    if upstream:
        findings.extend(_check_pin_drift(root))

    return findings


def _retained_packages(root: Path, triple: str) -> dict[str, set[str]]:
    meta = lib.cargo_metadata(root, no_deps=False, filter_platform=triple)
    lock_entries = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))["package"]
    locked = {(entry["name"], entry["version"]) for entry in lock_entries}
    resolved = {(pkg["name"], pkg["version"]) for pkg in meta["packages"]}
    if not (resolved <= locked):
        raise lib.ScriptError("Cargo metadata packages missing from Cargo.lock")
    by_name: dict[str, set[str]] = {}
    for name, version in resolved:
        by_name.setdefault(name, set()).add(version)
    return by_name


def run_transport(root: Path, target_triple: str | None, *, plan: bool) -> None:
    rustc = lib.run(["rustc", "-vV"], cwd=root, capture=True).stdout
    triple = target_triple or next(line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: "))
    retained = _retained_packages(root, triple)

    for target in TRANSPORT_TARGETS:
        manifest = root / "vendor" / target.package / "Cargo.toml"
        if not manifest.is_file():
            if target.package in retained and target.package in tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8")).get("patch", {}).get("crates-io", {}):
                raise lib.ScriptError(f"{target.package}: patched in Cargo.toml but missing {manifest}")
            continue
        pkg_meta = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
        vendored_ver = pkg_meta.get("version", "")
        if target.package not in retained:
            continue
        if vendored_ver not in retained[target.package]:
            raise lib.ScriptError(
                f"{target.package}: vendor manifest is {vendored_ver} but workspace resolved {sorted(retained[target.package])}"
            )

        target_dir = os.environ.get("CARGO_TARGET_DIR") or str(root / "target/vendor-transport")
        base = [
            "cargo", "test", "--locked", "--manifest-path", str(manifest),
            "--target-dir", target_dir,
            "--no-default-features", "--features", ",".join(target.features),
            "--target", triple, "--lib",
        ]
        cmd = [*base, "--", "--exact", "--test-threads=1", *target.filters]
        if plan:
            print(" ".join(cmd))
            continue

        listed = lib.run([*base, "--", "--list"], cwd=root, capture=True).stdout
        names = {line.removesuffix(": test") for line in listed.splitlines() if line.endswith(": test")}
        missing = sorted(set(target.filters) - names)
        if missing:
            raise lib.ScriptError(f"{target.package}: missing exact native tests {missing}")

        result = lib.run(cmd, cwd=root, capture=True, check=False)
        match = _RESULT_RE.search(result.stdout)
        counts = (int(match[1]), int(match[2]), int(match[3])) if match else None
        passed = {
            line.removeprefix("test ").removesuffix(" ... ok")
            for line in result.stdout.splitlines()
            if line.startswith("test ") and line.endswith(" ... ok")
        }
        if result.returncode or counts != (len(target.filters), 0, 0) or passed != set(target.filters):
            raise lib.ScriptError(
                f"{target.package}: expected {len(target.filters)} passing tests (counts={counts})\n"
                f"{result.stdout}\n{result.stderr}"
            )
        print(f"{target.package} ({vendored_ver}): {len(target.filters)} native transport regressions passed")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    drift = sub.add_parser("drift")
    drift.add_argument("--repo", type=Path, default=lib.ROOT)
    drift.add_argument("--online", action="store_true", help="query OSV and crates.io for advisories/yanks")
    drift.add_argument("--upstream", action="store_true", help="also check upstream releases behind PATCHES.md")
    transport = sub.add_parser("transport")
    transport.add_argument("--repo", type=Path, default=lib.ROOT)
    transport.add_argument("--target", default=os.environ.get("CARGO_BUILD_TARGET") or os.environ.get("TARGET"))
    transport.add_argument("--plan", action="store_true")

    args = parser.parse_args(argv)

    def _dispatch() -> int:
        if args.command == "drift":
            findings = check_drift(args.repo.resolve(), online=args.online, upstream=args.upstream)
            if findings:
                for item in findings:
                    print(f"- {item}")
                return 1
            print("vendor-drift: no finding")
            return 0
        run_transport(args.repo.resolve(), args.target, plan=args.plan)
        return 0

    return lib.run_cli(_dispatch)


if __name__ == "__main__":
    raise SystemExit(main())
