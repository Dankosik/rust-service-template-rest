#!/usr/bin/env python3
"""Classify changed repository paths into validation and CI surfaces."""

from __future__ import annotations

import argparse
from collections.abc import Sequence
from dataclasses import dataclass
import fnmatch
from functools import cache
import importlib.util
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
from typing import Any

sys.dont_write_bytecode = True

if str(Path(__file__).resolve().parent) not in sys.path:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
import changed_paths as _changed_paths  # noqa: E402

SELF = Path(__file__).resolve()

SURFACE_NAMES: tuple[str, ...] = (
    "monitoring_rules",
    "rust_source",
    "cargo_dependencies",
    "dependency_policy",
    "lint_config",
    "openapi",
    "tool_manifest",
    "duplication",
    "architecture",
    "quality_tooling",
    # template:begin grpc:classifier-grpc-surface
    "grpc_schema",
    # template:end grpc:classifier-grpc-surface
    # template:begin runtime-progress:classifier-runtime-progress-surface
    "runtime_progress",
    # template:end runtime-progress:classifier-runtime-progress-surface
    "github_workflows",
    "dependency_automation",
    "shell",
    "python",
    "runtime_image",
    "publication_metadata",
    "secret_scanning",
    "db_integration",
    "messaging_integration",
    "cache_integration",
    "object_storage_integration",
    "oauth_integration",
    "migrations",
    "agent_instructions",
    "documentation",
    "validation_system",
    "template_engine",
    "module_initializer",
    "initializer_source",
    "initializer_runtime",
    "initializer_artifacts",
    "no_validation_required",
)


class SurfaceConfigError(RuntimeError):
    def __init__(self, message: str, exit_code: int = 2) -> None:
        super().__init__(message)
        self.exit_code = exit_code


class UnclassifiedPathsError(ValueError):
    def __init__(self, unclassified_files: list[str], surfaces: dict[str, bool]) -> None:
        super().__init__(f"unclassified files: {','.join(unclassified_files)}")
        self.unclassified_files = unclassified_files
        self.surfaces = surfaces


@dataclass(frozen=True)
class ClassificationReport:
    surfaces: dict[str, bool]
    classified: bool
    surface_count: int
    unclassified_files: tuple[str, ...]


@cache
def _compile_pattern(pattern: str) -> re.Pattern[str]:
    return re.compile(fnmatch.translate(pattern))


def _matches(path: str, *patterns: str) -> bool:
    for pattern in patterns:
        if "*" not in pattern and "?" not in pattern and "[" not in pattern:
            if path == pattern:
                return True
        elif _compile_pattern(pattern).match(path) is not None:
            return True
    return False


@cache
def _load_state_module(module_path: Path) -> Any:
    spec = importlib.util.spec_from_file_location(
        f"_verify_template_state_{abs(hash(module_path))}", module_path
    )
    if spec is None or spec.loader is None:
        return None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _load_profiles(repo: Path) -> dict[str, str]:
    if repo.is_symlink() or not repo.is_dir():
        raise SurfaceConfigError("cannot resolve selected template profiles")
    lock_path = repo / "template.lock"
    if not lock_path.exists() and not lock_path.is_symlink():
        return {
            "database": "postgres",
            "messaging": "nats-jetstream" if (repo / "crates/infra-messaging").is_dir() else "none",
            "cache": "redis" if (repo / "crates/infra-cache").is_dir() else "none",
            "object_storage": "s3" if (repo / "crates/infra-object-storage").is_dir() else "none",
            "outbound_auth": (
                "oauth2-client-credentials"
                if (repo / "crates/infra-oauth2-client-credentials").is_dir()
                else "none"
            ),
            "outbox": (
                "postgres" if (repo / "crates/infra-messaging/src/outbox.rs").is_file() else "none"
            ),
        }

    for candidate in (repo / "scripts/template/state.py", SELF.parents[1] / "template/state.py"):
        if candidate.is_file() and not candidate.is_symlink():
            module = _load_state_module(candidate.resolve())
            if module is not None and hasattr(module, "selected_profile_map"):
                try:
                    return dict(module.selected_profile_map(repo.resolve(strict=True)))
                except Exception:
                    raise SurfaceConfigError("cannot resolve selected template profiles")
    raise SurfaceConfigError("cannot resolve selected template profiles")


def _run_service_hook(repo: Path, files: Sequence[str]) -> dict[int, list[str]]:
    legacy_hook = repo / "scripts/ci/service-changed-surfaces.sh"
    if legacy_hook.exists() or legacy_hook.is_symlink():
        raise SurfaceConfigError(
            "refusing legacy hook scripts/ci/service-changed-surfaces.sh; "
            "move to scripts/service/changed-surfaces.sh"
        )
    if not files:
        return {}
    hook_path = repo / "scripts/service/changed-surfaces.sh"
    if not hook_path.is_file() or hook_path.is_symlink():
        return {}

    script = (
        "set -euo pipefail\n"
        "service_changed_surfaces() { :; }\n"
        'source "$1"\n'
        "idx=0\n"
        "while IFS= read -r -d '' file; do\n"
        "  mark() { local s; for s in \"$@\"; do printf '%s\\t%s\\n' \"${idx}\" \"${s}\"; done; }\n"
        '  service_changed_surfaces "${file}"\n'
        "  idx=$((idx + 1))\n"
        "done\n"
    )
    payload = "".join(f"{path}\0" for path in files).encode("utf-8")
    result = subprocess.run(
        ["bash", "-c", script, "service-changed-surfaces", os.fspath(hook_path)],
        cwd=repo,
        input=payload,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        raise SurfaceConfigError(detail or "service changed-surfaces hook failed")

    valid_names = set(SURFACE_NAMES)
    marked_by_index: dict[int, list[str]] = {}
    for raw_line in result.stdout.decode("utf-8").splitlines():
        if not raw_line:
            continue
        idx_str, sep, surface = raw_line.partition("\t")
        if not sep or surface not in valid_names:
            raise SurfaceConfigError(f"service hook marked unknown surface: {surface or raw_line}")
        marked_by_index.setdefault(int(idx_str), []).append(surface)
    return marked_by_index


def _count_surfaces(surfaces: dict[str, bool]) -> int:
    return sum(1 for name, active in surfaces.items() if name != "no_validation_required" and active)


def all_surfaces_report(repo: Path, *, post_merge: bool = False) -> ClassificationReport:
    repo = Path(repo)
    profiles = _load_profiles(repo)
    source_only = (repo / "make/source.mk").is_file()
    surfaces = dict.fromkeys(SURFACE_NAMES, True)
    if profiles["database"] == "none":
        surfaces["db_integration"] = False
        surfaces["migrations"] = False
    if profiles["messaging"] != "nats-jetstream":
        surfaces["messaging_integration"] = False
    if profiles["cache"] != "redis":
        surfaces["cache_integration"] = False
    if profiles["object_storage"] != "s3":
        surfaces["object_storage_integration"] = False
    if profiles["outbound_auth"] != "oauth2-client-credentials":
        surfaces["oauth_integration"] = False
    if not source_only:
        for key in ("template_engine", "module_initializer", "initializer_source", "initializer_runtime", "initializer_artifacts"):
            surfaces[key] = False
    if post_merge and (surfaces["duplication"] or surfaces["architecture"]):
        surfaces["quality_tooling"] = True
    return ClassificationReport(
        surfaces=surfaces,
        classified=True,
        surface_count=_count_surfaces(surfaces),
        unclassified_files=(),
    )


def classify_report(
    repo: Path,
    files: Sequence[str],
    *,
    post_merge: bool = False,
) -> ClassificationReport:
    repo = Path(repo)
    profiles = _load_profiles(repo)
    database = profiles["database"]
    messaging = profiles["messaging"]
    cache_profile = profiles["cache"]
    object_storage = profiles["object_storage"]
    outbound_auth = profiles["outbound_auth"]
    outbox = profiles["outbox"]
    source_only = (repo / "make/source.mk").is_file()
    p9_retained = (repo / "test/tests/http_idempotency/mounted.rs").is_file()

    surfaces: dict[str, bool] = dict.fromkeys(SURFACE_NAMES, False)
    clean_files = [f for raw in files if (f := raw.strip("\r\n"))]
    hook_marks = _run_service_hook(repo, clean_files)
    unclassified: list[str] = []

    for idx, file in enumerate(clean_files):
        matched = False

        def mark(*names: str) -> None:
            nonlocal matched
            matched = True
            for name in names:
                surfaces[name] = True

        if _matches(file, "*.rs", "crates/*/src/*", "crates/*/tests/*", "test/src/*", "test/tests/*", "env/config/*"):
            mark("rust_source")

        if _matches(
            file,
            "Cargo.toml", "Cargo.lock", "crates/*/Cargo.toml", "test/Cargo.toml",
            "rust-toolchain.toml", ".cargo/config.toml", ".config/nextest.toml",
        ):
            mark("cargo_dependencies")

        # template:begin runtime-progress:classifier-runtime-progress-inputs
        if _matches(
            file,
            "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml",
            "crates/*/Cargo.toml", "crates/*/src/*", "vendor/*",
            "crates/service/examples/runtime_progress.rs", "crates/service/tests/runtime_progress.rs",
            "build/docker/Dockerfile", "build/docker/runtime-progress.Dockerfile",
            "scripts/profiles/runtime_progress.sh", "scripts/image/cargo_artifact.py",
            "make/template.mk", ".github/workflows/ci.yml",
            ".github/actions/resolve-changes/*", ".github/actions/load-versions/*",
            "scripts/verify/surfaces.py", "scripts/verify/runner.py",
        ):
            mark("runtime_progress")
        # template:end runtime-progress:classifier-runtime-progress-inputs

        if _matches(file, "vendor/hotpath/*"):
            mark("rust_source", "cargo_dependencies", "runtime_image")
            if source_only:
                mark("module_initializer", "initializer_runtime")
        elif file == "scripts/image/cargo_artifact.py":
            mark("openapi", "runtime_image", "validation_system")
            if source_only:
                mark("cargo_dependencies", "module_initializer")
        elif _matches(file, "vendor/sqlx-core/*"):
            mark("rust_source", "cargo_dependencies", "runtime_image")
            if database == "postgres":
                mark("db_integration")
            if source_only:
                mark("module_initializer", "initializer_runtime")
        elif _matches(file, "vendor/async-nats/*"):
            mark("rust_source", "cargo_dependencies", "runtime_image")
            if messaging == "nats-jetstream":
                mark("messaging_integration")
            if source_only:
                mark("module_initializer", "initializer_runtime")
        elif _matches(file, "vendor/aws-smithy-http-client/*"):
            mark("rust_source", "cargo_dependencies", "runtime_image")
            if object_storage == "s3":
                mark("object_storage_integration")
            if source_only:
                mark("module_initializer", "initializer_runtime")
        elif _matches(file, "vendor/hyper-util/*", "scripts/checks/native_transport.py"):
            mark("rust_source", "cargo_dependencies", "runtime_image")
            if source_only:
                mark("module_initializer", "initializer_runtime")
        elif _matches(file, "vendor/tonic/*"):
            mark("rust_source", "cargo_dependencies", "runtime_image")
            if source_only:
                mark("module_initializer", "initializer_runtime")

        if _matches(file, "crates/*.rs", "test/*.rs"):
            mark("duplication", "architecture")

        if _matches(
            file,
            "Cargo.toml", "Cargo.lock", "*/Cargo.toml", "*/Cargo.lock", "rust-toolchain.toml",
            "make/template.mk", "make/source.mk", "scripts/template/check_init.sh",
            "scripts/tests/template-profile-projections.py", ".github/workflows/ci.yml",
            ".github/actions/resolve-changes/*", ".github/actions/load-versions/*",
            "scripts/verify/surfaces.py", "scripts/verify/runner.py", "tools/versions.env",
        ):
            mark("duplication", "architecture")

        if _matches(
            file,
            ".jscpd.json", "quality/architecture.json", "scripts/checks/architecture.py",
            "scripts/checks/npx.py", "scripts/tests/test_quality.py",
        ):
            mark("duplication", "architecture", "quality_tooling")

        if _matches(file, "tools/versions.env", "make/template.mk"):
            mark("quality_tooling")

        if database == "postgres":
            if _matches(
                file,
                "crates/infra-postgres/*", "crates/infra-idempotency-store/*", "crates/infra-jobs/*",
                "crates/infra-webhooks/*", "crates/jobs-worker/*", "crates/migrate/*",
                "crates/tls-test-support/*", "test/*",
                "env/docker-compose.yml", "env/pgbouncer/*",
                "scripts/profiles/integration.sh", "scripts/profiles/compose.sh",
                "scripts/profiles/sqlx_prepare.sh", "scripts/service/database.sh",
                "migrations/*.sql", ".sqlx/*", "crates/*/.sqlx/*", ".cargo/config.toml",
            ):
                mark("db_integration")
            if _matches(file, ".sqlx/*", "crates/*/.sqlx/*"):
                mark("cargo_dependencies")
            if p9_retained and _matches(file, "crates/infra-http/*", "crates/infra-http-transport/*", "crates/infra-bearerauthn/*"):
                mark("db_integration")
            if _matches(
                file,
                "migrations/*.sql", "crates/migrate/*", "env/docker-compose.yml",
                "scripts/profiles/migration_validate.sh", "scripts/profiles/migration_history.sh",
                "scripts/profiles/compose.sh", "scripts/service/database.sh",
            ):
                mark("migrations", "runtime_image")
            if _matches(file, "scripts/tests/test_migration_history.py", "scripts/checks/npx.py"):
                mark("migrations")

        if messaging == "nats-jetstream" and _matches(
            file,
            "Cargo.toml", "Cargo.lock", "crates/domain-events/*", "crates/infra-messaging/*",
            "crates/nats-test-support/*",
            "crates/config/Cargo.toml", "crates/config/src/messaging.rs", "crates/config/src/messaging/*",
            "crates/jobs-worker/*", "crates/service/Cargo.toml", "crates/service/src/bootstrap/*",
            "env/docker-compose.yml", "env/nats/*",
            "test/tests/messaging_outbox.rs", "test/tests/messaging_recovery.rs",
            "test/fixtures/messaging_recovery.sql",
            "scripts/profiles/integration.sh", "scripts/profiles/compose.sh",
            "scripts/profiles/messaging_go_compat.sh", "make/template.mk", ".github/workflows/ci.yml",
        ):
            mark("messaging_integration")

        if cache_profile == "redis" and _matches(
            file,
            "Cargo.toml", "Cargo.lock", "crates/infra-cache/*", "crates/tls-test-support/*",
            "crates/config/Cargo.toml", "crates/config/src/cache.rs", "crates/config/src/cache/*",
            "crates/service/Cargo.toml", "crates/service/src/bootstrap/*", "env/docker-compose.yml",
            "scripts/profiles/integration.sh", "scripts/profiles/compose.sh",
            "make/template.mk", ".github/workflows/ci.yml",
        ):
            mark("cache_integration")

        if object_storage == "s3" and _matches(
            file,
            "Cargo.toml", "Cargo.lock", "crates/infra-object-storage/*", "env/docker-compose.yml",
            "scripts/profiles/integration.sh", "scripts/profiles/compose.sh",
            "make/template.mk", ".github/workflows/ci.yml",
        ):
            mark("object_storage_integration")

        if outbound_auth == "oauth2-client-credentials" and _matches(
            file,
            "Cargo.toml", "Cargo.lock", "crates/infra-oauth2-client-credentials/*",
            "crates/infra-outbound-grpc/*", "crates/infra-outbound-http/*",
            "scripts/profiles/integration.sh", "scripts/profiles/compose.sh",
            "make/template.mk", ".github/workflows/ci.yml",
        ):
            mark("oauth_integration")

        if outbox == "postgres" and _matches(
            file,
            "crates/infra-messaging/src/outbox.rs", "crates/infra-jobs/*", "crates/infra-postgres/*",
            "crates/config/src/jobs.rs", "crates/config/src/jobs/*",
        ):
            mark("db_integration", "messaging_integration")

        if _matches(file, "deny.toml", "scripts/checks/dependency_drift.sh"):
            mark("dependency_policy")

        if _matches(file, "clippy.toml", "rustfmt.toml", ".typos.toml", "Cargo.toml"):
            mark("lint_config")

        if _matches(
            file,
            ".redocly.yaml", "api/openapi/*", "crates/infra-webhooks/src/inbound/route.rs",
            "crates/infra-http/src/contract.rs", "crates/infra-http/src/contract/*",
            "crates/infra-http/src/authn.rs", "crates/infra-http/src/probes.rs",
            "crates/infra-http/src/problem.rs", "crates/infra-http/src/idempotency/openapi.rs",
            "crates/infra-http/src/idempotency/declaration.rs",
            "crates/service/src/api.rs", "scripts/image/cargo_artifact.py", "scripts/checks/npx.py",
        ):
            mark("openapi")

        # template:begin grpc:classifier-grpc-schema
        if _matches(
            file,
            "api/proto/*", "crates/grpc-contracts/*", "tools/grpc-codegen/*", "buf.yaml",
            "scripts/profiles/grpc_generate.sh", "scripts/profiles/grpc_check.sh", "tools/versions.env",
        ):
            mark("grpc_schema")
        if _matches(file, "tools/grpc-codegen/Cargo.toml", "tools/grpc-codegen/Cargo.lock"):
            mark("cargo_dependencies")
        if file == "tools/grpc-codegen/deny.toml":
            mark("dependency_policy")
        # template:end grpc:classifier-grpc-schema

        if _matches(
            file,
            "env/monitoring/*.rules.yml", "env/monitoring/tests/*",
            "scripts/checks/monitoring_rules.sh", "tools/versions.env", "make/template.mk",
            "scripts/verify/runner.py", "scripts/verify/surfaces.py",
            ".github/workflows/ci.yml", ".github/actions/resolve-changes/*", ".github/actions/load-versions/*",
        ):
            mark("monitoring_rules")

        if _matches(
            file,
            "tools/versions.env", "scripts/checks/tools.py", "scripts/checks/npx.py",
            "scripts/checks/dependency_drift.sh", "scripts/tests/test_checks_and_image.py",
            "build/docker/Dockerfile", "env/docker-compose.yml", ".cargo/config.toml",
            ".github/dependabot.yml",
            ".railway/package.json", ".railway/package-lock.json", ".railway/.npmrc",
        ):
            mark("tool_manifest")

        if file != "build/docker/runtime-progress.Dockerfile" and _matches(
            file,
            ".dockerignore", "build/docker/*", "scripts/image/build.sh", "scripts/image/check.sh",
            "scripts/image/inventory.py", "scripts/image/scan.sh", "scripts/image/cargo_artifact.py",
            "scripts/service/runtime-image.sh", "scripts/service/runtime-binaries.json", "Cargo.toml",
            "deny.toml", "tools/versions.env", "LICENSE",
        ):
            mark("runtime_image")

        if _matches(file, ".github/workflows/*", ".github/actions/*", ".github/codeql/*"):
            mark("github_workflows")
        if file == ".github/dependabot.yml":
            mark("dependency_automation")
        if _matches(file, "*.sh"):
            mark("shell")
        if not _matches(file, "vendor/*") and _matches(file, "*.py", "ruff.toml"):
            mark("python")
        if _matches(file, ".github/actions/publish-image/*", "scripts/image/publish_metadata.py", "scripts/tests/test_checks_and_image.py"):
            mark("publication_metadata")
        if _matches(file, ".gitleaks.toml", ".gitleaksignore"):
            mark("secret_scanning")

        if _matches(
            file,
            "template.lock", "template-owned.paths", "scripts/template/state.py",
            "scripts/template/sync.py", "scripts/template/init.py", "scripts/template/profiles.json",
            "scripts/harness/*", "AGENTS.md", "CLAUDE.md", "QWEN.md", "Grok.md", "opencode.json",
            ".agents/*", ".claude/*", ".codex/*", ".cursor/*", ".qwen/*", ".grok/*", ".opencode/*",
            "docs/skill-authoring.md", "docs/agent-harness.md", "docs/agent-harness/*",
            "docs/spec-first-workflow.md", "docs/spec-first-workflow/*",
            "docs/prompt-composition.md", "docs/prompt-maintenance.md",
        ):
            mark("agent_instructions")

        if _matches(
            file,
            "*.md", "docs/*", "specs/*", ".dockerignore", "build/docker/*",
            "scripts/checks/image_inputs.py", "scripts/tests/test_image_inputs.py", ".railway/*",
        ):
            mark("documentation")

        if _matches(
            file,
            ".editorconfig", ".gitattributes", ".gitignore", ".gitkeep", "*/.gitkeep", "LICENSE",
            ".github/CODEOWNERS", ".github/ISSUE_TEMPLATE/*",
        ):
            mark("no_validation_required")

        if _matches(
            file,
            "template.lock", "template-owned.paths", "Makefile", "make/*.mk",
            "scripts/verify/*", "scripts/checks/image_inputs.py", "scripts/checks/npx.py",
            "scripts/checks/tools.py", "scripts/image/cargo_artifact.py", "scripts/image/inventory.py",
            "scripts/image/scan.sh", "scripts/image/publish_metadata.py",
            "scripts/template/state.py", "scripts/template/sync.py", "scripts/template/init.py",
            "scripts/template/profiles.json", "scripts/profiles/integration.sh",
            "scripts/service/changed-surfaces.sh", "scripts/service/runtime-binaries.json",
            "scripts/tests/test_lock.py", "scripts/tests/test_verify.py",
            "scripts/tests/test_checks_and_image.py", "scripts/tests/test_image_inputs.py",
            "scripts/tests/test_image_inventory.py", "scripts/tests/test_profile_runners.py",
        ):
            mark("validation_system")

        if file != "scripts/template/sync.py" and _matches(
            file,
            "template-owned.paths", "make/source.mk", "scripts/template/*", "scripts/tests/template-*",
        ):
            mark("template_engine")

        if source_only:
            if _matches(file, "scripts/template/matrix.py", "scripts/template/check_init.sh"):
                mark("validation_system")

            if file == "scripts/template/sync.py":
                mark("module_initializer", "initializer_source")
            elif _matches(
                file,
                "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "template.lock",
                "Makefile", "make/*.mk", "api/openapi/*", "env/config/*",
                ".github/workflows/ci.yml", ".github/actions/resolve-changes/*", ".github/actions/load-versions/*",
                "scripts/template/*", "template-owned.paths", "scripts/tests/template-*",
                "crates/config/src/*", "crates/config/Cargo.toml",
                "crates/service/src/*", "crates/service/tests/*", "crates/service/Cargo.toml",
                "crates/infra-bearerauthn/*", "crates/infra-outbound-http/*",
                "crates/infra-idempotency-store/*", "crates/infra-webhooks/*",
                "crates/infra-http-transport/*", "crates/infra-http/Cargo.toml", "crates/infra-http/src/authn.rs",
                "crates/infra-http/src/context.rs", "crates/infra-http/src/idempotency/*",
                "crates/infra-http/src/harden.rs", "crates/infra-http/src/harden/*",
                "crates/infra-http/src/contract.rs", "crates/infra-http/src/contract/*",
                "crates/infra-http/src/lib.rs", "crates/infra-http/src/problem.rs",
                "crates/infra-postgres/*", "crates/migrate/*",
                "crates/infra-jobs/*", "crates/jobs-worker/*", "crates/domain-events/*",
                "crates/infra-messaging/*", "crates/nats-test-support/*", "crates/infra-cache/*",
                "crates/infra-object-storage/*",
                "crates/operation-context/*", "crates/shared-rules/*", "crates/tls-test-support/*",
                "crates/infra-telemetry/src/logging.rs", "crates/infra-telemetry/src/logging/tests.rs",
                "crates/service-failure/*", "crates/infra-oauth2-client-credentials/*",
                "test/*", "migrations/*", ".sqlx/*", ".cargo/config.toml", ".config/nextest.toml",
                "scripts/profiles/sqlx_prepare.sh", "scripts/profiles/integration.sh",
                "scripts/profiles/compose.sh", "scripts/checks/dependency_drift.sh",
            ):
                mark("module_initializer", "initializer_runtime")

            # template:begin grpc:classifier-grpc-initializer
            if _matches(
                file,
                "api/proto/*", "crates/infra-grpc/*", "crates/infra-outbound-grpc/*",
                "crates/grpc-contracts/*", "crates/shared-rules/src/grpc.rs",
                "crates/service/examples/grpc.rs", "tools/grpc-codegen/*", "buf.yaml",
                "scripts/profiles/grpc_generate.sh", "scripts/profiles/grpc_check.sh",
            ):
                mark("module_initializer", "initializer_runtime")
            # template:end grpc:classifier-grpc-initializer

            # template:begin service-auth:classifier-service-auth-initializer
            if _matches(file, "crates/infra-serviceauth/*"):
                mark("module_initializer", "initializer_runtime")
            # template:end service-auth:classifier-service-auth-initializer

            if _matches(
                file,
                "env/docker-compose.yml", "env/monitoring/*.rules.yml", "env/monitoring/tests/*",
                ".jscpd.json", "ruff.toml", "quality/*.json",
                "scripts/checks/*", ".dockerignore", "build/docker/*", "README.md",
                "SECURITY.md", ".gitleaks.toml", ".github/CODEOWNERS", ".github/ISSUE_TEMPLATE/*",
                ".github/dependabot.yml", ".github/workflows/cd.yml", ".github/workflows/published-image-scan.yml",
                ".github/actions/publish-image/action.yml", ".github/actions/rust-toolchain/action.yml",
                "scripts/verify/*", "scripts/image/*",
                "scripts/profiles/migration_history.sh", "scripts/profiles/migration_validate.sh",
                "scripts/profiles/messaging_go_compat.sh", "scripts/profiles/runtime_progress.sh",
                "scripts/tests/test_*.py", "scripts/harness/*", ".agents/*",
                "AGENTS.md", "CLAUDE.md", "QWEN.md", "Grok.md", "opencode.json",
                ".claude/*", ".codex/*", ".cursor/*", ".qwen/*", ".grok/*", ".opencode/*",
                "docs/repo-architecture.md", "docs/production-contract.md",
                "docs/backend-library-selection.md", "docs/ci-cd-production-ready.md",
                "docs/railway-deployment-profile.md", ".railway/*",
                "docs/template-sync.md", "docs/decisions/*", "docs/alerting.md",
            ):
                mark("module_initializer")

            if _matches(file, "tools/versions.env", "vendor/*"):
                mark("initializer_artifacts")

        if idx in hook_marks:
            mark(*hook_marks[idx])

        if not matched:
            unclassified.append(file)

    if source_only:
        if surfaces["initializer_runtime"]:
            surfaces["initializer_source"] = True
        if surfaces["initializer_runtime"] or surfaces["runtime_image"]:
            surfaces["initializer_artifacts"] = True

    if post_merge and (surfaces["duplication"] or surfaces["architecture"]):
        surfaces["quality_tooling"] = True

    return ClassificationReport(
        surfaces=surfaces,
        classified=len(unclassified) == 0,
        surface_count=_count_surfaces(surfaces),
        unclassified_files=tuple(unclassified),
    )


def classify_files(
    repo: Path,
    files: Sequence[str],
    *,
    post_merge: bool = False,
) -> dict[str, bool]:
    report = classify_report(repo, files, post_merge=post_merge)
    if not report.classified:
        raise UnclassifiedPathsError(list(report.unclassified_files), report.surfaces)
    return dict(report.surfaces)


def _git_show_bytes(repo: Path, rev_path: str) -> bytes | None:
    env = {**os.environ, "GIT_OPTIONAL_LOCKS": "0"}
    result = subprocess.run(
        ["git", "-c", "core.fsmonitor=false", "-C", os.fspath(repo), "show", rev_path],
        capture_output=True,
        check=False,
        env=env,
    )
    return result.stdout if result.returncode == 0 else None


def has_base_classifier(repo: Path, base_ref: str) -> bool:
    env = {**os.environ, "GIT_OPTIONAL_LOCKS": "0"}
    for rel in ("scripts/verify/surfaces.py", "scripts/ci/changed-surfaces.sh"):
        res = subprocess.run(
            ["git", "-c", "core.fsmonitor=false", "-C", os.fspath(repo), "cat-file", "-e", f"{base_ref}:{rel}"],
            capture_output=True,
            check=False,
            env=env,
        )
        if res.returncode == 0:
            return True
    return False


def _paths_present_in_commit(repo: Path, commit: str, files: Sequence[str]) -> set[str]:
    if not files:
        return set()
    env = {**os.environ, "GIT_OPTIONAL_LOCKS": "0", "GIT_LITERAL_PATHSPECS": "1"}
    result = subprocess.run(
        ["git", "-c", "core.fsmonitor=false", "-C", os.fspath(repo), "ls-tree", "--name-only", "-z", "--full-tree", commit, "--", *files],
        capture_output=True,
        check=False,
        env=env,
    )
    if result.returncode != 0:
        raise SurfaceConfigError(f"classifier base is unavailable: {commit}")
    return {entry.decode("utf-8") for entry in result.stdout.split(b"\0") if entry}


def _parse_kv_surfaces(text: str) -> dict[str, bool]:
    parsed: dict[str, bool] = {}
    for line in text.splitlines():
        key, sep, val = line.partition("=")
        if sep and val in ("true", "false"):
            parsed[key] = val == "true"
    return parsed


def classify_union_report(
    repo: Path,
    base_ref: str,
    files: Sequence[str] | None = None,
    *,
    post_merge: bool = False,
) -> ClassificationReport:
    repo = Path(repo).resolve()
    try:
        base_sha = _changed_paths.require_commit(repo, base_ref)
    except _changed_paths.ChangedPathsError:
        raise SurfaceConfigError(f"classifier base is unavailable: {base_ref}")

    if files is None:
        try:
            resolved_files = _changed_paths.changed_paths(repo, union=base_ref)
        except _changed_paths.ChangedPathsError as error:
            raise SurfaceConfigError(str(error))
    else:
        resolved_files = [f for raw in files if (f := raw.strip("\r\n"))]

    base_py = _git_show_bytes(repo, f"{base_sha}:scripts/verify/surfaces.py")
    base_sh = None if base_py is not None else _git_show_bytes(repo, f"{base_sha}:scripts/ci/changed-surfaces.sh")
    if base_py is None and base_sh is None:
        raise SurfaceConfigError(f"classifier base is unavailable: {base_ref}")

    classifier_rel = "scripts/verify/surfaces.py" if base_py is not None else "scripts/ci/changed-surfaces.sh"
    base_existing = _paths_present_in_commit(repo, base_sha, resolved_files)

    current_files = [f for f in resolved_files if (repo / f).exists() or (repo / f).is_symlink() or f not in base_existing]
    old_files = [f for f in resolved_files if f != classifier_rel and f in base_existing]

    current_report = classify_report(repo, current_files, post_merge=post_merge)

    with tempfile.TemporaryDirectory(prefix="surfaces-union-") as tmp_str:
        tmp = Path(tmp_str)
        if base_py is not None:
            script_path = tmp / "scripts/verify/surfaces.py"
            script_path.parent.mkdir(parents=True, exist_ok=True)
            script_path.write_bytes(base_py)
            base_cp = _git_show_bytes(repo, f"{base_sha}:scripts/verify/changed_paths.py")
            if base_cp is not None:
                (tmp / "scripts/verify/changed_paths.py").write_bytes(base_cp)
            base_state = _git_show_bytes(repo, f"{base_sha}:scripts/template/state.py")
            if base_state is not None:
                state_path = tmp / "scripts/template/state.py"
                state_path.parent.mkdir(parents=True, exist_ok=True)
                state_path.write_bytes(base_state)
            cmd = [sys.executable, "-B", os.fspath(script_path), "--repo", os.fspath(repo), "--files", *old_files]
            if post_merge:
                cmd.append("--post-merge")
            old_proc = subprocess.run(cmd, cwd=repo, capture_output=True, text=True, check=False)
        else:
            assert base_sh is not None
            script_path = tmp / "changed-surfaces.sh"
            patched_sh = base_sh
            legacy_state = _git_show_bytes(repo, f"{base_sha}:scripts/lib/template_state.py")
            if legacy_state is not None:
                legacy_state_path = tmp / "template_state.py"
                legacy_state_path.write_bytes(legacy_state)
                patched_sh = patched_sh.replace(
                    b"${root}/scripts/lib/template_state.py",
                    os.fsencode(legacy_state_path),
                )
            script_path.write_bytes(patched_sh)
            old_proc = subprocess.run(
                ["bash", os.fspath(script_path)],
                cwd=repo,
                input="".join(f"{f}\n" for f in old_files),
                capture_output=True,
                text=True,
                check=False,
            )

    if old_proc.returncode != 0:
        raise SurfaceConfigError(f"base classifier failed: {base_ref}", exit_code=old_proc.returncode)
    old_surfaces = _parse_kv_surfaces(old_proc.stdout)

    merged = {
        name: bool(current_report.surfaces[name] or old_surfaces.get(name, False))
        for name in SURFACE_NAMES
    }
    if post_merge and (merged["duplication"] or merged["architecture"]):
        merged["quality_tooling"] = True

    return ClassificationReport(
        surfaces=merged,
        classified=current_report.classified,
        surface_count=_count_surfaces(merged),
        unclassified_files=current_report.unclassified_files,
    )


def format_report(report: ClassificationReport) -> str:
    lines = [f"{name}={'true' if report.surfaces[name] else 'false'}" for name in SURFACE_NAMES]
    lines.append(f"classified={'true' if report.classified else 'false'}")
    lines.append(f"surface_count={report.surface_count}")
    lines.append(f"unclassified_files={','.join(report.unclassified_files)}")
    return "\n".join(lines) + "\n"


def _stdin_is_piped() -> bool:
    try:
        mode = os.fstat(sys.stdin.fileno()).st_mode
    except OSError:
        return False
    return stat.S_ISFIFO(mode) or stat.S_ISREG(mode)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=None, help="repository root directory")
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--all", action="store_true", help="select all applicable surfaces")
    group.add_argument("--base", metavar="REF", help="classify committed diff from REF to HEAD")
    group.add_argument("--union", metavar="REF", help="union HEAD and REF classifiers over changed paths")
    parser.add_argument("--files", nargs="*", metavar="PATH", help="explicit changed paths")
    parser.add_argument("--post-merge", action="store_true", help="enable post-merge surfaces")
    parser.add_argument("--github-output", type=Path, default=None, help="GITHUB_OUTPUT file path")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    post_merge = args.post_merge or os.environ.get("POST_MERGE") == "true"
    if args.files is not None and (args.all or args.base is not None):
        parser.error("--files cannot be combined with --all or --base")

    repo = args.repo.resolve() if args.repo is not None else _changed_paths.resolve_repo_root()
    try:
        if args.all:
            report = all_surfaces_report(repo, post_merge=post_merge)
        elif args.union is not None:
            explicit: Sequence[str] | None = args.files
            if explicit is None and _stdin_is_piped():
                explicit = [line.rstrip("\r\n") for line in sys.stdin if line.rstrip("\r\n")]
            report = classify_union_report(repo, args.union, explicit, post_merge=post_merge)
        elif args.base is not None:
            files = _changed_paths.changed_paths(repo, base=args.base)
            report = classify_report(repo, files, post_merge=post_merge)
        elif args.files is not None:
            report = classify_report(repo, args.files, post_merge=post_merge)
        elif _stdin_is_piped():
            piped = [line.rstrip("\r\n") for line in sys.stdin if line.rstrip("\r\n")]
            report = classify_report(repo, piped, post_merge=post_merge)
        else:
            files = _changed_paths.changed_paths(repo)
            report = classify_report(repo, files, post_merge=post_merge)
    except _changed_paths.ChangedPathsError as error:
        print(str(error), file=sys.stderr)
        return 2
    except SurfaceConfigError as error:
        print(str(error), file=sys.stderr)
        return error.exit_code

    rendered = format_report(report)
    sys.stdout.write(rendered)
    if args.github_output is not None:
        args.github_output.parent.mkdir(parents=True, exist_ok=True)
        with args.github_output.open("a", encoding="utf-8") as handle:
            handle.write(rendered)

    return 0 if report.classified else 1


if __name__ == "__main__":
    raise SystemExit(main())
