#!/usr/bin/env python3
"""Safe default-branch fast-forward, drift checks, and verified post-merge retirement for Git worktrees."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

SHARED_ROOT_FILES = frozenset(
    {
        "AGENTS.md",
        "Cargo.lock",
        "Cargo.toml",
        "Makefile",
        "clippy.toml",
        "deny.toml",
        "rust-toolchain.toml",
        "rustfmt.toml",
        "quality/architecture.json",
        "tools/versions.env",
    }
)

STATIC_SHARED_PREFIXES = (
    ".sqlx/",
    "api/",
    "migrations/",
)

SHARED_ARCHITECTURE_ROLES = frozenset({"config", "health", "contract", "shared-rules"})

MAX_LISTED_FILES = 20


class GitError(RuntimeError):
    pass


@dataclass(frozen=True)
class WorktreeEntry:
    path: Path
    branch_ref: str | None
    is_primary: bool
    prunable: bool
    locked: bool


def _git(*args: str, cwd: Path, check: bool = True) -> subprocess.CompletedProcess[bytes]:
    result = subprocess.run(
        ["git", *args],
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and result.returncode != 0:
        stderr = result.stderr.decode("utf-8", errors="replace").strip()
        raise GitError(stderr or f"git {' '.join(args)} exited with {result.returncode}")
    return result


def _git_text(*args: str, cwd: Path) -> str:
    return os.fsdecode(_git(*args, cwd=cwd).stdout).rstrip("\r\n")


def _git_nul_lines(*args: str, cwd: Path) -> list[str]:
    raw = _git(*args, cwd=cwd).stdout
    return [os.fsdecode(item) for item in raw.split(b"\x00") if item]


def _repo_root() -> Path:
    return Path(_git_text("rev-parse", "--show-toplevel", cwd=Path.cwd()))


def _resolve_branch(root: Path, remote: str, explicit: str | None) -> str:
    if explicit:
        return explicit
    probe = _git("symbolic-ref", "--quiet", f"refs/remotes/{remote}/HEAD", cwd=root, check=False)
    if probe.returncode == 0:
        ref = os.fsdecode(probe.stdout).rstrip("\r\n")
        prefix = f"refs/remotes/{remote}/"
        if ref.startswith(prefix) and len(ref) > len(prefix):
            return ref[len(prefix) :]
    return "main"


def _fetch_remote_branch(root: Path, remote: str, branch: str) -> None:
    _git("fetch", remote, f"+refs/heads/{branch}:refs/remotes/{remote}/{branch}", cwd=root)


def _list_worktrees(root: Path) -> list[WorktreeEntry]:
    raw = _git("worktree", "list", "--porcelain", "-z", cwd=root).stdout
    entries: list[WorktreeEntry] = []
    current_path: Path | None = None
    current_branch: str | None = None
    prunable = False
    locked = False

    def flush() -> None:
        if current_path is not None:
            entries.append(
                WorktreeEntry(
                    path=current_path,
                    branch_ref=current_branch,
                    is_primary=len(entries) == 0,
                    prunable=prunable,
                    locked=locked,
                )
            )

    for raw_line in raw.split(b"\x00"):
        line = os.fsdecode(raw_line)
        if not line:
            flush()
            current_path = None
            current_branch = None
            prunable = False
            locked = False
            continue
        if line.startswith("worktree "):
            current_path = Path(line[len("worktree ") :])
        elif line.startswith("branch "):
            current_branch = line[len("branch ") :]
        elif line.startswith("prunable"):
            prunable = True
        elif line.startswith("locked"):
            locked = True

    flush()
    return entries


def _find_branch_worktree(root: Path, branch: str) -> WorktreeEntry | None:
    target_ref = f"refs/heads/{branch}"
    for entry in _list_worktrees(root):
        if entry.branch_ref == target_ref:
            return entry
    return None


def _short_sha(root: Path, rev: str) -> str:
    return _git_text("rev-parse", "--short", rev, cwd=root)


def _ref_exists(root: Path, full_ref: str) -> bool:
    return _git("show-ref", "--verify", "--quiet", full_ref, cwd=root, check=False).returncode == 0


def _is_ancestor(root: Path, ancestor: str, descendant: str) -> bool:
    probe = _git("merge-base", "--is-ancestor", ancestor, descendant, cwd=root, check=False)
    if probe.returncode == 0:
        return True
    if probe.returncode == 1:
        return False
    stderr = probe.stderr.decode("utf-8", errors="replace").strip()
    raise GitError(stderr or f"git merge-base --is-ancestor failed with {probe.returncode}")


def cmd_sync_main(root: Path, remote: str, branch: str) -> int:
    _fetch_remote_branch(root, remote, branch)
    remote_ref = f"refs/remotes/{remote}/{branch}"
    local_ref = f"refs/heads/{branch}"
    remote_oid = _git_text("rev-parse", remote_ref, cwd=root)
    remote_short = _short_sha(root, remote_ref)
    checked_out = _find_branch_worktree(root, branch)

    if not _ref_exists(root, local_ref):
        if checked_out is not None:
            raise GitError(
                f"{branch} is checked out as an unborn branch at {checked_out.path}; refusing to create ref beneath active worktree"
            )
        _git("branch", branch, remote_ref, cwd=root)
        print(f"created local {branch} at {remote_short} ({remote}/{branch})")
        return 0

    local_oid = _git_text("rev-parse", local_ref, cwd=root)
    local_short = _short_sha(root, local_ref)
    if local_oid == remote_oid:
        print(f"local {branch} is already up to date at {local_short}")
        return 0

    if not _is_ancestor(root, local_oid, remote_oid):
        raise GitError(
            f"local {branch} ({local_short}) has commits not in {remote}/{branch} ({remote_short}); refusing non-fast-forward update"
        )

    if checked_out is not None:
        if checked_out.prunable or not checked_out.path.is_dir():
            raise GitError(
                f"{branch} is registered to missing/prunable worktree {checked_out.path}; inspect worktrees before updating"
            )
        if _git("status", "--porcelain", "--untracked-files=normal", cwd=checked_out.path).stdout.strip():
            raise GitError(f"{branch} is checked out with uncommitted changes at {checked_out.path}; left untouched")
        _git("merge", "--ff-only", remote_ref, cwd=checked_out.path)
        print(f"fast-forwarded checked-out {branch} at {checked_out.path}: {local_short} -> {remote_short}")
        return 0

    _git("fetch", ".", f"{remote_ref}:{local_ref}", cwd=root)
    print(f"fast-forwarded local {branch}: {local_short} -> {remote_short}")
    return 0


def _shared_prefixes(root: Path) -> tuple[str, ...]:
    prefixes = list(STATIC_SHARED_PREFIXES)
    arch_path = root / "quality/architecture.json"
    if arch_path.is_file():
        try:
            policy = json.loads(arch_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            raise GitError(f"failed to read {arch_path}: {exc}") from exc
        members = policy.get("members", {})
        if isinstance(members, dict):
            for manifest_rel, entry in sorted(members.items()):
                if not isinstance(entry, dict) or entry.get("role") not in SHARED_ARCHITECTURE_ROLES:
                    continue
                if manifest_rel.endswith("/Cargo.toml"):
                    prefixes.append(manifest_rel[: -len("Cargo.toml")])
    return tuple(dict.fromkeys(prefixes))


def _is_shared_surface(path: str, shared_prefixes: tuple[str, ...]) -> bool:
    return path in SHARED_ROOT_FILES or any(path.startswith(prefix) for prefix in shared_prefixes)


def _crate_of(path: str) -> str | None:
    parts = path.split("/")
    if len(parts) >= 2 and parts[0] == "crates":
        return f"crates/{parts[1]}"
    return None


def _print_limited(label: str, items: list[str]) -> None:
    if not items:
        return
    shown = items[:MAX_LISTED_FILES]
    print(f"{label} ({len(items)}):")
    for item in shown:
        print(f"  - {item}")
    remaining = len(items) - len(shown)
    if remaining > 0:
        print(f"  ... (+{remaining} more)")


def _sync_hint(remote: str, branch: str, default_branch: str) -> str:
    flags: list[str] = []
    if remote != "origin":
        flags.append(f"--remote {remote}")
    if branch != default_branch:
        flags.append(f"--branch {branch}")
    suffix = f" {' '.join(flags)}" if flags else ""
    return f"python3 scripts/worktree-sync.py sync-main{suffix}` or `wt sync-main{suffix}"


def cmd_drift(root: Path, remote: str, branch: str, default_branch: str, *, fetch: bool) -> int:
    if fetch:
        _fetch_remote_branch(root, remote, branch)

    remote_ref = f"refs/remotes/{remote}/{branch}"
    local_ref = f"refs/heads/{branch}"
    if not _ref_exists(root, remote_ref):
        raise GitError(f"missing {remote}/{branch}; run without --no-fetch or fetch {remote}/{branch} first")

    current_branch = _git_text("rev-parse", "--abbrev-ref", "HEAD", cwd=root)
    base = _git_text("merge-base", "HEAD", remote_ref, cwd=root)
    behind = int(_git_text("rev-list", "--count", f"{base}..{remote_ref}", cwd=root))
    ahead = int(_git_text("rev-list", "--count", f"{base}..HEAD", cwd=root))

    print(f"branch: {current_branch}")
    print(f"target: {remote}/{branch} ({_short_sha(root, remote_ref)})")
    print(f"divergence: ahead {ahead}, behind {behind}")

    if _ref_exists(root, local_ref):
        counts = _git_text("rev-list", "--left-right", "--count", f"{local_ref}...{remote_ref}", cwd=root).split()
        local_ahead, local_behind = int(counts[0]), int(counts[1])
        if local_ahead > 0 and local_behind > 0:
            print(
                f"local_{branch}: diverged from {remote}/{branch} (ahead {local_ahead}, behind {local_behind}; reconcile manually)"
            )
        elif local_ahead > 0:
            print(f"local_{branch}: ahead of {remote}/{branch} by {local_ahead} (unpushed local commits)")
        elif local_behind > 0:
            hint = _sync_hint(remote, branch, default_branch)
            print(f"local_{branch}: behind {remote}/{branch} by {local_behind} (run `{hint}`)")

    if behind == 0:
        print("status: up-to-date")
        return 0

    shared_prefixes = _shared_prefixes(root)
    upstream_files = set(
        _git_nul_lines("diff", "--no-renames", "--name-only", "-z", f"{base}..{remote_ref}", "--", cwd=root)
    )
    branch_files = set(_git_nul_lines("diff", "--no-renames", "--name-only", "-z", f"{base}..HEAD", "--", cwd=root))
    branch_files.update(
        _git_nul_lines("diff", "--no-renames", "--name-only", "-z", "--cached", base, "--", cwd=root)
    )
    branch_files.update(_git_nul_lines("diff", "--no-renames", "--name-only", "-z", base, "--", cwd=root))
    branch_files.update(_git_nul_lines("ls-files", "--others", "--exclude-standard", "-z", cwd=root))

    direct_overlap = sorted(upstream_files & branch_files)
    upstream_crates = {c for p in upstream_files if (c := _crate_of(p)) is not None}
    branch_crates = {c for p in branch_files if (c := _crate_of(p)) is not None}
    same_crate_overlap = sorted(upstream_crates & branch_crates)
    shared_surface = sorted(p for p in upstream_files if _is_shared_surface(p, shared_prefixes))

    if direct_overlap:
        status = "direct-overlap"
        action = "sync now before expanding diff; resolve conflicts with merge-conflict-resolution and re-run validation"
    elif same_crate_overlap or shared_surface:
        status = "shared-or-same-crate"
        action = "sync at the next clean commit before building further on stale contracts; re-run validation"
    else:
        status = "disjoint"
        action = "safe to finish the current slice; sync before final validation, PR, or merge"

    print(f"status: {status}")
    print(f"action: {action}")
    _print_limited("direct_overlap", direct_overlap)
    _print_limited("same_crate_overlap", same_crate_overlap)
    _print_limited("shared_surface", shared_surface)
    _print_limited("upstream_files", sorted(upstream_files))
    return 0


def _verify_merged_pr(root: Path, pr: str, target_branch: str, target_oid: str, default_branch: str) -> str:
    try:
        probe = subprocess.run(
            ["gh", "pr", "view", pr, "--json", "state,headRefName,headRefOid,baseRefName"],
            cwd=root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    except OSError as exc:
        raise GitError(f"failed to run gh pr view {pr}: {exc}") from exc
    if probe.returncode != 0:
        stderr = probe.stderr.decode("utf-8", errors="replace").strip()
        raise GitError(stderr or f"gh pr view {pr} exited with {probe.returncode}")
    try:
        data = json.loads(probe.stdout.decode("utf-8"))
    except json.JSONDecodeError as exc:
        raise GitError(f"invalid JSON from gh pr view {pr}: {exc}") from exc
    if data.get("state") != "MERGED":
        raise GitError(f"PR #{pr} state is {data.get('state')!r}, expected 'MERGED'")
    if data.get("headRefName") != target_branch:
        raise GitError(f"PR #{pr} head branch is {data.get('headRefName')!r}, expected {target_branch!r}")
    if data.get("baseRefName") != default_branch:
        raise GitError(f"PR #{pr} base branch is {data.get('baseRefName')!r}, expected {default_branch!r}")
    if data.get("headRefOid") != target_oid:
        raise GitError(
            f"PR #{pr} merged head {str(data.get('headRefOid', ''))[:9]} does not match local {target_branch} ({target_oid[:9]}); refusing deletion"
        )
    return f"PR #{pr} merged at {target_oid[:9]}"


def cmd_retire(
    root: Path,
    target_branch: str,
    remote: str,
    default_branch: str,
    *,
    pr: str | None,
    fetch: bool,
) -> int:
    if target_branch == default_branch:
        raise GitError(f"refusing to retire default branch {default_branch!r}")

    local_ref = f"refs/heads/{target_branch}"
    if not _ref_exists(root, local_ref):
        raise GitError(f"local branch {target_branch!r} does not exist")

    wt = _find_branch_worktree(root, target_branch)
    if wt is not None:
        if wt.is_primary or (wt.path / ".git").is_dir():
            raise GitError(f"refusing to remove primary worktree {wt.path}")
        if wt.locked:
            raise GitError(f"worktree {wt.path} is locked; refusing removal")
        if wt.prunable or not wt.path.is_dir():
            raise GitError(f"worktree {wt.path} is missing/prunable; inspect before retiring {target_branch}")
        cwd_real = Path.cwd().resolve()
        wt_real = wt.path.resolve()
        if cwd_real == wt_real or wt_real in cwd_real.parents:
            raise GitError(f"current working directory is inside target worktree {wt.path}; run from outside it")
        if _git("status", "--porcelain", "--untracked-files=normal", cwd=wt.path).stdout.strip():
            raise GitError(f"worktree {wt.path} has uncommitted or untracked changes; refusing removal")

    if fetch:
        _fetch_remote_branch(root, remote, default_branch)

    remote_default_ref = f"refs/remotes/{remote}/{default_branch}"
    if not _ref_exists(root, remote_default_ref):
        raise GitError(
            f"missing {remote}/{default_branch}; run without --no-fetch or fetch {remote}/{default_branch} first"
        )

    target_oid = _git_text("rev-parse", local_ref, cwd=root)
    target_short = _short_sha(root, local_ref)
    remote_default_oid = _git_text("rev-parse", remote_default_ref, cwd=root)

    if _is_ancestor(root, target_oid, remote_default_oid):
        reason = f"ancestor of {remote}/{default_branch}"
    elif pr is not None:
        reason = _verify_merged_pr(root, pr, target_branch, target_oid, default_branch)
    else:
        raise GitError(
            f"{target_branch} ({target_short}) is not an ancestor of {remote}/{default_branch}; pass --pr <number> to verify a GitHub squash-merge"
        )

    if wt is not None:
        head_oid = _git_text("rev-parse", "HEAD", cwd=wt.path)
        if head_oid != target_oid:
            raise GitError(f"worktree {wt.path} HEAD moved during retirement; retry after checking status")
        _git("worktree", "remove", "--", os.fspath(wt.path), cwd=root)
        print(f"removed worktree: {wt.path}")

    _git("update-ref", "-d", local_ref, target_oid, cwd=root)
    print(f"deleted local branch: {target_branch} ({target_short}; {reason})")

    remote_branch_ref = f"refs/remotes/{remote}/{target_branch}"
    if fetch and _ref_exists(root, remote_branch_ref):
        remote_probe = _git("ls-remote", "--heads", remote, f"refs/heads/{target_branch}", cwd=root, check=False)
        if remote_probe.returncode == 0 and not remote_probe.stdout.strip():
            _git("update-ref", "-d", remote_branch_ref, cwd=root)
            print(f"pruned stale remote-tracking ref: {remote}/{target_branch}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    sync_parser = subparsers.add_parser("sync-main", help="Fetch and safely fast-forward the local default branch")
    sync_parser.add_argument("--remote", default="origin", help="Git remote name (default: origin)")
    sync_parser.add_argument("--branch", default=None, help="Default branch name (default: remote HEAD or main)")

    drift_parser = subparsers.add_parser("drift", help="Report commits behind default branch and file/crate overlap")
    drift_parser.add_argument("--remote", default="origin", help="Git remote name (default: origin)")
    drift_parser.add_argument("--branch", default=None, help="Default branch name (default: remote HEAD or main)")
    drift_parser.add_argument("--no-fetch", action="store_true", help="Inspect local refs without running git fetch")

    retire_parser = subparsers.add_parser(
        "retire", help="Verify a branch is merged, remove its linked worktree, and delete the local branch"
    )
    retire_parser.add_argument("target_branch", help="Merged local task branch to retire")
    retire_parser.add_argument("--pr", default=None, help="GitHub PR number proving a squash-merge at exact HEAD")
    retire_parser.add_argument("--remote", default="origin", help="Git remote name (default: origin)")
    retire_parser.add_argument("--branch", default=None, help="Default branch name (default: remote HEAD or main)")
    retire_parser.add_argument("--no-fetch", action="store_true", help="Inspect local refs without running git fetch")

    args = parser.parse_args(argv)
    try:
        root = _repo_root()
        default_branch = _resolve_branch(root, args.remote, None)
        branch = args.branch or default_branch
        if args.command == "sync-main":
            return cmd_sync_main(root, args.remote, branch)
        if args.command == "drift":
            return cmd_drift(root, args.remote, branch, default_branch, fetch=not args.no_fetch)
        return cmd_retire(root, args.target_branch, args.remote, branch, pr=args.pr, fetch=not args.no_fetch)
    except GitError as exc:
        print(f"worktree-sync: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
