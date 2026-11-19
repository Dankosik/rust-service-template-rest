#!/usr/bin/env python3

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path


sys.dont_write_bytecode = True

PROFILE_SELF_TEST = "scripts/tests/test_profile_runners.py"
SYNC_SCRIPT = "scripts/template/sync.py"
MANIFEST = "template-owned.paths"


def _clean_env() -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if k not in {"PYTHONPATH", "PYTHONHOME"}}
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    return env


def run(args: list[str], *, cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, text=True, capture_output=True, check=False, env=_clean_env())


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
    cwd = target if runner == target else source
    args = [
        sys.executable, "-B", "-E",
        os.fspath((runner or source) / SYNC_SCRIPT),
        mode, "--from", os.fspath(source), "--repo", "." if cwd == target else os.fspath(target),
    ]
    if instructions_only:
        args.append("--instructions-only")
    return run(args, cwd=cwd)


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


def assert_profile_pack(source: Path, target: Path, profile_name: str, selected: bool) -> None:
    profile = json.loads((source / "scripts/template/profiles.json").read_text(encoding="utf-8"))
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


def assert_tls_fixture_output(source: Path, target: Path) -> None:
    manifest = tomllib.loads((target / "Cargo.toml").read_text(encoding="utf-8"))
    shared_selected = "tls-test-support" in manifest["workspace"]["dependencies"]
    assert_profile_pack(source, target, "tls-fixtures", shared_selected)


def assert_profile_output(
    source: Path, target: Path, authn: str, outbound_http: str,
    jobs: str = "none", webhooks: str = "none", inbound_webhooks: str = "none",
) -> None:
    for profile, selected in (("authn", authn != "none"), ("oidc-jwt", authn == "oidc-jwt"), ("oidc-introspection", False)):
        assert_profile_pack(source, target, profile, selected)
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    profiles = lock["profiles"]
    if profiles.get("authn") != authn:
        raise AssertionError(f"sync canary lock did not record authn={authn}")
    if profiles.get("outbound_http") != outbound_http:
        raise AssertionError(f"sync canary lock did not record outbound_http={outbound_http}")
    if profiles.get("http_idempotency") != "none":
        raise AssertionError("sync canary lock did not record http_idempotency=none")
    if profiles.get("jobs") != jobs:
        raise AssertionError(f"sync canary lock did not record jobs={jobs}")
    if profiles.get("webhooks") != webhooks:
        raise AssertionError(f"sync canary lock did not record webhooks={webhooks}")
    if profiles.get("inbound_webhooks") != inbound_webhooks:
        raise AssertionError(f"sync canary lock did not record inbound_webhooks={inbound_webhooks}")
    assert_profile_pack(source, target, "outbound-http", outbound_http == "bounded")
    assert_tls_fixture_output(source, target)
    assert_profile_pack(source, target, "cache", False)
    assert_profile_pack(source, target, "object-storage", False)
    has_compose_integration = (
        profiles.get("database") == "postgres"
        or profiles.get("messaging") == "nats-jetstream"
    )
    assert_profile_pack(source, target, "integration", has_compose_integration)
    assert_profile_pack(
        source, target, "container-integration",
        has_compose_integration or profiles.get("outbound_auth") == "oauth2-client-credentials",
    )
    assert_profile_pack(
        source, target, "request-budget",
        outbound_http == "bounded" or inbound_webhooks == "standard-webhooks",
    )
    assert_profile_pack(source, target, "http-idempotency", False)
    assert_profile_pack(source, target, "http-idempotency-mounted", False)
    assert_profile_pack(source, target, "jobs", jobs == "postgres")
    assert_profile_pack(source, target, "jobs-http-idempotency", False)
    assert_profile_pack(source, target, "webhooks-common", webhooks == "durable" or inbound_webhooks == "standard-webhooks")
    assert_profile_pack(source, target, "webhooks", webhooks == "durable")
    assert_profile_pack(source, target, "inbound-webhooks", inbound_webhooks == "standard-webhooks")
    assert_profile_pack(source, target, "grpc", False)
    # Service tokens need the gRPC listener, which this canary never retains.
    assert_profile_pack(source, target, "service-auth", False)
    if "grpc-check:" in (target / "make/template.mk").read_text(encoding="utf-8"):
        raise AssertionError("portable sync restored unselected gRPC Make targets")


def initialize(
    source: Path, target: Path, authn: str | None, outbound_http: str | None = None,
    *,
    database: str = "none",
    agent_harness: str = "claude",
    outbound_auth: str | None = None,
    jobs: str | None = None,
    webhooks: str | None = None,
    inbound_webhooks: str | None = None,
) -> subprocess.CompletedProcess[str]:
    command = [
        sys.executable, "-B", "-E", os.fspath(source / "scripts/template/init.py"), "--repo", os.fspath(target),
        "--service-name", "canary-api", "--repository", "https://github.com/example/canary-api",
        "--description", "Canary API", "--codeowner", "@example/platform", "--database", database,
        "--agent-harness", agent_harness,
    ]
    if authn is not None:
        command.extend(("--authn", authn))
    if outbound_http is not None:
        command.extend(("--outbound-http", outbound_http))
    if outbound_auth is not None:
        command.extend(("--outbound-auth", outbound_auth))
    if jobs is not None:
        command.extend(("--jobs", jobs))
    if webhooks is not None:
        command.extend(("--webhooks", webhooks))
    if inbound_webhooks is not None:
        command.extend(("--inbound-webhooks", inbound_webhooks))
    return run(command, cwd=source)


def require_result(result: subprocess.CompletedProcess[str], code: int | None, message: str = "") -> str:
    output = result.stdout + result.stderr
    expected = result.returncode != 0 if code is None else result.returncode == code
    if not expected or message not in output:
        raise AssertionError(f"{result.args}: expected code {code} and {message!r}, got {result.returncode}\n{output}")
    return result.stdout


def git(repo: Path, *args: str) -> str:
    return require_result(run(["git", *args], cwd=repo), 0)


def make_sync(source: Path, target: Path, mode: str, *, instructions_only: bool = False) -> subprocess.CompletedProcess[str]:
    return run([
        "make", "template-sync", f"TEMPLATE_SOURCE={source}", f"SYNC_MODE={mode}",
        f"SYNC_FLAGS={'--instructions-only' if instructions_only else ''}",
    ], cwd=target)


def changed_paths(target: Path) -> set[str]:
    tracked = git(target, "diff", "--no-renames", "--name-only", "-z", "HEAD").split("\0")
    untracked = git(target, "ls-files", "--others", "--exclude-standard", "-z").split("\0")
    return set(filter(None, tracked + untracked))


def assert_profile_self_test(target: Path, *, delivered: bool) -> None:
    test_path = target / PROFILE_SELF_TEST
    test_ok = test_path.is_file() if delivered else not test_path.exists()
    owned = PROFILE_SELF_TEST in (target / MANIFEST).read_text(encoding="utf-8").splitlines()
    if test_path.is_symlink() or not test_ok or owned != delivered:
        raise AssertionError(f"{target.name}: expected {PROFILE_SELF_TEST} delivery={delivered} in the worktree and manifest")


def assert_unchanged(target: Path, expected: dict[str, str]) -> None:
    for relative, checksum in expected.items():
        path = target / relative
        if path.is_symlink() or not path.is_file() or digest(path) != checksum:
            raise AssertionError(f"{target.name}: sync changed protected file {relative}")


def prepare_service_extensions(target: Path, *, custom_ci: bool) -> dict[str, str]:
    ci_mode = "github-actions" if custom_ci else "none"
    service_make = target / "make/service.mk"
    service_make.write_text(
        service_make.read_text(encoding="utf-8")
        + f"\nREPOSITORY_CI := {ci_mode}\ncanary-local-ci:\n\t@printf '%s\\n' '$(REPOSITORY_CI)'\n",
        encoding="utf-8",
    )
    extensions = {
        ".railway/canary.toml": 'profile = "canary"\n',
        "scripts/service/canary.sh": "#!/bin/sh\nexit 0\n",
        ".agents/skills/canary-local/.service-owned": "",
        ".agents/skills/canary-local/SKILL.md": "---\nname: canary-local\ndescription: Use when testing local service-owned skills.\n---\n",
    }
    if custom_ci:
        extensions[".github/workflows/canary-local.yml"] = (
            "name: Local workflow\non: [push]\njobs:\n  verify:\n    runs-on: ubuntu-latest\n"
            "    steps:\n      - uses: actions/checkout@v4\n      - run: make verify-check\n"
        )
        integration = target / "scripts/profiles/integration.sh"
        extensions["scripts/profiles/integration.sh"] = integration.read_text(encoding="utf-8") + "\n# local CI profile\n"
    for relative, contents in extensions.items():
        path = target / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")

    settings_path = ".claude/settings.json"
    settings = json.loads((target / settings_path).read_text(encoding="utf-8"))
    settings["env"]["CANARY_LOCAL_SETTING"] = "keep"
    (target / settings_path).write_text(json.dumps(settings, indent=2) + "\n", encoding="utf-8")
    return {path: digest(target / path) for path in (*extensions, "make/service.mk", "template.lock", settings_path)}


def adopt_from_previous(source: Path, target: Path, previous: Path, *, custom_ci: bool) -> dict[str, str]:
    label = target.name
    protected = prepare_service_extensions(target, custom_ci=custom_ci)
    if digest(target / SYNC_SCRIPT) != digest(previous / SYNC_SCRIPT):
        raise AssertionError(f"{label}: expected the previous revision's installed sync client")
    assert_profile_self_test(target, delivered=True)
    instructions = target / "AGENTS.md"
    instructions.write_bytes(instructions.read_bytes() + b"\nCommitted legacy canary instructions drift.\n")
    commit(target, "retain service-local extensions and create instruction drift")

    owned = "scripts/verify/runner.py"
    tooling = target / owned
    local_drift = b"\n# legacy canary drift\n"
    tooling.write_bytes(tooling.read_bytes() + local_drift)
    non_instruction_paths = {path: digest(target / path) for path in (*protected, MANIFEST, PROFILE_SELF_TEST, SYNC_SCRIPT, owned)}
    require_result(make_sync(source, target, "--check", instructions_only=True), None, "template sync: drift AGENTS.md")
    require_result(make_sync(source, target, "--apply", instructions_only=True), 0, "mode=instructions-only applied")
    assert_unchanged(target, non_instruction_paths)
    commit_paths(target, "adopt instructions only", *sorted(changed_paths(target) - {owned}))
    require_result(make_sync(source, target, "--check", instructions_only=True), 0, "mode=instructions-only parity")

    before = tree_state(target)
    require_result(sync(source, target, "--check", runner=target), 2, f"dirty selected target path: {owned}")
    if tree_state(target) != before:
        raise AssertionError(f"{label}: dirty-target refusal changed the worktree")
    commit_paths(target, "commit legacy portable drift", owned)
    head = git(target, "rev-parse", "HEAD").strip()
    before = tree_state(target)
    drift = require_result(sync(source, target, "--check", runner=target), 1, f"template sync: drift {owned}")
    require_result(make_sync(source, target, "--check"), None, f"template sync: drift {owned}")
    planned = {line.removeprefix("template sync: drift ") for line in drift.splitlines()}
    if tree_state(target) != before:
        raise AssertionError(f"{label}: check for committed drift changed the worktree")

    if not custom_ci:
        if PROFILE_SELF_TEST not in planned:
            raise AssertionError(f"{label}: the retired profile self-test was absent from the announced plan")
        excludes = Path(git(target, "rev-parse", "--path-format=absolute", "--git-path", "info/exclude").strip())
        with excludes.open("ab") as stream:
            excludes_size = stream.tell()
            stream.write(f"\n/{PROFILE_SELF_TEST}\n".encode())
        require_result(sync(source, target, "--apply", runner=target), 2, f"ignored destination overlaps selected sync path: {PROFILE_SELF_TEST}")
        with excludes.open("r+b") as stream:
            stream.truncate(excludes_size)
        if tree_state(target) != before:
            raise AssertionError(f"{label}: ignored collision changed the worktree")

    require_result(make_sync(source, target, "--apply"), 0, "mode=full applied")
    changed = changed_paths(target)
    if owned not in changed or changed - planned or git(target, "diff", "--cached", "--name-only"):
        raise AssertionError(f"{label}: full apply did not leave only the announced paths uncommitted")
    if git(target, "rev-parse", "HEAD").strip() != head or local_drift in tooling.read_bytes():
        raise AssertionError(f"{label}: full sync failed to replace {owned} while leaving HEAD unchanged")
    assert_profile_self_test(target, delivered=custom_ci)
    assert_profile_output(source, target, "none", "bounded" if custom_ci else "none")
    assert_unchanged(target, protected)
    require_result(run(["make", "canary-local-ci"], cwd=target), 0, "github-actions\n" if custom_ci else "none\n")
    commit(target, "adopt portable tooling from the new source")
    if digest(target / SYNC_SCRIPT) != digest(source / SYNC_SCRIPT):
        raise AssertionError(f"{label}: the installed client differs from the new source")
    require_result(make_sync(source, target, "--check"), 0, "mode=full parity")
    require_result(sync(source, target, "--check", runner=target), 0, "mode=full parity")
    print(f"template sync canary: {label} previous client and installed parity pass")
    return protected


def check_previous(source: Path, previous_revision: str) -> None:
    revision = git(source, "rev-parse", "--verify", "--end-of-options", f"{previous_revision}^{{commit}}").strip()
    with tempfile.TemporaryDirectory(prefix="template-sync-previous-") as temporary:
        work = Path(temporary)
        previous = work / "previous"
        clone(source, previous)
        git(previous, "checkout", "--detach", revision)

        minimal = work / "no-ci"
        clone(previous, minimal)
        require_result(initialize(previous, minimal, None, agent_harness="all"), 0)
        commit(minimal, "initialize the previous minimal installation")
        compatible = work / "compatible-initialized-source"
        clone(minimal, compatible)
        markerless = work / "previous-markerless-source"
        clone(compatible, markerless)
        git(markerless, "rm", "--", "template.lock")
        commit(markerless, "freeze the previous markerless portable layout")

        custom = work / "custom-ci"
        clone(previous, custom)
        require_result(initialize(previous, custom, None, outbound_auth="oauth2-client-credentials"), 0)
        adopt_from_previous(source, custom, previous, custom_ci=True)
        protected = adopt_from_previous(source, minimal, previous, custom_ci=False)

        before = tree_state(minimal)
        require_result(sync(custom, minimal, "--apply", runner=minimal), 2, "initialized sync source has different capability profiles")
        if tree_state(minimal) != before:
            raise AssertionError("initialized source with different capabilities changed the target")

        for old_source in (markerless, compatible):
            require_result(sync(old_source, minimal, "--apply", runner=minimal), 0, "mode=full applied")
            assert_profile_self_test(minimal, delivered=True)
            if digest(minimal / PROFILE_SELF_TEST) != digest(old_source / PROFILE_SELF_TEST):
                raise AssertionError(f"{old_source.name}: full sync failed to restore the previous profile self-test")
            assert_profile_output(source, minimal, "none", "none")
            assert_unchanged(minimal, protected)
            commit(minimal, f"adopt {old_source.name}")
            require_result(sync(old_source, minimal, "--check", runner=minimal), 0, "mode=full parity")
            require_result(make_sync(source, minimal, "--apply"), 0, "mode=full applied")
            assert_profile_self_test(minimal, delivered=False)
            assert_unchanged(minimal, protected)
            commit(minimal, "return to the new portable source")
            require_result(sync(source, minimal, "--check", runner=minimal), 0, "mode=full parity")
            print(f"template sync canary: {old_source.name} rollback and re-adoption pass")


def check(source: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="template-sync-canary-") as temp:
        work = Path(temp)
        target = work / "target"
        clone(source, target)
        initialized = initialize(source, target, "oidc-jwt", "bounded")
        if initialized.returncode:
            raise AssertionError(initialized.stderr)
        assert_profile_output(source, target, "oidc-jwt", "bounded")
        for outbound_http, webhooks in (("bounded", "durable"), ("none", "none")):
            webhook_target = work / f"webhooks-{webhooks}"
            clone(source, webhook_target)
            webhook_initialized = initialize(
                source, webhook_target, "none", outbound_http, database="postgres", jobs="postgres",
                webhooks=webhooks, inbound_webhooks="standard-webhooks",
            )
            if webhook_initialized.returncode:
                raise AssertionError(webhook_initialized.stderr)
            assert_profile_output(
                source, webhook_target, "none", outbound_http, jobs="postgres",
                webhooks=webhooks, inbound_webhooks="standard-webhooks",
            )
            # Initialization removes unselected adapter carriers. Commit that
            # derived baseline before sync admission, as the existing jobs-none
            # canary does, so the sync check exercises parity rather than its
            # production dirty-selected-target refusal.
            commit(webhook_target, "initialize webhook profiles")
            webhook_sync = sync(source, webhook_target, "--check")
            if webhook_sync.returncode:
                raise AssertionError(f"webhook profile sync parity failed: {webhook_sync.stderr}")
        omitted_authn = work / "omitted-authn"
        clone(source, omitted_authn)
        profile_initialized = initialize(source, omitted_authn, None)
        if profile_initialized.returncode:
            raise AssertionError(profile_initialized.stderr)
        assert_profile_output(source, omitted_authn, "none", "none")
        before = tree_state(omitted_authn)
        replay = initialize(source, omitted_authn, "none")
        if replay.returncode or tree_state(omitted_authn) != before:
            raise AssertionError("none lock replay changed the sync canary target")
        mismatch = initialize(source, omitted_authn, "oidc-jwt")
        if mismatch.returncode == 0 or tree_state(omitted_authn) != before:
            raise AssertionError("none sync canary accepted an authn profile migration")
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
        jobs_owned = jobs_none / "scripts/template/sync.py"
        jobs_owned.write_bytes(jobs_owned.read_bytes() + b"\n# committed full-sync drift\n")
        commit_paths(jobs_none, "committed full sync drift", "scripts/template/sync.py")
        jobs_apply = sync(source, jobs_none, "--apply")
        if jobs_apply.returncode:
            raise AssertionError(f"postgres jobs-none full sync apply failed: {jobs_apply.stderr}")
        if not (jobs_none / "migrations").is_dir():
            raise AssertionError("postgres jobs-none sync removed migrations/")
        for restored in (
            "crates/infra-jobs",
            "migrations/20260924000001_create_background_jobs.sql",
            "crates/config/src/jobs.rs",
        ):
            if (jobs_none / restored).exists() or (jobs_none / restored).is_symlink():
                raise AssertionError(f"postgres jobs-none sync restored {restored}")
        assert_profile_pack(source, jobs_none, "jobs", False)
        assert_profile_pack(source, jobs_none, "jobs-http-idempotency", False)
        skill = target / ".agents/skills/local-service-skill"
        skill.mkdir(parents=True)
        (skill / ".service-owned").write_text("", encoding="utf-8")
        (skill / "SKILL.md").write_text(
            "---\nname: local-service-skill\ndescription: Use when testing local service-owned skills.\n---\n",
            encoding="utf-8",
        )
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
        cargo_artifacts = target / "target"
        cargo_artifacts.mkdir(exist_ok=True)
        local_pipes = (cargo_artifacts / "build-pipe", target / ".env.local-pipe", target / "docs/service-pipe")
        for pipe in local_pipes:
            os.mkfifo(pipe)
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
        if (
            full_refusal.returncode != 2
            or "dirty selected target path: make/template.mk" not in full_refusal.stderr
            or tooling.read_bytes() != bytes_before
        ):
            raise AssertionError("full sync did not preserve dirty portable tooling refusal")

        tooling.write_bytes(tooling_before)

        full_owned = tooling
        full_owned.write_bytes(full_owned.read_bytes() + b"\n# committed full-sync drift\n")
        commit_paths(target, "committed full sync drift", "make/template.mk")
        full_drift = sync(source, target, "--check")
        if full_drift.returncode != 1:
            raise AssertionError(f"full sync check should report committed drift, got {full_drift.returncode}: {full_drift.stderr}")
        full_apply = sync(source, target, "--apply")
        if full_apply.returncode:
            raise AssertionError(f"full sync apply failed: {full_apply.stderr}")
        if "Local canary architecture fact." not in service_doc.read_text(encoding="utf-8") or not (skill / ".service-owned").is_file():
            raise AssertionError("full sync changed local authority or marked service skill")
        assert_profile_output(source, target, "oidc-jwt", "bounded")
        commit(target, "adopt full portable snapshot")
        full_parity = sync(source, target, "--check")
        if full_parity.returncode:
            raise AssertionError(f"committed full parity failed: {full_parity.stderr}")
        if not all(pipe.is_fifo() for pipe in local_pipes):
            raise AssertionError("portable sync removed service-local files")

        for invalid, diagnostic in (
            (b'{"env":{},"env":{}}', ".claude/settings.json: JSON object has a duplicate key"),
            (b'{"extra":NaN}', ".claude/settings.json: JSON contains unsupported constant NaN"),
            (b'{"extra":"\xff"}', ".claude/settings.json is not UTF-8"),
        ):
            settings.write_bytes(invalid)
            commit_paths(target, "invalid settings", ".claude/settings.json")
            refusal = sync(source, target, "--apply", instructions_only=True)
            if refusal.returncode != 2 or diagnostic not in refusal.stderr or settings.read_bytes() != invalid:
                raise AssertionError(f"invalid settings did not refuse unchanged: {refusal.stderr}")

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
        rendered_text = settings.read_text(encoding="utf-8")
        if not rendered_text.endswith("\n") or json.loads(rendered_text) != {
            "unrelated": {"keep": True},
            "env": {"CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH": "3"},
        }:
            raise AssertionError("settings repair lost unrelated consumer keys or did not normalize the managed setting")
        commit_paths(target, "adopt managed setting", ".claude/settings.json")

        make_cases = (
            ("space-target", b"\n build: ; @:\n", "overrides"),
            ("reserved-initializer", b"\ntemplate-init: ; @:\n", "overrides"),
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

        # Retired portable manifest entries are refused when dirty and pruned when clean;
        # legacy scripts/ci/service-* hooks refuse full sync before mutating the target.
        retired_target = work / "retired-manifest"
        clone(target, retired_target)
        retired_script = retired_target / "scripts/ci/legacy-portable.sh"
        retired_script.parent.mkdir(parents=True, exist_ok=True)
        retired_script.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        manifest_file = retired_target / "template-owned.paths"
        manifest_file.write_text(
            manifest_file.read_text(encoding="utf-8") + "scripts/ci/legacy-portable.sh\n",
            encoding="utf-8",
        )
        commit_paths(retired_target, "record legacy portable script", "scripts/ci/legacy-portable.sh", "template-owned.paths")

        legacy_hook = retired_target / "scripts/ci/service-changed-surfaces.sh"
        legacy_hook.write_text("service_changed_surfaces() { :; }\n", encoding="utf-8")
        commit_paths(retired_target, "add legacy hook", "scripts/ci/service-changed-surfaces.sh")
        hook_refusal = sync(source, retired_target, "--apply")
        if hook_refusal.returncode != 2 or "legacy service hook" not in hook_refusal.stderr:
            raise AssertionError(f"legacy service hook did not refuse full sync: {hook_refusal.stderr}")
        run(["git", "rm", "-qf", "scripts/ci/service-changed-surfaces.sh"], cwd=retired_target)
        commit(retired_target, "remove legacy hook")

        retired_script.write_text("#!/bin/sh\necho dirty\n", encoding="utf-8")
        dirty_retired = sync(source, retired_target, "--apply")
        if dirty_retired.returncode != 2 or "dirty selected target path: scripts/ci/legacy-portable.sh" not in dirty_retired.stderr:
            raise AssertionError(f"dirty retired portable file did not refuse sync: {dirty_retired.stderr}")
        run(["git", "checkout", "--", "scripts/ci/legacy-portable.sh"], cwd=retired_target)

        retired_apply = sync(source, retired_target, "--apply")
        if retired_apply.returncode != 0 or retired_script.exists() or (retired_target / "scripts/ci").exists():
            raise AssertionError(f"clean retired portable file/dir was not pruned on sync: {retired_apply.stderr}")
    print("template sync canary: pass")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--previous-revision", help="run the installed-client compatibility canary against this Git revision")
    args = parser.parse_args()
    source = args.source.resolve()
    if args.previous_revision:
        check_previous(source, args.previous_revision)
    else:
        check(source)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
