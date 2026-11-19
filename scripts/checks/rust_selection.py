#!/usr/bin/env python3
"""Deterministic Cargo workspace reverse-dependency selector with fail-closed fallback."""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import subprocess
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from scripts import lib

_GLOBAL_FULL_PATTERNS: tuple[str, ...] = (
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rustfmt.toml",
    "clippy.toml",
    ".cargo/**",
    "vendor/**",
    ".config/nextest.toml",
    "env/config/**",
    "quality/architecture.json",
    "scripts/checks/architecture.py",
    "scripts/checks/vendor.py",
    "scripts/checks/rust_selection.py",
    "scripts/lib.py",
    "justfile",
    "mise.toml",
    "mise.lock",
    ".mise/**",
)

_LINT_INTEGRATION_FEATURES: tuple[tuple[str, str], ...] = (
    ("integration-tests", "integration-tests/integration"),
    ("infra-messaging", "infra-messaging/integration"),
    ("infra-cache", "infra-cache/integration"),
    ("infra-object-storage", "infra-object-storage/integration"),
    ("infra-oauth2-client-credentials", "infra-oauth2-client-credentials/integration"),
)


@dataclass(frozen=True)
class SelectionPlan:
    mode: str  # "full" | "selected" | "empty"
    reason: str
    packages: tuple[str, ...]
    lint_features: tuple[str, ...]
    provenance: dict[str, tuple[str, ...]]
    all_count: int
    check_grpc_codegen: bool


def _match(path: str, pattern: str) -> bool:
    if pattern.endswith("/**"):
        prefix = pattern[:-3]
        return path == prefix or path.startswith(prefix + "/")
    return fnmatch.fnmatch(path, pattern)


def _resolve_base(repo: Path, base: str) -> str:
    res = lib.run(["git", "-C", os.fspath(repo), "merge-base", base, "HEAD"], cwd=repo, capture=True, check=False)
    merged = res.stdout.strip()
    return merged if res.returncode == 0 and merged else base


def _changed_files(repo: Path, base: str, *, working_tree: bool) -> list[str]:
    effective_base = _resolve_base(repo, base)
    diff_args = ["diff", "--name-only", "-z", "--no-renames", effective_base]
    if not working_tree:
        diff_args.append("HEAD")
    raw = lib.git(*diff_args, cwd=repo, binary=True)
    paths = {entry.decode("utf-8") for entry in raw.split(b"\0") if entry}
    if working_tree:
        untracked = lib.git("ls-files", "--others", "--exclude-standard", "-z", cwd=repo, binary=True)
        paths.update(entry.decode("utf-8") for entry in untracked.split(b"\0") if entry)
    return sorted(paths)


def _workspace_graph(repo: Path, metadata: dict[str, Any] | None = None) -> tuple[dict[str, str], dict[str, set[str]], dict[str, set[str]]]:
    meta = metadata if metadata is not None else lib.cargo_metadata(repo, no_deps=True)
    ws_ids = set(meta.get("workspace_members", []))
    repo_resolved = repo.resolve()
    pkg_dirs: dict[str, str] = {}
    pkg_features: dict[str, set[str]] = {}
    pkgs_by_name: dict[str, dict[str, Any]] = {}
    for pkg in meta.get("packages", []):
        if pkg["id"] not in ws_ids:
            continue
        name = pkg["name"]
        manifest_dir = Path(pkg["manifest_path"]).resolve().parent
        rel_dir = manifest_dir.relative_to(repo_resolved).as_posix()
        pkg_dirs[name] = "" if rel_dir == "." else rel_dir
        pkg_features[name] = set(pkg.get("features", {}).keys())
        pkgs_by_name[name] = pkg

    rev_deps: dict[str, set[str]] = {name: set() for name in pkgs_by_name}
    for name, pkg in pkgs_by_name.items():
        for dep in pkg.get("dependencies", []):
            dep_name = dep["name"]
            if dep_name in rev_deps and dep_name != name:
                rev_deps[dep_name].add(name)
    return pkg_dirs, rev_deps, pkg_features


def _features_for(selected: Sequence[str], pkg_features: dict[str, set[str]]) -> tuple[str, ...]:
    selected_set = set(selected)
    return tuple(
        feat
        for pkg, feat in _LINT_INTEGRATION_FEATURES
        if pkg in selected_set and "integration" in pkg_features.get(pkg, set())
    )


def plan_selection(
    repo: Path,
    files: Sequence[str] | None,
    *,
    metadata: dict[str, Any] | None = None,
    force_full: bool = False,
    allow_empty: bool = False,
) -> SelectionPlan:
    pkg_dirs, rev_deps, pkg_features = _workspace_graph(repo, metadata=metadata)
    all_pkgs = tuple(sorted(pkg_dirs))
    has_grpc_codegen = (repo / "tools/grpc-codegen/Cargo.toml").is_file()

    def _full(reason: str) -> SelectionPlan:
        return SelectionPlan(
            mode="full",
            reason=reason,
            packages=all_pkgs,
            lint_features=_features_for(all_pkgs, pkg_features),
            provenance={},
            all_count=len(all_pkgs),
            check_grpc_codegen=has_grpc_codegen,
        )

    if force_full or os.environ.get("CI_RUST_SELECTION") == "full":
        return _full("env:CI_RUST_SELECTION=full" if not force_full else "flag:--all")
    if files is None:
        return _full("no-base-diff")

    service_pkg = next((name for name, rel in pkg_dirs.items() if rel == "crates/service"), None)
    dir_entries = sorted(((rel, name) for name, rel in pkg_dirs.items() if rel), key=lambda item: -len(item[0]))
    direct: dict[str, list[str]] = {}
    check_grpc_codegen = False

    for path in files:
        if any(_match(path, pat) for pat in _GLOBAL_FULL_PATTERNS):
            return _full(f"global-input:{path}")
        if path.startswith("tools/grpc-codegen/"):
            check_grpc_codegen = has_grpc_codegen
            continue
        if _match(path, "api/openapi/**") and service_pkg:
            direct.setdefault(service_pkg, []).append(f"explicit-owner:{path}")
            continue
        if _match(path, "migrations/**"):
            for target in ("migrate", "integration-tests", service_pkg):
                if target and target in pkg_dirs:
                    direct.setdefault(target, []).append(f"explicit-owner:{path}")
            continue
        if _match(path, ".sqlx/**"):
            if "infra-postgres" in pkg_dirs:
                direct.setdefault("infra-postgres", []).append(f"explicit-owner:{path}")
                continue
            return _full(f"global-input:{path}")
        if _match(path, "api/proto/**") and "grpc-contracts" in pkg_dirs:
            direct.setdefault("grpc-contracts", []).append(f"explicit-owner:{path}")
            continue

        owner = next((name for rel, name in dir_entries if path == rel or path.startswith(rel + "/")), None)
        if owner is not None:
            if Path(path).name == "Cargo.toml":
                return _full(f"manifest-change:{path}")
            direct.setdefault(owner, []).append(f"owner:{path}")
        elif path.endswith((".rs", ".toml")):
            return _full(f"unmapped-input:{path}")

    if not direct:
        if allow_empty:
            return SelectionPlan(
                mode="empty",
                reason="no-rust-changes",
                packages=(),
                lint_features=(),
                provenance={},
                all_count=len(all_pkgs),
                check_grpc_codegen=check_grpc_codegen,
            )
        return _full("no-rust-owners-fallback")

    provenance: dict[str, list[str]] = {pkg: list(reasons) for pkg, reasons in direct.items()}
    queue = sorted(direct)
    while queue:
        cur = queue.pop(0)
        for dependent in sorted(rev_deps.get(cur, set())):
            reason = f"reverse-dep:{cur}"
            if dependent not in provenance:
                provenance[dependent] = [reason]
                queue.append(dependent)
            elif reason not in provenance[dependent]:
                provenance[dependent].append(reason)

    selected = tuple(sorted(provenance))
    if len(selected) == len(all_pkgs):
        return _full("closure-covers-workspace")

    return SelectionPlan(
        mode="selected",
        reason=f"affected:{len(selected)}/{len(all_pkgs)}",
        packages=selected,
        lint_features=_features_for(selected, pkg_features),
        provenance={pkg: tuple(reasons) for pkg, reasons in sorted(provenance.items())},
        all_count=len(all_pkgs),
        check_grpc_codegen=check_grpc_codegen,
    )


def _write_summary(plan: SelectionPlan) -> None:
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if not summary_path:
        return
    lines = [
        "### Rust workspace selection",
        f"- **Mode:** `{plan.mode}` (`{plan.reason}`)",
        f"- **Crates ({len(plan.packages)}/{plan.all_count}):** "
        + (", ".join(f"`{p}`" for p in plan.packages) if plan.packages else "_none_"),
    ]
    if plan.provenance:
        lines.append("")
        lines.append("<details><summary>Selection provenance</summary>")
        lines.append("")
        for pkg, reasons in plan.provenance.items():
            lines.append(f"- `{pkg}`: {', '.join(f'`{r}`' for r in reasons[:4])}")
        lines.append("")
        lines.append("</details>")
    with Path(summary_path).open("a", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n\n")


def _run_lint(repo: Path, plan: SelectionPlan) -> None:
    if plan.mode == "empty":
        print("rust-selection: no Rust crates affected; skipping lint")
        return
    scope = ["--workspace"] if plan.mode == "full" else [arg for pkg in plan.packages for arg in ("-p", pkg)]
    feat_args = [arg for feat in plan.lint_features for arg in ("--features", feat)]
    lib.run(["cargo", "clippy", *scope, "--all-targets", "--keep-going", *feat_args, "--locked", "--", "-D", "warnings"], cwd=repo)
    rustdocflags = f"{os.environ['RUSTDOCFLAGS']} -D warnings" if os.environ.get("RUSTDOCFLAGS") else "-D warnings"
    lib.run(
        ["cargo", "doc", "--keep-going", *scope, "--no-deps", "--document-private-items", *feat_args, "--locked"],
        cwd=repo,
        env={**os.environ, "RUSTDOCFLAGS": rustdocflags},
    )
    if plan.check_grpc_codegen:
        codegen_dir = os.environ.get("CARGO_TARGET_DIR") or str(repo / "target/grpc-codegen")
        env = {**os.environ, "CARGO_TARGET_DIR": codegen_dir}
        lib.run(["cargo", "clippy", "--manifest-path", "tools/grpc-codegen/Cargo.toml", "--all-targets", "--keep-going", "--locked", "--", "-D", "warnings"], cwd=repo, env=env)
        lib.run(["cargo", "doc", "--manifest-path", "tools/grpc-codegen/Cargo.toml", "--no-deps", "--document-private-items", "--locked"], cwd=repo, env={**env, "RUSTDOCFLAGS": rustdocflags})


def _run_test(repo: Path, plan: SelectionPlan, extra_args: Sequence[str]) -> None:
    if plan.mode == "empty":
        print("rust-selection: no Rust crates affected; skipping tests")
        return
    scope = ["--workspace"] if plan.mode == "full" else [arg for pkg in plan.packages for arg in ("-p", pkg)]
    threads = os.environ.get("NEXTEST_TEST_THREADS") or str(2 * (os.cpu_count() or 2))
    lib.run(["cargo", "nextest", "run", "--no-tests=pass", f"--test-threads={threads}", "--locked", *scope, *extra_args], cwd=repo)
    lib.run(["cargo", "test", "--doc", *scope, "--no-fail-fast", "--locked"], cwd=repo)


def main(argv: Sequence[str] | None = None) -> int:
    raw_argv = list(sys.argv[1:] if argv is None else argv)
    if "--" in raw_argv:
        sep = raw_argv.index("--")
        cli_argv, extra_args = raw_argv[:sep], raw_argv[sep + 1 :]
    else:
        cli_argv, extra_args = raw_argv, []

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("plan", "lint", "test"))
    parser.add_argument("--repo", type=Path, default=lib.ROOT)
    parser.add_argument("--base", default=None)
    parser.add_argument("--working-tree", action="store_true")
    parser.add_argument("--all", action="store_true")
    parser.add_argument("--allow-empty", action="store_true")
    parser.add_argument("--summary", action="store_true")
    parser.add_argument("--files", nargs="*", default=None)
    args = parser.parse_args(cli_argv)

    def _dispatch() -> None:
        repo = args.repo.resolve()
        if args.files is not None:
            files: Sequence[str] | None = args.files
        elif args.all or not args.base:
            files = None
        else:
            try:
                files = _changed_files(repo, args.base, working_tree=args.working_tree)
            except (lib.ScriptError, subprocess.SubprocessError):
                files = None
        plan = plan_selection(repo, files, force_full=args.all, allow_empty=args.allow_empty)
        if args.summary:
            _write_summary(plan)
        if args.action == "plan":
            print(json.dumps({
                "mode": plan.mode,
                "reason": plan.reason,
                "packages": list(plan.packages),
                "lint_features": list(plan.lint_features),
                "all_count": plan.all_count,
                "check_grpc_codegen": plan.check_grpc_codegen,
                "provenance": {k: list(v) for k, v in plan.provenance.items()},
            }, indent=2))
        elif args.action == "lint":
            print(f"rust-selection: lint mode={plan.mode} ({plan.reason})")
            _run_lint(repo, plan)
        else:
            print(f"rust-selection: test mode={plan.mode} ({plan.reason})")
            _run_test(repo, plan, extra_args)

    return lib.run_cli(_dispatch)


if __name__ == "__main__":
    raise SystemExit(main())
