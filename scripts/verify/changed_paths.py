#!/usr/bin/env python3
"""Resolve changed repository paths from git refs, worktree state, or explicit file lists."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path

sys.dont_write_bytecode = True


class ChangedPathsError(RuntimeError):
    pass


def resolve_repo_root(cwd: Path | None = None) -> Path:
    start = (cwd or Path.cwd()).resolve()
    result = subprocess.run(
        ["git", "-c", "core.fsmonitor=false", "rev-parse", "--show-toplevel"],
        cwd=start,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode == 0 and result.stdout.strip():
        return Path(result.stdout.strip()).resolve()
    return start


def _git_bytes(repo: Path, args: Sequence[str]) -> bytes:
    env = {**os.environ, "GIT_OPTIONAL_LOCKS": "0"}
    try:
        result = subprocess.run(
            ["git", "-c", "core.fsmonitor=false", "-C", os.fspath(repo), *args],
            capture_output=True,
            check=False,
            env=env,
        )
    except OSError as error:
        raise ChangedPathsError(f"git is unavailable: {error}")
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        raise ChangedPathsError(detail or f"git {' '.join(args)} failed")
    return result.stdout


def require_commit(repo: Path, ref: str) -> str:
    env = {**os.environ, "GIT_OPTIONAL_LOCKS": "0"}
    try:
        result = subprocess.run(
            [
                "git",
                "-c",
                "core.fsmonitor=false",
                "-C",
                os.fspath(repo),
                "rev-parse",
                "--verify",
                "--end-of-options",
                f"{ref}^{{commit}}",
            ],
            capture_output=True,
            text=True,
            check=False,
            env=env,
        )
    except OSError as error:
        raise ChangedPathsError(f"git is unavailable: {error}")
    if result.returncode != 0 or not result.stdout.strip():
        raise ChangedPathsError(
            f"verification base is unavailable: {ref}; set BASE_REF to a readable commit"
        )
    return result.stdout.strip()


def _has_merge_base(repo: Path, left: str, right: str) -> bool:
    env = {**os.environ, "GIT_OPTIONAL_LOCKS": "0"}
    result = subprocess.run(
        ["git", "-c", "core.fsmonitor=false", "-C", os.fspath(repo), "merge-base", left, right],
        capture_output=True,
        check=False,
        env=env,
    )
    return result.returncode == 0


def _split_nul_paths(raw: bytes) -> list[str]:
    if not raw:
        return []
    return [entry.decode("utf-8") for entry in raw.split(b"\0") if entry]


def _tracked_diff(repo: Path, *diff_args: str) -> list[str]:
    raw = _git_bytes(repo, ["diff", "--name-only", "-z", "--no-renames", *diff_args])
    return _split_nul_paths(raw)


def changed_paths(
    repo: Path | str,
    *,
    base: str | None = None,
    union: str | None = None,
    files: Sequence[str] | None = None,
) -> list[str]:
    provided = sum(value is not None for value in (base, union, files))
    if provided > 1:
        raise ChangedPathsError("choose at most one of --base, --union, or --files")

    repo_path = Path(repo)
    if files is not None:
        return sorted({path for path in files if path})

    if base is None and union is None:
        union = os.environ.get("BASE_REF") or "origin/main"

    if base is not None:
        require_commit(repo_path, base)
        require_commit(repo_path, "HEAD")
        return sorted(set(_tracked_diff(repo_path, base, "HEAD")))

    assert union is not None
    require_commit(repo_path, union)
    require_commit(repo_path, "HEAD")
    if _has_merge_base(repo_path, union, "HEAD"):
        committed = _tracked_diff(repo_path, f"{union}...HEAD")
    else:
        committed = _tracked_diff(repo_path, union, "HEAD")
    unstaged = _tracked_diff(repo_path)
    staged = _tracked_diff(repo_path, "--cached")
    untracked = _split_nul_paths(
        _git_bytes(repo_path, ["ls-files", "-z", "--others", "--exclude-standard"])
    )
    return sorted({*committed, *unstaged, *staged, *untracked})


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=None, help="repository root directory")
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--base", metavar="REF", help="committed diff from REF to HEAD")
    group.add_argument(
        "--union",
        metavar="REF",
        help="union of branch diff since REF and staged, unstaged, and untracked worktree paths",
    )
    group.add_argument("--files", nargs="*", metavar="PATH", help="explicit changed paths")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    repo = args.repo.resolve() if args.repo is not None else resolve_repo_root()
    try:
        paths = changed_paths(repo, base=args.base, union=args.union, files=args.files)
    except ChangedPathsError as error:
        print(str(error), file=sys.stderr)
        return 2
    for path in paths:
        print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
