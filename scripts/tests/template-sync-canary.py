#!/usr/bin/env python3
"""Committed-source sync composition canary with local-byte preservation oracles."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import tempfile
from pathlib import Path


_LEGACY_B206_PROFILE_SHA256 = "75e68f9c7defd4031f5d7a0bc2866f337a6c79f69a69f56c79a268d48d8d6530"
_LEGACY_B206_REVISION = "b2060279370713f05be81b1ad44a31e3e960bccc"
_LEGACY_PROFILE_KEYS = ("schema_version", "source_only", "postgres", "identity", "cargo_lock")
_AUTH_ONLY_PROFILE_KEYS = (
    "schema_version", "source_only", "postgres", "authn", "oidc-jwt", "oidc-introspection", "identity", "cargo_lock",
)
_OUTBOUND_ONLY_PROFILE_KEYS = (
    "schema_version", "source_only", "postgres", "authn", "oidc-jwt", "oidc-introspection",
    "outbound-http", "egress-dns", "tls-fixtures", "request-budget", "identity", "cargo_lock",
)
_NEW_PROJECTION_CHECKER = "scripts/tests/template-profile-projections.py"


def run(args: list[str], *, cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def commit(repo: Path, message: str) -> None:
    for args in (["git", "config", "user.email", "template-sync-canary@example.invalid"], ["git", "config", "user.name", "template-sync-canary"], ["git", "add", "-A"], ["git", "commit", "-qm", message]):
        result = run(list(args), cwd=repo)
        if result.returncode:
            raise AssertionError(result.stderr)


def commit_paths(repo: Path, message: str, *paths: str) -> None:
    for args in (["git", "add", "--", *paths], ["git", "-c", "user.email=template-sync-canary@example.invalid", "-c", "user.name=template-sync-canary", "commit", "-qm", message, "--", *paths]):
        result = run(list(args), cwd=repo)
        if result.returncode:
            raise AssertionError(result.stderr)


def sync(source: Path, target: Path, mode: str, instructions_only: bool = False, *, runner: Path | None = None) -> subprocess.CompletedProcess[str]:
    args = ["bash", os.fspath((runner or source) / "scripts/template-sync.sh"), mode, "--from", os.fspath(source), "--repo", os.fspath(target)]
    if instructions_only:
        args.append("--instructions-only")
    return run(args, cwd=source)


def tree_state(root: Path) -> str:
    records: list[str] = []
    for path in sorted(root.rglob("*")):
        if ".git" in path.parts:
            continue
        relative = path.relative_to(root).as_posix()
        mode = path.lstat().st_mode
        if path.is_symlink():
            payload = os.readlink(path).encode("utf-8")
        elif path.is_file():
            payload = path.read_bytes()
        else:
            records.append(f"dir {relative} {mode:o}")
            continue
        records.append(f"entry {relative} {mode:o} {hashlib.sha256(payload).hexdigest()}")
    status = run(["git", "status", "--porcelain=v1", "--ignored"], cwd=root)
    records.append(status.stdout)
    return "\n".join(records)


def clone(source: Path, destination: Path) -> None:
    cloned = run(["git", "clone", "--quiet", "--no-local", os.fspath(source), os.fspath(destination)], cwd=source)
    if cloned.returncode:
        raise AssertionError(cloned.stderr)


def install_historical_none(source: Path, target: Path) -> None:
    profile = json.loads((source / "scripts/lib/template_profiles.json").read_text(encoding="utf-8"))
    legacy = {key: profile[key] for key in _LEGACY_PROFILE_KEYS}
    source_only = legacy["source_only"]
    if not isinstance(source_only, list) or source_only.count(_NEW_PROJECTION_CHECKER) != 1:
        raise AssertionError("current profile inventory lacks exactly one projection checker entry")
    legacy["source_only"] = [item for item in source_only if item != _NEW_PROJECTION_CHECKER]
    # The shared PostgreSQL proxy did not exist in the pinned b206 inventory.
    legacy["postgres"]["remove_when_none"].remove("test/tests/support/commit_proxy.rs")
    rendered = (json.dumps(legacy, indent=2) + "\n").encode("utf-8")
    if hashlib.sha256(rendered).hexdigest() != _LEGACY_B206_PROFILE_SHA256:
        raise AssertionError("legacy b206 profile inventory bytes changed")
    (target / "scripts/lib/template_profiles.json").write_bytes(rendered)
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    lock["profiles"].pop("authn")
    lock["profiles"].pop("outbound_http", None)
    lock["profiles"].pop("http_idempotency", None)
    lock["profiles"].pop("jobs", None)
    lock["profiles"].pop("webhooks", None)
    lock["profiles"].pop("inbound_webhooks", None)
    lock["source"]["checkout_revision"] = _LEGACY_B206_REVISION
    (target / "template.lock").write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")


def install_derived_auth_only_none(source: Path, target: Path) -> None:
    profile = json.loads((source / "scripts/lib/template_profiles.json").read_text(encoding="utf-8"))
    auth_only = {key: profile[key] for key in _AUTH_ONLY_PROFILE_KEYS}
    (target / "scripts/lib/template_profiles.json").write_text(
        json.dumps(auth_only, indent=2) + "\n", encoding="utf-8"
    )
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    lock["profiles"].pop("outbound_http", None)
    lock["profiles"].pop("http_idempotency", None)
    lock["profiles"].pop("jobs", None)
    lock["profiles"].pop("webhooks", None)
    lock["profiles"].pop("inbound_webhooks", None)
    (target / "template.lock").write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")


def install_derived_outbound_only_none(source: Path, target: Path) -> None:
    profile = json.loads((source / "scripts/lib/template_profiles.json").read_text(encoding="utf-8"))
    outbound_only = {key: profile[key] for key in _OUTBOUND_ONLY_PROFILE_KEYS}
    (target / "scripts/lib/template_profiles.json").write_text(
        json.dumps(outbound_only, indent=2) + "\n", encoding="utf-8"
    )
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    # The outbound generation's four-field shape, whatever an earlier
    # fixture left in the lock: a missing selection there means `none`.
    profiles = lock["profiles"]
    lock["profiles"] = {
        "database": profiles["database"],
        "authn": profiles.get("authn", "none"),
        "outbound_http": profiles.get("outbound_http", "none"),
        "agent_harness": profiles["agent_harness"],
    }
    (target / "template.lock").write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")


def assert_profile_pack(source: Path, target: Path, profile_name: str, selected: bool) -> None:
    profile = json.loads((source / "scripts/lib/template_profiles.json").read_text(encoding="utf-8"))
    key = "remove_when_none" if profile_name == "postgres" else "remove_when_unselected"
    for relative in profile[profile_name][key]:
        path = target / relative.rstrip("/")
        if not selected:
            if path.exists() or path.is_symlink():
                raise AssertionError(f"unselected {profile_name} output remains {relative}")
            continue
        if relative.endswith("/"):
            if path.is_symlink() or not path.is_dir():
                raise AssertionError(f"selected {profile_name} directory is missing {relative}")
        elif path.is_symlink() or not path.is_file():
            raise AssertionError(f"selected {profile_name} file is missing {relative}")


def assert_profile_output(
    source: Path, target: Path, authn: str, outbound_http: str, http_idempotency: str = "none",
    jobs: str = "none", webhooks: str = "none", inbound_webhooks: str = "none",
) -> None:
    for profile, selected in (("authn", authn != "none"), ("oidc-jwt", authn == "oidc-jwt"), ("oidc-introspection", authn == "oidc-introspection")):
        assert_profile_pack(source, target, profile, selected)
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    if lock["profiles"].get("authn") != authn:
        raise AssertionError(f"sync canary lock did not record authn={authn}")
    if lock["profiles"].get("outbound_http") != outbound_http:
        raise AssertionError(f"sync canary lock did not record outbound_http={outbound_http}")
    if lock["profiles"].get("http_idempotency") != http_idempotency:
        raise AssertionError(f"sync canary lock did not record http_idempotency={http_idempotency}")
    if lock["profiles"].get("jobs") != jobs:
        raise AssertionError(f"sync canary lock did not record jobs={jobs}")
    if lock["profiles"].get("webhooks") != webhooks:
        raise AssertionError(f"sync canary lock did not record webhooks={webhooks}")
    if lock["profiles"].get("inbound_webhooks") != inbound_webhooks:
        raise AssertionError(f"sync canary lock did not record inbound_webhooks={inbound_webhooks}")
    assert_profile_pack(source, target, "outbound-http", outbound_http == "bounded")
    shared_selected = authn != "none" or outbound_http == "bounded"
    assert_profile_pack(source, target, "tls-fixtures", shared_selected)
    assert_profile_pack(source, target, "egress-dns", outbound_http == "bounded")
    assert_profile_pack(source, target, "request-budget", shared_selected)
    assert_profile_pack(source, target, "http-idempotency", http_idempotency == "postgres")
    assert_profile_pack(
        source, target, "http-idempotency-mounted", http_idempotency == "postgres" and authn == "oidc-introspection"
    )
    assert_profile_pack(source, target, "jobs", jobs == "postgres")
    assert_profile_pack(
        source, target, "jobs-http-idempotency", jobs == "postgres" and http_idempotency == "postgres"
    )
    assert_profile_pack(source, target, "webhooks-common", webhooks == "durable" or inbound_webhooks == "standard-webhooks")
    assert_profile_pack(source, target, "webhooks", webhooks == "durable")
    assert_profile_pack(source, target, "inbound-webhooks", inbound_webhooks == "standard-webhooks")


def initialize(
    source: Path, target: Path, authn: str | None, outbound_http: str | None = None,
    http_idempotency: str | None = None,
    *,
    database: str = "none",
    jobs: str | None = None,
    webhooks: str | None = None,
    inbound_webhooks: str | None = None,
) -> subprocess.CompletedProcess[str]:
    command = [
        "bash", os.fspath(source / "scripts/init-module.sh"), "--repo", os.fspath(target),
        "--service-name", "canary-api", "--repository", "https://github.com/example/canary-api",
        "--description", "Canary API", "--codeowner", "@example/platform", "--database", database,
        "--agent-harness", "claude",
    ]
    if authn is not None:
        command.extend(("--authn", authn))
    if outbound_http is not None:
        command.extend(("--outbound-http", outbound_http))
    if http_idempotency is not None:
        command.extend(("--http-idempotency", http_idempotency))
    if jobs is not None:
        command.extend(("--jobs", jobs))
    if webhooks is not None:
        command.extend(("--webhooks", webhooks))
    if inbound_webhooks is not None:
        command.extend(("--inbound-webhooks", inbound_webhooks))
    return run(command, cwd=source)


def check(source: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="template-sync-canary-") as temp:
        work = Path(temp)
        target = work / "target"
        clone(source, target)
        initialized = initialize(source, target, "oidc-jwt", "bounded")
        if initialized.returncode:
            raise AssertionError(initialized.stderr)
        assert_profile_output(source, target, "oidc-jwt", "bounded")
        webhook_target = work / "webhooks"
        clone(source, webhook_target)
        webhook_initialized = initialize(
            source, webhook_target, "none", "bounded", database="postgres", jobs="postgres",
            webhooks="durable", inbound_webhooks="standard-webhooks",
        )
        if webhook_initialized.returncode:
            raise AssertionError(webhook_initialized.stderr)
        assert_profile_output(
            source, webhook_target, "none", "bounded", jobs="postgres",
            webhooks="durable", inbound_webhooks="standard-webhooks",
        )
        # Initialization removes unselected adapter carriers. Commit that
        # derived baseline before sync admission, as the existing jobs-none
        # canary does, so the sync check exercises parity rather than its
        # production dirty-selected-target refusal.
        commit(webhook_target, "initialize webhook profiles")
        webhook_sync = sync(source, webhook_target, "--check")
        if webhook_sync.returncode:
            raise AssertionError(f"webhook profile sync parity failed: {webhook_sync.stderr}")
        for authn in ("oidc-introspection", None):
            profile_target = work / f"profile-{authn or 'historical-none'}"
            clone(source, profile_target)
            profile_initialized = initialize(source, profile_target, authn)
            if profile_initialized.returncode:
                raise AssertionError(profile_initialized.stderr)
            expected = authn or "none"
            assert_profile_output(source, profile_target, expected, "none")
            if authn is None:
                install_derived_auth_only_none(source, profile_target)
                auth_only_before = tree_state(profile_target)
                auth_only_replay = initialize(source, profile_target, "none", "none")
                if auth_only_replay.returncode or tree_state(profile_target) != auth_only_before:
                    raise AssertionError("derived auth-only none lock replay changed the sync canary target")
                auth_only_mismatch = initialize(source, profile_target, "none", "bounded")
                if auth_only_mismatch.returncode == 0 or tree_state(profile_target) != auth_only_before:
                    raise AssertionError("auth-only none sync canary accepted an outbound profile migration")
                install_derived_outbound_only_none(source, profile_target)
                outbound_only_before = tree_state(profile_target)
                outbound_only_replay = initialize(source, profile_target, "none", "none", "none")
                if outbound_only_replay.returncode or tree_state(profile_target) != outbound_only_before:
                    raise AssertionError("derived outbound-only none lock replay changed the sync canary target")
                outbound_only_mismatch = initialize(source, profile_target, "none", "none", "postgres")
                if outbound_only_mismatch.returncode == 0 or tree_state(profile_target) != outbound_only_before:
                    raise AssertionError("outbound-only none sync canary accepted an http idempotency profile migration")
                install_historical_none(source, profile_target)
                before = tree_state(profile_target)
                replay = initialize(source, profile_target, "none")
                if replay.returncode or tree_state(profile_target) != before:
                    raise AssertionError("actual historical none lock replay changed the sync canary target")
                mismatch = initialize(source, profile_target, "oidc-jwt")
                if mismatch.returncode == 0 or tree_state(profile_target) != before:
                    raise AssertionError("historical none sync canary accepted an authn profile migration")
        # A DATABASE=postgres JOBS=none service keeps its migrations, and a
        # full portable sync from a source that retains the jobs pack restores
        # none of it.
        jobs_none = work / "postgres-jobs-none"
        clone(source, jobs_none)
        jobs_none_initialized = initialize(source, jobs_none, None, database="postgres", jobs="none")
        if jobs_none_initialized.returncode:
            raise AssertionError(jobs_none_initialized.stderr)
        assert_profile_output(source, jobs_none, "none", "none", jobs="none")
        commit(jobs_none, "initialize postgres without jobs")
        jobs_owned = jobs_none / "scripts/template-sync.sh"
        jobs_owned.write_bytes(jobs_owned.read_bytes() + b"\n# committed full-sync drift\n")
        commit_paths(jobs_none, "committed full sync drift", "scripts/template-sync.sh")
        jobs_apply = sync(source, jobs_none, "--apply")
        if jobs_apply.returncode:
            raise AssertionError(f"postgres jobs-none full sync apply failed: {jobs_apply.stderr}")
        if not (jobs_none / "migrations").is_dir():
            raise AssertionError("postgres jobs-none sync removed migrations/")
        for restored in (
            "crates/infra-jobs",
            "migrations/20260924000001_create_background_jobs.sql",
            "crates/config/src/jobs.rs",
            "docs/background-jobs.md",
        ):
            if (jobs_none / restored).exists() or (jobs_none / restored).is_symlink():
                raise AssertionError(f"postgres jobs-none sync restored {restored}")
        assert_profile_pack(source, jobs_none, "jobs", False)
        assert_profile_pack(source, jobs_none, "jobs-http-idempotency", False)
        skill = target / ".agents/skills/local-service-skill"
        skill.mkdir(parents=True)
        (skill / ".service-owned").write_text("", encoding="utf-8")
        (skill / "SKILL.md").write_text("---\nname: local-service-skill\ndescription: local\n---\n", encoding="utf-8")
        settings = target / ".claude/settings.json"
        settings_before = digest(settings)
        service_doc = target / "docs/repo-architecture.md"
        service_doc.write_text(service_doc.read_text(encoding="utf-8") + "\nLocal canary architecture fact.\n", encoding="utf-8")
        commit(target, "local service facts")

        portable = target / "AGENTS.md"
        portable.write_text(portable.read_text(encoding="utf-8") + "\ncanary instruction drift\n", encoding="utf-8")
        tooling = target / "make/template.mk"
        tooling_before = tooling.read_bytes()
        tooling.write_bytes(tooling.read_bytes() + b"\n# local tooling dirt\n")
        # Commit only selected instruction drift. The unrelated-to-instruction
        # portable tooling remains dirty throughout instructions-only adoption.
        commit_paths(target, "committed portable instruction drift", "AGENTS.md")
        check_drift = sync(source, target, "--check", instructions_only=True)
        if check_drift.returncode != 1:
            raise AssertionError(f"instructions-only check should report drift, got {check_drift.returncode}: {check_drift.stderr}")
        applied = sync(source, target, "--apply", instructions_only=True)
        if applied.returncode:
            raise AssertionError(f"instructions-only apply failed: {applied.stderr}")
        if tooling.read_bytes() != tooling_before + b"\n# local tooling dirt\n" or "Local canary architecture fact." not in service_doc.read_text(encoding="utf-8"):
            raise AssertionError("instructions-only sync changed service tooling or local architecture")
        if not (skill / ".service-owned").is_file() or digest(settings) != settings_before:
            raise AssertionError("instructions-only sync failed to preserve marked skill or settings bytes")
        commit_paths(target, "adopt portable instructions", "AGENTS.md")
        parity = sync(source, target, "--check", instructions_only=True)
        if parity.returncode:
            raise AssertionError(f"instructions-only parity with dirty tooling failed: {parity.stderr}")

        bytes_before = tooling.read_bytes()
        full_refusal = sync(source, target, "--check")
        if full_refusal.returncode != 2 or tooling.read_bytes() != bytes_before:
            raise AssertionError("full sync did not preserve dirty portable tooling refusal")

        tooling.write_bytes(tooling_before)
        if tooling.read_bytes() != tooling_before:
            raise AssertionError("canary could not restore the exact portable tooling baseline")

        full_owned = target / "scripts/template-sync.sh"
        full_owned.write_bytes(full_owned.read_bytes() + b"\n# committed full-sync drift\n")
        commit_paths(target, "committed full sync drift", "scripts/template-sync.sh")
        full_drift = sync(source, target, "--check")
        if full_drift.returncode != 1:
            raise AssertionError(f"full sync check should report committed drift, got {full_drift.returncode}: {full_drift.stderr}")
        full_apply = sync(source, target, "--apply")
        if full_apply.returncode:
            raise AssertionError(f"full sync apply failed: {full_apply.stderr}")
        if "Local canary architecture fact." not in service_doc.read_text(encoding="utf-8") or not (skill / ".service-owned").is_file():
            raise AssertionError("full sync changed local authority or marked service skill")
        # `target` never selected HTTP_IDEMPOTENCY=postgres; a full sync from a
        # source that retains the pack must not restore it.
        assert_profile_pack(source, target, "http-idempotency", False)
        assert_profile_pack(source, target, "http-idempotency-mounted", False)
        commit(target, "adopt full portable snapshot")
        full_parity = sync(source, target, "--check")
        if full_parity.returncode:
            raise AssertionError(f"committed full parity failed: {full_parity.stderr}")

        crlf_settings = (
            b"{\r\n"
            b"  \"unrelated\": { \"keep\": true },\r\n"
            b"  \"env\": {\r\n"
            b"    \"CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH\": 3\r\n"
            b"  }\r\n"
            b"}\r\n"
        )
        settings.write_bytes(crlf_settings)
        commit_paths(target, "managed setting type drift", ".claude/settings.json")
        setting_drift = sync(source, target, "--check", instructions_only=True)
        if setting_drift.returncode != 1:
            raise AssertionError(f"managed setting type drift should be reported, got {setting_drift.returncode}: {setting_drift.stderr}")
        setting_apply = sync(source, target, "--apply", instructions_only=True)
        if setting_apply.returncode:
            raise AssertionError(f"managed setting type repair failed: {setting_apply.stderr}")
        rendered_settings = settings.read_bytes()
        if b"\r\n  \"unrelated\": { \"keep\": true },\r\n" not in rendered_settings or b'\"CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH\": \"3\"' not in rendered_settings:
            raise AssertionError("settings repair changed unrelated CRLF bytes or did not replace the managed token")
        commit_paths(target, "adopt lexical managed setting", ".claude/settings.json")

        make_cases = (
            ("space-target", b"\n build: ; @:\n", "overrides"),
            ("paren-eval", b"\nOVERRIDE := $(eval build: ; @:)\n", "evaluation"),
            ("brace-eval", b"\nOVERRIDE := ${eval build: ; @:}\n", "evaluation"),
            ("computed-target", b"\nTARGET := build\n$(TARGET): ; @:\n", "nonstatic target"),
        )
        for label, payload, reason in make_cases:
            make_target = work / f"make-{label}"
            clone(target, make_target)
            service_make = make_target / "make/service.mk"
            service_make.write_bytes(service_make.read_bytes() + payload)
            commit_paths(make_target, label, "make/service.mk")
            make_before = tree_state(make_target)
            make_refusal = sync(source, make_target, "--check")
            if make_refusal.returncode != 2 or reason not in make_refusal.stderr or tree_state(make_target) != make_before:
                raise AssertionError(f"Make {label} did not refuse for its expected reason before changing the target")

        nested_source = target / ".canary-nested-source"
        nested_source.mkdir()
        for args in (["git", "init", "-q"], ["git", "config", "user.email", "template-sync-canary@example.invalid"], ["git", "config", "user.name", "template-sync-canary"]):
            result = run(list(args), cwd=nested_source)
            if result.returncode:
                raise AssertionError(result.stderr)
        (nested_source / "README.md").write_text("nested source\n", encoding="utf-8")
        commit(nested_source, "nested source")
        nested_before = tree_state(target)
        nested_refusal = sync(nested_source, target, "--check", runner=source)
        if nested_refusal.returncode != 2 or tree_state(target) != nested_before:
            raise AssertionError("nested source/target refusal did not preserve the target")
    print("template sync canary: pass")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", required=True, type=Path)
    args = parser.parse_args()
    check(args.source.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
