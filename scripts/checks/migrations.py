#!/usr/bin/env python3
"""Enforce append-only SQL migration history and lint newly added migrations with squawk."""

from __future__ import annotations

import argparse
import os
import re
import shlex
import shutil
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path

from scripts import lib

MIGRATION_RE = re.compile(r"^([0-9]+)_.+\.sql$")
MAX_I64 = (1 << 63) - 1
SQUAWK_EXCLUDES = "require-lock-timeout,require-statement-timeout,prefer-bigint-over-smallint"


class MigrationCheckError(lib.ScriptError):
    pass


def _is_migration(rel_path: str) -> bool:
    return bool(MIGRATION_RE.match(rel_path.rsplit("/", 1)[-1]))


def _parse_version(rel_path: str) -> int | None:
    match = MIGRATION_RE.match(rel_path.rsplit("/", 1)[-1])
    return int(match.group(1), 10) if match else None


def _resolve_base(repo: Path, base_ref: str | None) -> tuple[str, str, bool]:
    if not base_ref:
        return "HEAD", "worktree", True
    verify = subprocess.run(
        ["git", "-C", os.fspath(repo), "cat-file", "-e", f"{base_ref}^{{commit}}"],
        capture_output=True,
        check=False,
    )
    if verify.returncode != 0:
        raise MigrationCheckError(f"BASE_REF {base_ref} is not a readable commit")
    mb = subprocess.run(
        ["git", "-C", os.fspath(repo), "merge-base", base_ref, "HEAD"],
        capture_output=True,
        text=True,
        check=False,
    )
    if mb.returncode == 0 and mb.stdout.strip():
        sha = mb.stdout.strip()
        return sha, f"merge-base:{sha}", False
    sha = lib.git("rev-parse", f"{base_ref}^{{commit}}", cwd=repo).strip()
    return sha, f"base:{sha}", False


def check_history(repo: Path, migration_dir: str, base_ref: str | None) -> tuple[list[str], str]:
    if not (repo / migration_dir).is_dir():
        raise MigrationCheckError(f"configured directory does not exist: {migration_dir}", exit_code=2)

    base, scope, include_untracked = _resolve_base(repo, base_ref)
    raw_status = lib.git(
        "diff", "--name-status", "-z", "--find-renames", "--find-copies", base, "--", f"{migration_dir}/",
        cwd=repo,
        binary=True,
    )
    tokens = raw_status.split(b"\0")
    added: list[str] = []
    refused: list[str] = []
    idx = 0
    while idx < len(tokens) and tokens[idx]:
        status = tokens[idx].decode("utf-8", "surrogateescape")
        idx += 1
        if idx >= len(tokens):
            raise MigrationCheckError("incomplete git diff record")
        path = tokens[idx].decode("utf-8", "surrogateescape")
        idx += 1
        if status.startswith(("R", "C")):
            if idx >= len(tokens):
                raise MigrationCheckError("incomplete git rename/copy record")
            dest = tokens[idx].decode("utf-8", "surrogateescape")
            idx += 1
            if (status.startswith("R") and _is_migration(path)) or _is_migration(dest):
                refused.append(f"{status}\t{path}\t{dest}")
        elif status == "A":
            added.append(path)
        elif _is_migration(path):
            refused.append(f"{status}\t{path}")

    if refused:
        raise MigrationCheckError(
            f"applied migration files are append-only; only additions are allowed ({scope})\n" + "\n".join(refused)
        )

    base_files = [
        raw.decode("utf-8", "surrogateescape")
        for raw in lib.git("ls-tree", "-r", "--name-only", "-z", base, "--", f"{migration_dir}/", cwd=repo, binary=True).split(b"\0")
        if raw
    ]
    if include_untracked:
        untracked = lib.git("ls-files", "-z", "--others", "--exclude-standard", "--", f"{migration_dir}/", cwd=repo, binary=True)
        added.extend(raw.decode("utf-8", "surrogateescape") for raw in untracked.split(b"\0") if raw)

    added_unique = sorted(set(added))
    base_versions = [(v, p) for p in base_files if (v := _parse_version(p)) is not None]
    added_versions = [(v, p) for p in added_unique if (v := _parse_version(p)) is not None]

    overflow = [p for v, p in (*base_versions, *added_versions) if not (0 <= v <= MAX_I64)]
    if overflow:
        raise MigrationCheckError(f"migration version exceeds i64 ({scope}): {', '.join(overflow)}")

    prior_max = max((v for v, _ in base_versions), default=None)
    if prior_max is not None:
        out_of_order = [p for v, p in added_versions if v <= prior_max]
        if out_of_order:
            raise MigrationCheckError(
                f"new migrations must be newer than prior version {prior_max} ({scope}): {', '.join(out_of_order)}"
            )

    seen_versions: dict[int, list[str]] = {}
    for v, p in added_versions:
        seen_versions.setdefault(v, []).append(p)
    dups = {v: ps for v, ps in seen_versions.items() if len(ps) > 1}
    if dups:
        detail = "; ".join(f"{v}: {', '.join(ps)}" for v, ps in sorted(dups.items()))
        raise MigrationCheckError(f"duplicate new migration versions ({scope}): {detail}")

    return [p for p in added_unique if _is_migration(p) and (repo / p).is_file()], scope


def lint_migrations(repo: Path, added_files: Sequence[str]) -> None:
    if not added_files:
        return
    transactional: list[str] = []
    outside_tx: list[str] = []
    for rel in added_files:
        if (repo / rel).read_bytes()[:17] == b"-- no-transaction":
            outside_tx.append(rel)
        else:
            transactional.append(rel)

    custom = os.environ.get("MIGRATION_LINTER", "").strip()
    if custom:
        cmd_base = shlex.split(custom)
    else:
        squawk = shutil.which("squawk")
        if squawk is None:
            raise MigrationCheckError("squawk is required; run `mise install`", exit_code=2)
        cmd_base = [squawk]

    common_flags = ["--pg-version", "14", "--exclude", SQUAWK_EXCLUDES]
    for mode_flag, files in (("--assume-in-transaction", transactional), ("--no-assume-in-transaction", outside_tx)):
        if not files:
            continue
        res = subprocess.run([*cmd_base, *common_flags, mode_flag, *files], cwd=repo, check=False)
        if res.returncode != 0:
            raise MigrationCheckError(
                "migration lint failed; fix the SQL or add `-- squawk-ignore <rule>` with a reason",
                exit_code=2 if (res.returncode == 2 and not custom) else 1,
            )
    print(f"migration lint passed ({len(transactional)} transactional, {len(outside_tx)} outside a transaction)")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(os.environ.get("MIGRATION_REPO_ROOT") or lib.ROOT))
    parser.add_argument("--dir", dest="migration_dir", default=os.environ.get("MIGRATION_DIR") or "migrations")
    parser.add_argument("--base", default=os.environ.get("BASE_REF") or None)
    args = parser.parse_args(argv)
    try:
        added, scope = check_history(args.repo.resolve(), args.migration_dir, args.base)
        print(f"migration history is append-only ({scope})")
        lint_migrations(args.repo.resolve(), added)
    except lib.ScriptError as exc:
        print(f"migration-check: {exc}", file=sys.stderr)
        return exc.exit_code
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
