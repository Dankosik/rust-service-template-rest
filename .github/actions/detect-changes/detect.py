#!/usr/bin/env python3
"""Classify changed paths into ~12 CI surface groups (replaces 35-surface classifier)."""

from __future__ import annotations

import argparse
import fnmatch
import os
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path

SURFACES: dict[str, tuple[str, ...]] = {
    "rust": (
        "crates/**", "test/**", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "rustfmt.toml", "clippy.toml", "vendor/**", ".cargo/**", ".config/nextest.toml",
        ".sqlx/**", "env/config/**", "Makefile",
    ),
    "deps": (
        "Cargo.toml", "Cargo.lock", "deny.toml", "rust-toolchain.toml", "vendor/**",
        "tools/grpc-codegen/**", "tools/versions.env", "scripts/check-dependency-drift.py",
    ),
    "openapi": (
        "api/openapi/**", "crates/service/**", "crates/infra-http/**", "crates/service-failure/**",
        ".redocly.yaml", "scripts/check-openapi-idempotency.py", "Makefile",
    ),
    "grpc": (
        "api/proto/**", "buf.yaml", "crates/grpc-contracts/**", "tools/grpc-codegen/**",
        "scripts/grpc.sh", "Makefile",
    ),
    "migrations": (
        "migrations/**", "crates/migrate/**", "scripts/migration-check.py",
        "scripts/migration-validate.sh", "test/fixtures/migrations/**", "Makefile",
    ),
    "integration": (
        "crates/infra-postgres/**", "crates/infra-jobs/**", "crates/infra-idempotency-store/**",
        "crates/infra-webhooks/**", "crates/infra-messaging/**", "crates/infra-cache/**",
        "crates/infra-object-storage/**", "crates/service/**", "crates/config/**",
        "migrations/**", ".sqlx/**", "test/**", "env/**", "Cargo.toml", "Cargo.lock",
        "rust-toolchain.toml", "vendor/**", "scripts/integration.sh", "scripts/sqlx-prepare.sh",
        "scripts/messaging-go-compat.sh", "Makefile",
    ),
    "oauth": (
        "crates/infra-oauth2-client-credentials/**", "crates/infra-outbound-http/**",
        "crates/config/**", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "tools/versions.env", "scripts/integration.sh", "Makefile",
    ),
    "docker": (
        "build/docker/**", ".dockerignore", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "tools/versions.env", "LICENSE", "deny.toml", "scripts/image.sh", "migrations/**",
        ".github/actions/publish-image/**", "Makefile",
    ),
    "runtime_progress": (
        "crates/service/examples/runtime_progress.rs", "crates/service/tests/runtime_progress.rs",
        "build/docker/runtime-progress.Dockerfile", "scripts/runtime-progress.sh",
        "crates/service/**", "crates/infra-http-transport/**", "crates/infra-grpc/**",
        "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
    ),
    "repo_lint": (
        "*.md", "docs/**", ".agents/**", ".github/**", "scripts/**", "template/**",
        "env/monitoring/**", "quality/architecture.json", "tools/versions.env",
        ".typos.toml", "ruff.toml", "Makefile", "crates/**", "Cargo.toml",
        ".config/wt.toml", ".worktreeinclude",
    ),
    "template": (
        "template/**", "Makefile", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "tools/versions.env", "build/docker/**", ".github/**", "scripts/**", "crates/**",
        "test/**", "env/**", ".railway/**", "README.md", "AGENTS.md", ".agents/**",
        ".config/wt.toml", ".worktreeinclude", "docs/worktrees.md",
    ),
    "codeql_actions": (".github/**",),
    "codeql_rust": (
        "crates/**", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "vendor/**",
        ".github/workflows/codeql.yml", ".github/codeql/**", ".github/actions/detect-changes/**",
    ),
}


def _match(path: str, pattern: str) -> bool:
    if pattern.endswith("/**"):
        prefix = pattern[:-3]
        return path == prefix or path.startswith(prefix + "/")
    return fnmatch.fnmatch(path, pattern)


def _applicable(repo: Path) -> dict[str, bool]:
    has_postgres = (repo / "crates/infra-postgres").is_dir()
    has_grpc = (repo / "crates/grpc-contracts").is_dir()
    has_oauth = (repo / "crates/infra-oauth2-client-credentials").is_dir()
    has_rt = (repo / "scripts/runtime-progress.sh").is_file()
    has_int = (repo / "env/docker-compose.yml").is_file()
    has_tpl = (repo / "template/init.py").is_file()
    return {
        "rust": True,
        "deps": True,
        "openapi": True,
        "grpc": has_grpc,
        "migrations": has_postgres,
        "integration": has_int,
        "oauth": has_oauth,
        "docker": True,
        "runtime_progress": has_rt,
        "repo_lint": True,
        "template": has_tpl,
        "codeql_actions": True,
        "codeql_rust": True,
    }


def classify(repo: Path, files: Sequence[str] | None) -> dict[str, bool]:
    allowed = _applicable(repo)
    if files is None:
        return dict(allowed)
    result = dict.fromkeys(SURFACES, False)
    for path in files:
        for surface, patterns in SURFACES.items():
            if allowed[surface] and not result[surface] and any(_match(path, pat) for pat in patterns):
                result[surface] = True
    return result


def diff_files(repo: Path, base: str) -> list[str]:
    res = subprocess.run(
        ["git", "-C", os.fspath(repo), "diff", "--name-only", "-z", "--no-renames", base, "HEAD"],
        capture_output=True,
        check=False,
    )
    if res.returncode != 0:
        raise RuntimeError(res.stderr.decode("utf-8", errors="replace").strip() or f"git diff {base}..HEAD failed")
    return [raw.decode("utf-8") for raw in res.stdout.split(b"\0") if raw]


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--all", action="store_true")
    group.add_argument("--base", metavar="SHA")
    group.add_argument("--files", nargs="*")
    args = parser.parse_args(argv)
    try:
        repo = args.repo.resolve()
        if args.all:
            selected = classify(repo, None)
        elif args.base:
            selected = classify(repo, diff_files(repo, args.base))
        else:
            selected = classify(repo, args.files)
    except RuntimeError as exc:
        print(f"detect-changes: {exc}", file=sys.stderr)
        return 2
    for key, active in selected.items():
        print(f"{key}={'true' if active else 'false'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
