#!/usr/bin/env python3
"""Refusal and one-shot preservation checks against an isolated candidate."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import tempfile
from pathlib import Path


def run(args: list[str], *, cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)


def state(root: Path) -> str:
    records: list[str] = []
    for path in sorted(root.rglob("*")):
        if ".git" in path.parts:
            continue
        relative = path.relative_to(root).as_posix()
        mode = path.lstat().st_mode
        if path.is_symlink():
            payload = os.readlink(path).encode()
        elif path.is_file():
            payload = path.read_bytes()
        else:
            records.append(f"dir {relative} {mode:o}")
            continue
        records.append(f"file {relative} {mode:o} {hashlib.sha256(payload).hexdigest()}")
    status = run(["git", "status", "--porcelain=v1", "--ignored"], cwd=root)
    records.append(f"git {status.stdout}")
    return "\n".join(records)


def clone(source: Path, destination: Path) -> None:
    result = run(["git", "clone", "--quiet", "--no-local", os.fspath(source), os.fspath(destination)], cwd=source)
    if result.returncode:
        raise AssertionError(result.stderr)


def init(source: Path, target: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    baseline = [
        ("--service-name", "safety-api"), ("--repository", "https://github.com/example/safety-api"),
        ("--description", "Safety API 👩‍💻"), ("--codeowner", "@example/platform"),
    ]
    replacements = set(extra[::2])
    identity = [value for pair in baseline if pair[0] not in replacements for value in pair]
    return run(
        [
            "bash", os.fspath(source / "scripts/init-module.sh"), "--repo", os.fspath(target),
            *identity, *extra,
        ],
        cwd=source,
    )


def must_refuse(source: Path, work: Path, label: str, *extra: str) -> None:
    target = work / label
    clone(source, target)
    before = state(target)
    result = init(source, target, *extra)
    if result.returncode == 0:
        raise AssertionError(f"{label}: initializer unexpectedly succeeded")
    if label != "duplicate-db" and "may be supplied once" in result.stderr:
        raise AssertionError(f"{label}: fixture accidentally exercised duplicate-option refusal")
    if state(target) != before:
        raise AssertionError(f"{label}: refusal changed target bytes or Git state")


def must_refuse_raw_description(source: Path, work: Path) -> None:
    target = work / "non-utf8-description"
    clone(source, target)
    before = state(target)
    result = subprocess.run(
        [
            b"bash", os.fsencode(source / "scripts/init-module.sh"), b"--repo", os.fsencode(target),
            b"--service-name", b"safety-api", b"--repository", b"https://github.com/example/safety-api",
            b"--description", b"bad\xed\xa0\x80description", b"--codeowner", b"@example/platform",
        ],
        cwd=os.fspath(source), stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
    )
    if result.returncode == 0 or state(target) != before:
        raise AssertionError("non-UTF-8 description was not a preserving refusal")


def assert_postgres_pack(source: Path, target: Path) -> None:
    profile = json.loads((source / "scripts/lib/template_profiles.json").read_text(encoding="utf-8"))
    for relative in profile["postgres"]["remove_when_none"]:
        path = target / relative.rstrip("/")
        if relative.endswith("/"):
            if path.is_symlink() or not path.is_dir():
                raise AssertionError(f"postgres output lacks retained directory {relative}")
        elif path.is_symlink() or not path.is_file():
            raise AssertionError(f"postgres output lacks retained file {relative}")


def check(source: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="template-init-safety-") as temp:
        work = Path(temp)
        # Invalid identity inputs refuse before a plan is written. Cc and the
        # explicit Unicode separators are refused; permitted emoji/ZWJ text is
        # exercised by the successful initialization below.
        must_refuse(source, work, "unknown-db", "--database", "sqlite")
        must_refuse(source, work, "duplicate-db", "--database", "none", "--database", "postgres")
        must_refuse(source, work, "codeowner-depth", "--codeowner", "@a/b/c")
        must_refuse(source, work, "reserved-abstract", "--service-name", "abstract")
        must_refuse(source, work, "reserved-gen", "--service-name", "gen")
        must_refuse(source, work, "c1-description", "--description", "bad\u0085description")
        must_refuse(source, work, "line-separator", "--description", "bad\u2028description")
        must_refuse(source, work, "paragraph-separator", "--description", "bad\u2029description")
        must_refuse_raw_description(source, work)

        target = work / "success"
        clone(source, target)
        local_adapter_note = target / ".codex/service-note.txt"
        local_adapter_note.write_text("service-owned sibling\n", encoding="utf-8")
        result = init(source, target, "--database", "none", "--agent-harness", "claude")
        if result.returncode:
            raise AssertionError(f"successful initialization failed: {result.stderr}")
        for removed in [
            "make/source.mk", "scripts/ci/template-init-check.sh", "scripts/tests/template-init-safety.py",
            "crates/infra-postgres", "crates/migrate", "migrations", "env/docker-compose.yml",
            "Grok.md", "QWEN.md", ".agents/codex-project.toml",
            ".codex/config.toml", ".codex/agents", ".cursor/rules/agent-harness.mdc", ".cursor/agents",
            ".grok/rules/harness.md", ".grok/agents", ".grok/roles", "opencode.json",
            ".opencode/rules/harness.md", ".opencode/agents", ".opencode/commands/orchestrator.md",
            ".opencode/plugins/task-subagents.js", ".qwen/agents", ".qwen/skills", ".qwen/settings.json",
        ]:
            if (target / removed).exists() or (target / removed).is_symlink():
                raise AssertionError(f"none/claude output retained {removed}")
        if local_adapter_note.read_text(encoding="utf-8") != "service-owned sibling\n":
            raise AssertionError("initializer changed unrelated product-root content")
        for empty_parent in (".cursor", ".grok", ".qwen", ".opencode/commands", ".opencode/plugins", ".opencode/rules"):
            if (target / empty_parent).exists():
                raise AssertionError(f"initializer retained an empty removed-pack parent: {empty_parent}")
        for parent in (".codex", ".cursor", ".grok", ".opencode", ".qwen"):
            path = target / parent
            if path.exists():
                print(f"preserved product parent {parent}: directory={path.is_dir()} "
                      f"symlink={path.is_symlink()} children={sorted(p.name for p in path.iterdir())}")
        if not (target / "template.lock").is_file() or not (target / "CLAUDE.md").is_file():
            raise AssertionError("initialized output omitted retained local state or selected carrier")
        lifecycle_source = (target / "crates/service/tests/lifecycle.rs").read_text(encoding="utf-8")
        if 'env!("CARGO_BIN_EXE_safety-api")' not in lifecycle_source:
            raise AssertionError("lifecycle test lost the exact hyphenated Cargo binary environment key")
        if not any((target / ".claude/skills").iterdir()):
            raise AssertionError("selected Claude skill links were not retained")
        service_skill = target / ".agents/skills/service-local"
        service_skill.mkdir()
        (service_skill / "SKILL.md").write_text(
            "# Service-local method\nRepository: https://github.com/example/safety-api\n",
            encoding="utf-8",
        )
        (service_skill / ".service-owned").write_text("", encoding="utf-8")
        (target / ".claude/skills/service-local").symlink_to("../../.agents/skills/service-local")
        service_spec = target / "specs/service-feature/spec.md"
        service_spec.parent.mkdir(parents=True)
        service_spec.write_text("Service-owned decision after initialization.\n", encoding="utf-8")
        with (target / "README.md").open("a", encoding="utf-8") as readme:
            readme.write("\nService-owned onboarding update.\n")
        after = state(target)
        repeat = init(source, target, "--database", "none", "--agent-harness", "claude")
        if repeat.returncode or state(target) != after:
            raise AssertionError("matching complete-lock initialization was not a byte-preserving no-op")
        (target / "template.lock").write_text("{\"state\": []}\n", encoding="utf-8")
        malformed_before = state(target)
        malformed = init(source, target, "--database", "none", "--agent-harness", "claude")
        if malformed.returncode == 0 or state(target) != malformed_before:
            raise AssertionError("malformed lock was not a preserving refusal")
        postgres_target = work / "postgres-replay"
        clone(source, postgres_target)
        postgres = init(source, postgres_target, "--database", "postgres", "--agent-harness", "core")
        if postgres.returncode:
            raise AssertionError(f"postgres initialization failed: {postgres.stderr}")
        assert_postgres_pack(source, postgres_target)
        (postgres_target / "specs/service-feature").mkdir(parents=True)
        (postgres_target / "specs/service-feature/decision.md").write_text(
            "Service-owned PostgreSQL decision.\n", encoding="utf-8"
        )
        postgres_before = state(postgres_target)
        postgres_repeat = init(source, postgres_target, "--database", "postgres", "--agent-harness", "core")
        if postgres_repeat.returncode or state(postgres_target) != postgres_before:
            raise AssertionError("complete postgres lock replay changed service-owned content")
        missing_pack = postgres_target / "make/profile-postgres.mk"
        missing_pack.unlink()
        missing_before = state(postgres_target)
        missing_replay = init(source, postgres_target, "--database", "postgres", "--agent-harness", "core")
        if missing_replay.returncode == 0 or state(postgres_target) != missing_before:
            raise AssertionError("complete postgres lock accepted a missing retained pack")
        for permitted in ("union", "raw"):
            allowed = work / f"permitted-{permitted}"
            clone(source, allowed)
            result = init(source, allowed, "--service-name", permitted, "--database", "none")
            if result.returncode:
                raise AssertionError(f"permitted Rust-adjacent name {permitted} was refused: {result.stderr}")
    print("template initializer safety: pass")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", required=True, type=Path)
    args = parser.parse_args()
    check(args.source.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
