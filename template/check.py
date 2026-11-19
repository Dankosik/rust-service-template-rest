#!/usr/bin/env python3
"""Verify canonical template profile projections, Cargo builds/tests, and release image artifacts."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
import tomllib
from collections.abc import Sequence
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]
if os.fspath(ROOT / "template") not in sys.path:
    sys.path.insert(0, os.fspath(ROOT / "template"))

import init as template_init  # noqa: E402

CANONICAL_PROFILES: dict[str, dict[str, str]] = {
    "minimal": {},
    "postgres": {"database": "postgres"},
    "postgres-full": {
        "database": "postgres",
        "authn": "oidc-jwt",
        "outbound_http": "bounded",
        "http_idempotency": "postgres",
        "jobs": "postgres",
        "webhooks": "durable",
        "inbound_webhooks": "standard-webhooks",
    },
    "authn-jwt": {"authn": "oidc-jwt"},
    "authn-introspection": {"authn": "oidc-introspection"},
    "outbound-oauth": {"outbound_auth": "oauth2-client-credentials"},
    "grpc-service-auth": {"grpc": "enabled", "authn": "oidc-jwt", "service_auth": "jwt"},
    "messaging-outbox": {"database": "postgres", "jobs": "postgres", "messaging": "nats-jetstream", "outbox": "postgres"},
    "cache-s3": {"cache": "redis", "object_storage": "s3"},
    "runtime-progress": {
        "grpc": "enabled",
        "authn": "oidc-jwt",
        "messaging": "nats-jetstream",
        "object_storage": "s3",
    },
    "full": {
        "database": "postgres",
        "authn": "oidc-jwt",
        "outbound_http": "bounded",
        "outbound_auth": "oauth2-client-credentials",
        "grpc": "enabled",
        "http_idempotency": "postgres",
        "jobs": "postgres",
        "messaging": "nats-jetstream",
        "outbox": "postgres",
        "webhooks": "durable",
        "inbound_webhooks": "standard-webhooks",
        "cache": "redis",
        "object_storage": "s3",
        "service_auth": "jwt",
    },
}

DEFAULT_ARTIFACTS = ("minimal", "postgres-full")


def _inputs_for(name: str, harness: str = "core") -> template_init.InitInputs:
    slug = f"demo-{name}"
    return template_init.InitInputs(
        service_name=slug,
        repository=f"https://github.com/example/{slug}",
        description=f"Verification projection for {name}",
        codeowner="@example/platform",
        agent_harness=harness,
        **CANONICAL_PROFILES[name],
    )


def _run(cmd: Sequence[str], *, cwd: Path, env: dict[str, str]) -> None:
    res = subprocess.run(cmd, cwd=cwd, env=env, check=False)
    if res.returncode != 0:
        raise RuntimeError(f"command failed (exit {res.returncode}): {' '.join(cmd)}")


def check_projections(names: Sequence[str]) -> None:
    harnesses = {"minimal": "claude", "postgres": "codex", "full": "all"}
    for name in names:
        harness = harnesses.get(name, "core")
        inputs = _inputs_for(name, harness=harness)
        with tempfile.TemporaryDirectory(prefix=f"proj-{name}-") as tmp:
            staged = Path(tmp) / "repo"
            template_init.project_tree(ROOT, staged, inputs, run_cargo=False, from_git_head=False)
            assert not (staged / "template").exists(), f"{name}: template/ survived"
            ws = tomllib.loads((staged / "Cargo.toml").read_text(encoding="utf-8"))
            svc = tomllib.loads((staged / "crates/service/Cargo.toml").read_text(encoding="utf-8"))
            assert ws["workspace"]["package"]["repository"] == inputs.repository
            assert svc["package"]["name"] == inputs.service_name
            _run(
                [sys.executable, "-B", "scripts/harness-sync.py", "--check", "--repo", os.fspath(staged), "--require-materialized"],
                cwd=staged,
                env=os.environ.copy(),
            )
        print(f"projection ok: {name} (harness={harness})")


def _tools_root() -> str:
    if existing := os.environ.get("TOOLS_ROOT"):
        return existing
    res = subprocess.run(
        ["git", "-C", os.fspath(ROOT), "rev-parse", "--path-format=absolute", "--git-common-dir"],
        text=True,
        capture_output=True,
        check=False,
    )
    common = Path(res.stdout.strip()) if res.returncode == 0 and res.stdout.strip() else ROOT / ".git"
    return os.fspath((common / "tools").resolve())


def check_matrix(names: Sequence[str]) -> None:
    shared_target = Path(os.environ.get("CARGO_TARGET_DIR") or (ROOT / "target/template-matrix")).resolve()
    shared_target.mkdir(parents=True, exist_ok=True)
    tools_root = _tools_root()
    for name in names:
        inputs = _inputs_for(name, harness="core")
        with tempfile.TemporaryDirectory(prefix=f"matrix-{name}-") as tmp:
            staged = Path(tmp) / "repo"
            env = {**os.environ, "CARGO_TARGET_DIR": os.fspath(shared_target), "TOOLS_ROOT": tools_root}
            template_init.project_tree(ROOT, staged, inputs, run_cargo=True, from_git_head=False)
            _run(["cargo", "fmt", "--all", "--check"], cwd=staged, env=env)
            _run(["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"], cwd=staged, env=env)
            _run(["make", "test"], cwd=staged, env=env)
            _run([sys.executable, "-B", "scripts/check-architecture.py", "--repo", os.fspath(staged)], cwd=staged, env=env)
        print(f"matrix ok: {name}")


def check_artifacts(names: Sequence[str]) -> None:
    for name in names:
        inputs = _inputs_for(name, harness="core")
        with tempfile.TemporaryDirectory(prefix=f"artifact-{name}-") as tmp:
            staged = Path(tmp) / "repo"
            env = os.environ.copy()
            template_init.project_tree(ROOT, staged, inputs, run_cargo=True, from_git_head=False)
            tag = f"{inputs.service_name}:template-check"
            try:
                _run(["bash", "scripts/image.sh", "build", tag], cwd=staged, env={**env, "VCS_REF": "template-check"})
                _run(["bash", "scripts/image.sh", "check", tag, "template-check"], cwd=staged, env=env)
                _run(["bash", "scripts/image.sh", "scan", tag], cwd=staged, env=env)
            finally:
                subprocess.run(["docker", "image", "rm", "-f", tag], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        print(f"artifact ok: {name}")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--projections", action="store_true", help="fast projection-only check (no Cargo)")
    mode.add_argument("--matrix", action="store_true", help="build, clippy, test, and architecture check")
    mode.add_argument("--artifacts", action="store_true", help="build and smoke-test release images")
    parser.add_argument("--profiles", nargs="*", choices=sorted(CANONICAL_PROFILES), default=None)
    args = parser.parse_args(argv)

    try:
        if args.projections:
            check_projections(args.profiles or list(CANONICAL_PROFILES))
        elif args.matrix:
            check_matrix(args.profiles or list(CANONICAL_PROFILES))
        elif args.artifacts:
            check_artifacts(args.profiles or list(DEFAULT_ARTIFACTS))
        return 0
    except (RuntimeError, AssertionError, template_init.InitError) as exc:
        print(f"template-check: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
