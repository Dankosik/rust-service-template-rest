#!/usr/bin/env python3
"""Select affected workspace crates and reverse dependents from one cargo metadata query."""

from __future__ import annotations

import argparse
from collections import deque
import json
import os
import stat
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path

try:
    sys.dont_write_bytecode = True
    from . import changed_paths as _changed_paths
except ImportError:  # pragma: no cover - direct script or sys.path=scripts/verify import
    if str(Path(__file__).resolve().parent) not in sys.path:
        sys.path.insert(0, str(Path(__file__).resolve().parent))
    import changed_paths as _changed_paths


_WORKSPACE_WIDE_FILES = frozenset(
    {
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo/config.toml",
        ".config/nextest.toml",
        "clippy.toml",
    }
)
_WORKSPACE_WIDE_PREFIXES = (
    "vendor/",
    ".sqlx/",
    "env/config/",
)


def _normalize_path(raw: str) -> str:
    cleaned = raw.strip("\r\n")
    while cleaned.startswith("./"):
        cleaned = cleaned[2:]
    return cleaned


def _stdin_is_piped() -> bool:
    try:
        mode = os.fstat(sys.stdin.fileno()).st_mode
    except OSError:
        return False
    return stat.S_ISFIFO(mode) or stat.S_ISREG(mode)


def select_affected_crates(repo: Path | str, files: Sequence[str]) -> tuple[str, list[str]]:
    repo_path = Path(repo).resolve()
    normalized = sorted({p for raw in files if (p := _normalize_path(raw))})
    if not normalized:
        return ("none", [])

    production_dirs: set[str] = set()
    test_only_dirs: set[str] = set()

    for path in normalized:
        if path in _WORKSPACE_WIDE_FILES:
            return ("workspace", [])
        if any(path.startswith(prefix) for prefix in _WORKSPACE_WIDE_PREFIXES):
            return ("workspace", [])
        if path.startswith("test/"):
            if path.endswith(".md"):
                continue
            return ("workspace", [])
        if path.startswith("crates/"):
            remainder = path.removeprefix("crates/")
            if "/" not in remainder:
                if path.endswith(".rs"):
                    return ("workspace", [])
                continue
            crate_slug, inner = remainder.split("/", 1)
            if not crate_slug or not inner:
                continue
            if path.endswith(".md"):
                continue
            crate_dir = f"crates/{crate_slug}"
            if inner.startswith("tests/"):
                test_only_dirs.add(crate_dir)
            else:
                production_dirs.add(crate_dir)
            continue
        if path.endswith(("/Cargo.toml", "/Cargo.lock")) or path.endswith(".rs"):
            return ("workspace", [])

    touched_dirs = production_dirs | test_only_dirs
    if not touched_dirs:
        return ("none", [])

    cargo_bin = os.environ.get("CARGO", "cargo")
    try:
        result = subprocess.run(
            [cargo_bin, "metadata", "--no-deps", "--format-version", "1", "--locked"],
            cwd=repo_path,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError:
        return ("workspace", [])

    if result.returncode != 0:
        return ("workspace", [])

    try:
        metadata = json.loads(result.stdout)
    except json.JSONDecodeError:
        return ("workspace", [])

    if not isinstance(metadata, dict):
        return ("workspace", [])

    raw_members = metadata.get("workspace_members")
    member_ids = set(raw_members) if isinstance(raw_members, list) else set()
    packages = metadata.get("packages")
    if not isinstance(packages, list):
        return ("workspace", [])

    workspace_packages: list[dict[str, object]] = []
    dir_to_pkg: dict[str, str] = {}
    workspace_pkg_names: set[str] = set()

    for pkg in packages:
        if not isinstance(pkg, dict):
            continue
        pkg_id = pkg.get("id")
        if member_ids and pkg_id not in member_ids:
            continue
        name = pkg.get("name")
        manifest_raw = pkg.get("manifest_path")
        if not isinstance(name, str) or not name or not isinstance(manifest_raw, str):
            continue
        try:
            rel_dir = Path(manifest_raw).resolve().parent.relative_to(repo_path).as_posix()
        except ValueError:
            continue
        dir_to_pkg[rel_dir] = name
        workspace_pkg_names.add(name)
        workspace_packages.append(pkg)

    total = len(workspace_pkg_names)
    if total == 0:
        return ("workspace", [])

    if any(crate_dir not in dir_to_pkg for crate_dir in touched_dirs):
        return ("workspace", [])

    reverse_deps: dict[str, set[str]] = {name: set() for name in workspace_pkg_names}
    for pkg in workspace_packages:
        dependent = pkg["name"]
        assert isinstance(dependent, str)
        deps = pkg.get("dependencies", [])
        if not isinstance(deps, list):
            continue
        for dep in deps:
            if not isinstance(dep, dict):
                continue
            dep_name = dep.get("name")
            if isinstance(dep_name, str) and dep_name in workspace_pkg_names and dep_name != dependent:
                reverse_deps[dep_name].add(dependent)

    selected: set[str] = {dir_to_pkg[d] for d in test_only_dirs}
    queue: deque[str] = deque()
    for crate_dir in production_dirs:
        pkg_name = dir_to_pkg[crate_dir]
        if pkg_name not in selected:
            selected.add(pkg_name)
        queue.append(pkg_name)

    visited_prod: set[str] = set(queue)
    while queue:
        current = queue.popleft()
        for rev in reverse_deps.get(current, ()):
            selected.add(rev)
            if rev not in visited_prod:
                visited_prod.add(rev)
                queue.append(rev)

    if len(selected) >= 50 or len(selected) * 5 >= total * 4:
        return ("workspace", [])

    return ("packages", sorted(selected))


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
    parser.add_argument(
        "--format",
        choices=("lines", "space", "json"),
        default="lines",
        dest="output_format",
        help="output format",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    repo = args.repo.resolve() if args.repo is not None else _changed_paths.resolve_repo_root()
    try:
        if args.base is not None or args.union is not None or args.files is not None:
            files = _changed_paths.changed_paths(
                repo, base=args.base, union=args.union, files=args.files
            )
        elif _stdin_is_piped():
            files = [line.rstrip("\r\n") for line in sys.stdin if line.rstrip("\r\n")]
        else:
            files = _changed_paths.changed_paths(repo)
    except _changed_paths.ChangedPathsError as error:
        print(str(error), file=sys.stderr)
        return 2

    mode, crates = select_affected_crates(repo, files)
    if args.output_format == "lines":
        for crate in crates:
            print(crate)
    elif args.output_format == "space":
        if crates:
            print(" ".join(crates))
    else:
        print(
            json.dumps(
                {
                    "mode": mode,
                    "crates": crates,
                    "packages": crates,
                    "fallback": mode == "workspace",
                }
            )
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
