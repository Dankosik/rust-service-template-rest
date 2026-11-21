#!/usr/bin/env python3
"""Classify changed paths into CI surface groups."""

from __future__ import annotations

import argparse
import fnmatch
import os
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path

_INTEGRATION_SHARED: tuple[str, ...] = (
    "crates/config/**", "crates/service/**", "env/docker-compose.yml",
    "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
    "scripts/integration.py", "scripts/lib.py", "justfile", "mise.toml", "mise.lock",
)

SURFACES: dict[str, tuple[str, ...]] = {
    "rust": (
        "crates/**", "test/**", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "rustfmt.toml", "clippy.toml", "vendor/**", ".cargo/**", ".config/nextest.toml",
        ".sqlx/**", "env/config/**", "quality/architecture.json",
        "scripts/checks/architecture.py", "scripts/checks/vendor.py",
        "scripts/checks/rust_selection.py", "scripts/checks/test_budget.py", "scripts/lib.py",
        "justfile", "mise.toml", "mise.lock", ".mise/**",
    ),
    "deps": (
        "Cargo.toml", "Cargo.lock", "deny.toml", "rust-toolchain.toml", "vendor/**",
        "tools/grpc-codegen/**", "mise.toml", "mise.lock",
        "scripts/checks/vendor.py", "scripts/lib.py",
    ),
    "profiling": (
        "vendor/hotpath/**", "Cargo.toml", "rust-toolchain.toml", "justfile",
    ),
    "vendor_transport": (
        "vendor/**", "Cargo.toml", "rust-toolchain.toml",
        "scripts/checks/vendor.py", "scripts/lib.py", "justfile",
    ),
    "openapi": (
        "api/openapi/**", "crates/service/**", "crates/infra-http/**", "crates/service-failure/**",
        ".redocly.yaml", "scripts/checks/openapi_idempotency.py", "justfile", "mise.toml", "mise.lock", ".mise/**",
    ),
    "grpc": (
        "api/proto/**", "buf.yaml", "crates/grpc-contracts/**", "tools/grpc-codegen/**",
        "scripts/grpc.py", "scripts/lib.py", "justfile", "mise.toml", "mise.lock",
    ),
    "migrations": (
        "migrations/**", "crates/migrate/**", "scripts/checks/migrations.py",
        "scripts/image.py", "scripts/lib.py", "test/fixtures/migrations/**",
        "justfile", "mise.toml", "mise.lock",
    ),
    "integration_db": (
        "crates/infra-postgres/**", "crates/infra-jobs/**", "crates/infra-idempotency-store/**",
        "crates/infra-webhooks/**", "crates/jobs-worker/**", "crates/migrate/**",
        "crates/infra-messaging/**", "crates/domain-events/**", "crates/nats-test-support/**",
        "crates/infra-http/**", "crates/infra-http-transport/**", "crates/infra-grpc/**",
        "crates/infra-grpc-core/**",
        "crates/infra-outbound-http/**", "crates/infra-outbound-grpc/**", "crates/grpc-contracts/**",
        "crates/infra-bearerauthn/**", "crates/health/**", "crates/operation-context/**",
        "crates/shared-rules/**", "crates/service-failure/**", "crates/json-payload/**",
        "crates/infra-telemetry/**", "crates/observe-test-support/**", "crates/tls-test-support/**",
        "migrations/**", ".sqlx/**", "env/pgbouncer/**", "env/nats/**",
        "test/Cargo.toml", "test/src/**", "test/fixtures/**",
        "test/tests/postgres.rs", "test/tests/postgres/**", "test/tests/jobs/**",
        "test/tests/http_idempotency/**", "test/tests/webhooks/**",
        "test/tests/messaging_outbox.rs", "test/tests/messaging_outbox/**",
        "test/tests/messaging_recovery.rs",
        "vendor/sqlx-core/**", "vendor/sqlx-postgres/**", "vendor/async-nats/**",
        "vendor/hyper-util/**", "vendor/tonic/**",
        *_INTEGRATION_SHARED,
    ),
    "sqlx_check": (
        "crates/infra-postgres/**", "crates/infra-jobs/**", "crates/infra-idempotency-store/**",
        "crates/infra-webhooks/**", "crates/migrate/**",
        "migrations/**", ".sqlx/**", "vendor/sqlx-core/**", "vendor/sqlx-postgres/**",
        "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "scripts/integration.py", "scripts/lib.py", "justfile", "mise.toml", "mise.lock",
    ),
    "integration_cache": (
        "crates/infra-cache/**", "crates/health/**", "crates/operation-context/**",
        "crates/shared-rules/**", "crates/infra-telemetry/**", "crates/observe-test-support/**",
        "crates/tls-test-support/**",
        *_INTEGRATION_SHARED,
    ),
    "integration_object_storage": (
        "crates/infra-object-storage/**", "crates/health/**", "crates/operation-context/**",
        "crates/shared-rules/**", "crates/infra-telemetry/**", "crates/observe-test-support/**",
        "crates/tls-test-support/**",
        "vendor/aws-smithy-http-client/**", "vendor/hyper-util/**",
        *_INTEGRATION_SHARED,
    ),
    "integration_messaging": (
        "crates/infra-messaging/**", "crates/domain-events/**", "crates/nats-test-support/**",
        "crates/jobs-worker/**", "crates/health/**", "crates/operation-context/**",
        "crates/shared-rules/**", "crates/json-payload/**", "crates/infra-telemetry/**",
        "crates/observe-test-support/**", "crates/tls-test-support/**",
        "env/nats/**", "vendor/async-nats/**",
        *_INTEGRATION_SHARED,
    ),
    "oauth": (
        "crates/infra-oauth2-client-credentials/**", "crates/infra-outbound-http/**",
        "crates/observe-test-support/**", "crates/tls-test-support/**",
        "crates/config/**", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "env/docker-compose.yml", "scripts/integration.py", "scripts/lib.py",
        "justfile", "mise.toml", "mise.lock",
    ),
    "docker": (
        "build/docker/**", ".dockerignore", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "mise.toml", "mise.lock", "LICENSE", "deny.toml", "scripts/image.py", "scripts/lib.py",
        "migrations/**", ".github/actions/publish-image/**", "justfile",
    ),
    "runtime_progress": (
        "crates/service/examples/runtime_progress.rs", "crates/service/tests/runtime_progress.rs",
        "build/docker/runtime-progress.Dockerfile", "template/runtime_progress.py",
        "crates/service/**", "crates/infra-http-transport/**", "crates/infra-grpc/**",
        "crates/infra-grpc-core/**",
        "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
    ),
    "repo_lint": (
        "*.md", "*.sh", "*.py", "docs/**", ".agents/**", ".github/**", "scripts/**", "template/**",
        "env/monitoring/**", "quality/architecture.json", "mise.toml", "mise.lock", ".mise/**",
        ".typos.toml", "ruff.toml", "justfile",
        ".config/wt.toml", ".worktreeinclude",
    ),
    "template": (
        "template/**", "justfile", "mise.toml", "mise.lock", ".mise/**", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "build/docker/**", ".github/**", "scripts/**", "crates/**",
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
    has_cache = (repo / "crates/infra-cache").is_dir()
    has_object_storage = (repo / "crates/infra-object-storage").is_dir()
    has_messaging = (repo / "crates/infra-messaging").is_dir()
    has_grpc = (repo / "crates/grpc-contracts").is_dir()
    has_oauth = (repo / "crates/infra-oauth2-client-credentials").is_dir()
    has_rt = (repo / "template/runtime_progress.py").is_file()
    has_int = has_postgres or has_cache or has_object_storage or has_messaging
    has_tpl = (repo / "template/init.py").is_file()
    return {
        "rust": True,
        "deps": True,
        "profiling": True,
        "vendor_transport": True,
        "openapi": True,
        "grpc": has_grpc,
        "migrations": has_postgres,
        "integration": has_int,
        "integration_db": has_postgres,
        "sqlx_check": has_postgres,
        "integration_cache": has_cache,
        "integration_object_storage": has_object_storage,
        "integration_messaging": has_messaging,
        "oauth": has_oauth,
        "docker": True,
        "runtime_progress": has_rt,
        "repo_lint": True,
        "template": has_tpl,
        "codeql_actions": True,
        "codeql_rust": True,
    }


_INTEGRATION_SUBSURFACES: tuple[str, ...] = (
    "integration_db",
    "sqlx_check",
    "integration_cache",
    "integration_object_storage",
    "integration_messaging",
)


def classify(repo: Path, files: Sequence[str] | None) -> dict[str, bool]:
    allowed = _applicable(repo)
    if files is None:
        return dict(allowed)
    result = dict.fromkeys(allowed, False)
    for path in files:
        for surface, patterns in SURFACES.items():
            if allowed[surface] and not result[surface] and any(_match(path, pat) for pat in patterns):
                result[surface] = True
    result["integration"] = allowed["integration"] and any(result[key] for key in _INTEGRATION_SUBSURFACES)
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
