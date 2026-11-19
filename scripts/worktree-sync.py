#!/usr/bin/env python3
"""Safe default-branch fast-forward, drift checks, and verified post-merge retirement for Git worktrees."""

from __future__ import annotations

import argparse
import json
import os
import re
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

_PR_NUMBER_RE = re.compile(r"^[1-9][0-9]*$")
_OID_RE = re.compile(r"^[0-9a-f]{40,64}$")
_GITHUB_URL_PATTERNS = (
    re.compile(r"^https?://github\.com/([^/]+/[^/]+?)(?:\.git)?/?$"),
    re.compile(r"^ssh://git@github\.com/([^/]+/[^/]+?)(?:\.git)?/?$"),
    re.compile(r"^git@github\.com:([^/]+/[^/]+?)(?:\.git)?/?$"),
)
_IN_PROGRESS_GIT_MARKERS = (
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
    "rebase-merge",
    "rebase-apply",
)


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


def _exact_ref_info(root: Path, namespace: str, full_ref: str) -> tuple[str, str] | None:
    raw = _git("for-each-ref", "--format=%(refname)%00%(objectname)%00%(symref)", namespace, cwd=root).stdout
    for record in raw.splitlines():
        parts = record.split(b"\x00")
        if len(parts) != 3:
            continue
        refname, oid, symref = (os.fsdecode(part) for part in parts)
        if refname == full_ref:
            return oid, symref
    return None


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


def _find_branch_worktrees(root: Path, branch: str) -> list[WorktreeEntry]:
    target_ref = f"refs/heads/{branch}"
    return [entry for entry in _list_worktrees(root) if entry.branch_ref == target_ref]


def _worktree_git_dir(wt_path: Path) -> Path:
    return Path(_git_text("rev-parse", "--path-format=absolute", "--git-dir", cwd=wt_path))


def _in_progress_markers(wt_path: Path) -> list[str]:
    git_dir = _worktree_git_dir(wt_path)
    return [name for name in _IN_PROGRESS_GIT_MARKERS if (git_dir / name).exists()]


def _rebase_head_name(wt_path: Path) -> str | None:
    git_dir = _worktree_git_dir(wt_path)
    for rel in ("rebase-merge/head-name", "rebase-apply/head-name"):
        head_file = git_dir / rel
        if head_file.is_file():
            return head_file.read_text(encoding="utf-8", errors="replace").strip()
    return None


def _short_sha(root: Path, rev: str) -> str:
    return _git_text("rev-parse", "--short", rev, cwd=root)


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
    remote_info = _exact_ref_info(root, f"refs/remotes/{remote}", remote_ref)
    if remote_info is None:
        raise GitError(f"missing {remote}/{branch} after fetch")
    remote_oid, _ = remote_info
    remote_short = _short_sha(root, remote_oid)
    matches = _find_branch_worktrees(root, branch)
    if len(matches) > 1:
        paths = ", ".join(os.fspath(entry.path) for entry in matches)
        raise GitError(f"{branch} is checked out in multiple worktrees ({paths}); reconcile manually")
    checked_out = matches[0] if matches else None

    local_info = _exact_ref_info(root, "refs/heads", local_ref)
    if local_info is None:
        if checked_out is not None:
            raise GitError(
                f"{branch} is checked out as an unborn branch at {checked_out.path}; refusing to create ref beneath active worktree"
            )
        _git("branch", branch, remote_ref, cwd=root)
        print(f"created local {branch} at {remote_short} ({remote}/{branch})")
        return 0

    local_oid, local_symref = local_info
    if local_symref:
        raise GitError(f"local {branch} is a symbolic ref ({local_symref}); refusing update")
    local_short = _short_sha(root, local_oid)
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
        markers = _in_progress_markers(checked_out.path)
        if markers:
            raise GitError(
                f"{branch} has an in-progress git operation ({', '.join(markers)}) at {checked_out.path}; left untouched"
            )
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
    remote_info = _exact_ref_info(root, f"refs/remotes/{remote}", remote_ref)
    if remote_info is None:
        raise GitError(f"missing {remote}/{branch}; run without --no-fetch or fetch {remote}/{branch} first")
    remote_oid, _ = remote_info

    current_branch = _git_text("rev-parse", "--abbrev-ref", "HEAD", cwd=root)
    base = _git_text("merge-base", "HEAD", remote_oid, cwd=root)
    behind = int(_git_text("rev-list", "--count", f"{base}..{remote_oid}", cwd=root))
    ahead = int(_git_text("rev-list", "--count", f"{base}..HEAD", cwd=root))

    print(f"branch: {current_branch}")
    print(f"target: {remote}/{branch} ({_short_sha(root, remote_oid)})")
    print(f"divergence: ahead {ahead}, behind {behind}")

    local_info = _exact_ref_info(root, "refs/heads", local_ref)
    if local_info is not None:
        local_oid, _ = local_info
        counts = _git_text("rev-list", "--left-right", "--count", f"{local_oid}...{remote_oid}", cwd=root).split()
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
        _git_nul_lines("diff", "--no-renames", "--name-only", "-z", f"{base}..{remote_oid}", "--", cwd=root)
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


def _github_repo_slug(root: Path, remote: str) -> str | None:
    url = _git_text("remote", "get-url", remote, cwd=root)
    for pattern in _GITHUB_URL_PATTERNS:
        matched = pattern.fullmatch(url)
        if matched is not None:
            return matched.group(1)
    return None


def _verify_merged_pr(
    root: Path,
    pr: str,
    target_branch: str,
    target_oid: str,
    remote: str,
    base_branch: str,
    remote_base_oid: str,
) -> str:
    if not _PR_NUMBER_RE.fullmatch(pr):
        raise GitError(f"invalid PR number {pr!r}; pass a positive numeric GitHub PR number")

    repo_slug = _github_repo_slug(root, remote)
    cmd = ["gh", "pr", "view"]
    if repo_slug is not None:
        cmd.extend(["--repo", repo_slug])
    cmd.extend(["--json", "state,headRefName,headRefOid,baseRefName,mergeCommit", "--", pr])

    env = {k: v for k, v in os.environ.items() if k != "GH_REPO"}
    try:
        probe = subprocess.run(
            cmd,
            cwd=root,
            env=env,
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
    if not isinstance(data, dict):
        raise GitError(f"unexpected JSON payload from gh pr view {pr}: expected object")
    if data.get("state") != "MERGED":
        raise GitError(f"PR #{pr} state is {data.get('state')!r}, expected 'MERGED'")
    if data.get("headRefName") != target_branch:
        raise GitError(f"PR #{pr} head branch is {data.get('headRefName')!r}, expected {target_branch!r}")
    if data.get("baseRefName") != base_branch:
        raise GitError(f"PR #{pr} base branch is {data.get('baseRefName')!r}, expected {base_branch!r}")
    if data.get("headRefOid") != target_oid:
        raise GitError(
            f"PR #{pr} merged head {str(data.get('headRefOid', ''))[:9]} does not match local {target_branch} ({target_oid[:9]}); refusing deletion"
        )
    merge_commit = data.get("mergeCommit")
    merge_oid = merge_commit.get("oid") if isinstance(merge_commit, dict) else None
    if not isinstance(merge_oid, str) or not _OID_RE.fullmatch(merge_oid):
        raise GitError(f"PR #{pr} did not report a valid mergeCommit.oid; refusing deletion")
    has_merge_commit = _git("cat-file", "-e", f"{merge_oid}^{{commit}}", cwd=root, check=False).returncode == 0
    if not has_merge_commit or not _is_ancestor(root, merge_oid, remote_base_oid):
        raise GitError(
            f"PR #{pr} merge commit {merge_oid[:9]} is not in {remote}/{base_branch}; fetch {remote}/{base_branch} or verify target remote"
        )
    return f"PR #{pr} merged at {target_oid[:9]} (merge {merge_oid[:9]})"


def _validate_target_worktree(
    all_worktrees: list[WorktreeEntry], wt: WorktreeEntry, local_ref: str, target_oid: str
) -> None:
    if wt.is_primary or (wt.path / ".git").is_dir():
        raise GitError(f"refusing to remove primary worktree {wt.path}")
    if wt.locked:
        raise GitError(f"worktree {wt.path} is locked; refusing removal")
    if wt.prunable or not wt.path.is_dir():
        raise GitError(f"worktree {wt.path} is missing/prunable; inspect before retiring {local_ref}")
    cwd_real = Path.cwd().resolve()
    wt_real = wt.path.resolve()
    if cwd_real == wt_real or wt_real in cwd_real.parents:
        raise GitError(f"current working directory is inside target worktree {wt.path}; run from outside it")
    for other in all_worktrees:
        if other.path == wt.path or other.prunable or not other.path.exists():
            continue
        other_real = other.path.resolve()
        if wt_real in other_real.parents:
            raise GitError(f"worktree {wt.path} contains nested worktree {other.path}; refusing removal")
    head_sym = _git("symbolic-ref", "--quiet", "HEAD", cwd=wt.path, check=False)
    if head_sym.returncode != 0 or os.fsdecode(head_sym.stdout).rstrip("\r\n") != local_ref:
        raise GitError(f"worktree {wt.path} is no longer checked out on {local_ref}; refusing removal")
    head_oid = _git_text("rev-parse", "HEAD", cwd=wt.path)
    if head_oid != target_oid:
        raise GitError(f"worktree {wt.path} HEAD moved during retirement; retry after checking status")
    markers = _in_progress_markers(wt.path)
    if markers:
        raise GitError(f"worktree {wt.path} has an in-progress git operation ({', '.join(markers)}); refusing removal")
    if _git("status", "--porcelain", "--untracked-files=normal", cwd=wt.path).stdout.strip():
        raise GitError(f"worktree {wt.path} has uncommitted or untracked changes; refusing removal")


def _check_branch_not_rebasing_elsewhere(all_worktrees: list[WorktreeEntry], local_ref: str) -> None:
    for entry in all_worktrees:
        if entry.prunable or not entry.path.is_dir():
            continue
        if _rebase_head_name(entry.path) == local_ref:
            raise GitError(f"{local_ref} has an in-progress rebase at {entry.path}; refusing retirement")


def cmd_retire(
    root: Path,
    target_branch: str,
    remote: str,
    base_branch: str,
    default_branch: str,
    *,
    pr: str | None,
    fetch: bool,
) -> int:
    if target_branch in {default_branch, base_branch}:
        raise GitError(f"refusing to retire protected branch {target_branch!r}")

    local_ref = f"refs/heads/{target_branch}"
    local_info = _exact_ref_info(root, "refs/heads", local_ref)
    if local_info is None:
        raise GitError(f"local branch {target_branch!r} does not exist")
    target_oid, local_symref = local_info
    if local_symref:
        raise GitError(f"local branch {target_branch!r} is a symbolic ref ({local_symref}); refusing retirement")
    target_short = _short_sha(root, target_oid)

    all_worktrees = _list_worktrees(root)
    _check_branch_not_rebasing_elsewhere(all_worktrees, local_ref)
    matches = [entry for entry in all_worktrees if entry.branch_ref == local_ref]
    if len(matches) > 1:
        paths = ", ".join(os.fspath(entry.path) for entry in matches)
        raise GitError(f"branch {target_branch!r} is checked out in multiple worktrees ({paths}); reconcile manually")
    wt = matches[0] if matches else None
    if wt is not None:
        _validate_target_worktree(all_worktrees, wt, local_ref, target_oid)

    if fetch:
        _fetch_remote_branch(root, remote, base_branch)

    remote_base_ref = f"refs/remotes/{remote}/{base_branch}"
    remote_base_info = _exact_ref_info(root, f"refs/remotes/{remote}", remote_base_ref)
    if remote_base_info is None:
        raise GitError(
            f"missing {remote}/{base_branch}; run without --no-fetch or fetch {remote}/{base_branch} first"
        )
    remote_base_oid, _ = remote_base_info

    is_ancestor = _is_ancestor(root, target_oid, remote_base_oid)
    if is_ancestor:
        reason = f"ancestor of {remote}/{base_branch}"
    elif pr is not None:
        reason = _verify_merged_pr(root, pr, target_branch, target_oid, remote, base_branch, remote_base_oid)
    else:
        raise GitError(
            f"{target_branch} ({target_short}) is not an ancestor of {remote}/{base_branch}; pass --pr <number> to verify a GitHub squash-merge"
        )

    # Re-validate after network calls (`git fetch` / `gh pr view`) to close TOCTOU windows.
    fresh_info = _exact_ref_info(root, "refs/heads", local_ref)
    if fresh_info is None or fresh_info != (target_oid, ""):
        raise GitError(f"local branch {target_branch!r} changed during retirement; retry after checking status")
    fresh_worktrees = _list_worktrees(root)
    _check_branch_not_rebasing_elsewhere(fresh_worktrees, local_ref)
    fresh_matches = [entry for entry in fresh_worktrees if entry.branch_ref == local_ref]
    if len(fresh_matches) > 1:
        paths = ", ".join(os.fspath(entry.path) for entry in fresh_matches)
        raise GitError(f"branch {target_branch!r} is checked out in multiple worktrees ({paths}); reconcile manually")
    fresh_wt = fresh_matches[0] if fresh_matches else None
    if fresh_wt is not None:
        if wt is None or fresh_wt.path.resolve() != wt.path.resolve():
            raise GitError(
                f"branch {target_branch!r} was checked out at {fresh_wt.path} during retirement; refusing removal"
            )
        _validate_target_worktree(fresh_worktrees, fresh_wt, local_ref, target_oid)
        _git("-c", "status.showUntrackedFiles=normal", "worktree", "remove", "--", os.fspath(fresh_wt.path), cwd=root)
        print(f"removed worktree: {fresh_wt.path}")

    _git("branch", "-D", "--", target_branch, cwd=root)
    print(f"deleted local branch: {target_branch} ({target_short}; {reason})")

    remote_branch_ref = f"refs/remotes/{remote}/{target_branch}"
    if fetch and _exact_ref_info(root, f"refs/remotes/{remote}", remote_branch_ref) is not None:
        remote_probe = _git("ls-remote", "--heads", remote, f"refs/heads/{target_branch}", cwd=root, check=False)
        if remote_probe.returncode == 0 and not remote_probe.stdout.strip():
            _git("update-ref", "--no-deref", "-d", remote_branch_ref, cwd=root)
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
    retire_parser.add_argument("--branch", default=None, help="Target base branch (default: remote HEAD or main)")
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
        return cmd_retire(root, args.target_branch, args.remote, branch, default_branch, pr=args.pr, fetch=not args.no_fetch)
    except GitError as exc:
        print(f"worktree-sync: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
