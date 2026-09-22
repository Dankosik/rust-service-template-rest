#!/usr/bin/env python3
"""Committed-source sync composition canary with local-byte preservation oracles."""

from __future__ import annotations

import argparse
import hashlib
import os
import subprocess
import tempfile
from pathlib import Path


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
    for args in (["git", "add", "--", *paths], ["git", "commit", "-qm", message, "--", *paths]):
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


def check(source: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="template-sync-canary-") as temp:
        work = Path(temp)
        target = work / "target"
        clone(source, target)
        initialized = run(
            ["bash", os.fspath(source / "scripts/init-module.sh"), "--repo", os.fspath(target), "--service-name", "canary-api", "--repository", "https://github.com/example/canary-api", "--description", "Canary API", "--codeowner", "@example/platform", "--database", "none", "--agent-harness", "claude"],
            cwd=source,
        )
        if initialized.returncode:
            raise AssertionError(initialized.stderr)
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
