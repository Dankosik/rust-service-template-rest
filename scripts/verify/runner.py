#!/usr/bin/env python3
"""Route changed surfaces to local validation commands and record reusable verification receipts."""

from __future__ import annotations

import argparse
from collections.abc import Sequence
from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True

if str(Path(__file__).resolve().parent) not in sys.path:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
import affected_crates as _affected_crates  # noqa: E402
import changed_paths as _changed_paths  # noqa: E402
import lock as _lock  # noqa: E402
import surfaces as _surfaces  # noqa: E402

SELF = Path(__file__).resolve()


@dataclass(frozen=True)
class PlannedStep:
    kind: str
    argument: str
    reason: str
    display: str
    cost_class: str
    requires_heavy: bool
    requires_docker: bool
    host_fallback: str | None = None


@dataclass(frozen=True)
class CiOwnedStep:
    display: str
    detail: str


class RunnerError(RuntimeError):
    def __init__(self, exit_code: int, message: str = "") -> None:
        super().__init__(message)
        self.exit_code = exit_code


def _git_output(repo: Path, *args: str) -> str | None:
    env = {**os.environ, "GIT_OPTIONAL_LOCKS": "0"}
    result = subprocess.run(
        ["git", "-c", "core.fsmonitor=false", "-C", os.fspath(repo), *args],
        capture_output=True,
        text=True,
        check=False,
        env=env,
    )
    if result.returncode != 0:
        return None
    value = result.stdout.strip()
    return value or None


def _resolve_ci_mode(repo: Path) -> str:
    ci_mode = os.environ.get("REPOSITORY_CI", "").strip()
    service_mk = repo / "make/service.mk"
    if not ci_mode and service_mk.is_file():
        pattern = re.compile(r"^REPOSITORY_CI\s*\??=\s*(.*)$")
        last_match: str | None = None
        for line in service_mk.read_text(encoding="utf-8", errors="replace").splitlines():
            matched = pattern.match(line)
            if matched is not None:
                tokens = matched.group(1).strip().split()
                last_match = tokens[0] if tokens else ""
        if last_match:
            ci_mode = last_match
    ci_mode = ci_mode or "github-actions"
    if ci_mode not in ("github-actions", "none"):
        raise RunnerError(2, f"unsupported REPOSITORY_CI: {ci_mode}; expected github-actions or none")
    return ci_mode


def fingerprint_candidate(
    repo: Path,
    files: Sequence[str],
    *,
    base_ref: str,
    resolved_base_sha: str,
    merge_base_sha: str,
    include_quality_configs: bool = False,
) -> str:
    head = _git_output(repo, "rev-parse", "HEAD")
    if head is None:
        raise RunnerError(2, "HEAD commit is unavailable")

    watched = [*files, "tools/versions.env"]
    if include_quality_configs:
        watched.extend(
            (
                ".jscpd.json",
                "quality/architecture.json",
                "scripts/checks/architecture.py",
                "scripts/checks/npx.py",
                "scripts/tests/test_quality.py",
            )
        )

    digest = hashlib.sha256()
    digest.update(f"head={head}\n".encode("utf-8"))
    digest.update(
        f"base_ref={base_ref}\nresolved_base_sha={resolved_base_sha}\nmerge_base_sha={merge_base_sha}\n".encode(
            "utf-8"
        )
    )

    for rel in watched:
        target = repo / rel
        try:
            st = target.lstat()
        except OSError:
            digest.update(f"deleted  {rel}\n".encode("utf-8"))
            continue

        mode = st.st_mode
        if stat.S_ISLNK(mode):
            link_hash = hashlib.sha256(os.readlink(target).encode("utf-8")).hexdigest()
            digest.update(f"mode 120000  {rel}\nsymlink {link_hash}\n".encode("utf-8"))
        elif stat.S_ISREG(mode):
            fmode = "100755" if (mode & 0o111) else "100644"
            file_hash = hashlib.sha256(target.read_bytes()).hexdigest()
            digest.update(f"mode {fmode}  {rel}\n{file_hash}  {rel}\n".encode("utf-8"))
        else:
            digest.update(f"deleted  {rel}\n".encode("utf-8"))

    return digest.hexdigest()


def prepare_command(
    kind: str,
    argument: str,
    *,
    execution_head: str,
    ci_mode: str,
) -> tuple[list[str], str]:
    if kind == "make":
        cmd = ["make", argument]
    elif kind == "lint":
        cmd = ["make", "lint-changed", f"PKGS={argument}"]
    elif kind == "test":
        cmd = ["make", "test-changed", f"PKGS={argument}"]
    elif kind == "shell":
        cmd = ["make", "shellcheck", f"SHELL_FILES={argument}"]
    elif kind == "image-build":
        cmd = ["env", f"VCS_REF={execution_head}", "make", "runtime-image-build", f"RUNTIME_IMAGE={argument}"]
    elif kind == "image-check":
        cmd = ["make", "runtime-image-check", f"RUNTIME_IMAGE={argument}", f"RUNTIME_EXPECTED_COMMIT={execution_head}"]
    elif kind == "artifacts":
        cmd = ["make", "template-init-artifacts", f"ARTIFACT_GRAPHS={argument}"]
    elif kind == "image-sbom":
        cmd = ["make", "container-sbom", f"CONTAINER_IMAGE={argument}"]
    elif kind == "image-security":
        cmd = ["make", "container-security", f"CONTAINER_IMAGE={argument}"]
    elif kind == "migration-validate":
        cmd = ["make", "migration-validate", f"RUNTIME_IMAGE={argument}", f"RUNTIME_EXPECTED_COMMIT={execution_head}"]
    elif kind == "make-vars":
        cmd = ["make", *argument.split()]
    else:
        raise RunnerError(2, f"unknown verification command kind: {kind}")

    if ci_mode == "none":
        cmd = ["env", "ALLOW_HEAVY=1", "ALLOW_FULL=1", *cmd]
    return cmd, shlex.join(cmd)


def _is_ci_owned(*, requires_heavy: bool, display: str, ci_mode: str) -> bool:
    if os.environ.get("CI") == "true" or ci_mode == "none":
        return False
    if requires_heavy:
        return os.environ.get("ALLOW_HEAVY") != "1"
    return display == "make template-init-check" and os.environ.get("ALLOW_FULL") != "1"


def _select_artifact_graphs(repo: Path, merge_base_sha: str, files: Sequence[str]) -> str:
    local_matrix = repo / "scripts/template/matrix.py"
    matrix_script = local_matrix if local_matrix.is_file() else SELF.parents[1] / "template/matrix.py"
    if not matrix_script.is_file():
        return ""

    cmd = [sys.executable, "-B", os.fspath(matrix_script), "--artifacts-only"]
    if local_matrix.is_file() and _surfaces.has_base_classifier(repo, merge_base_sha):
        cmd.extend(("--union", merge_base_sha))
    cmd.extend(("--files", *files))

    result = subprocess.run(cmd, cwd=repo, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        detail = result.stderr.strip()
        raise RunnerError(result.returncode, detail or "artifact graph planner failed")
    for line in result.stdout.splitlines():
        if line.startswith("artifact_graphs="):
            return line.partition("=")[2].strip()
    return result.stdout.strip()


def _build_plan(
    repo: Path,
    files: Sequence[str],
    report: _surfaces.ClassificationReport,
    *,
    ci_mode: str,
    merge_base_sha: str,
) -> tuple[list[PlannedStep], list[CiOwnedStep], list[str]]:
    verdicts = report.surfaces

    def is_true(name: str) -> bool:
        return bool(verdicts.get(name, False))

    steps: list[PlannedStep] = []
    ci_owned: list[CiOwnedStep] = []
    not_applicable: list[str] = []
    seen_keys: set[str] = set()

    def add_command(
        kind: str,
        argument: str,
        reason: str,
        cost_class: str = "cpu",
        *,
        display: str | None = None,
        heavy: bool = False,
        docker: bool = False,
        host_fallback: str | None = None,
    ) -> None:
        key = f"{kind}|{argument}"
        if key in seen_keys:
            return
        seen_keys.add(key)
        shown = display or f"make {argument}"
        req_heavy_str = "true" if heavy else "false"
        req_docker_str = "true" if docker else "false"
        if _is_ci_owned(requires_heavy=heavy, display=shown, ci_mode=ci_mode):
            detail = (
                f"    because {reason}\n"
                f"    cost_class={cost_class} requires_heavy={req_heavy_str} requires_docker={req_docker_str}"
            )
            ci_owned.append(CiOwnedStep(display=shown, detail=detail))
            return
        steps.append(
            PlannedStep(
                kind=kind,
                argument=argument,
                reason=reason,
                display=shown,
                cost_class=cost_class,
                requires_heavy=heavy,
                requires_docker=docker,
                host_fallback=host_fallback,
            )
        )

    def add_na(surface: str, reason: str) -> None:
        not_applicable.append(f"{surface}: {reason}")

    if is_true("monitoring_rules") and (repo / "scripts/checks/monitoring_rules.sh").is_file():
        add_command("make", "monitoring-rules", "monitoring rules, fixtures, or their pinned route changed", "cheap")

    if is_true("tool_manifest"):
        add_command("make", "tools-check", "tool manifest changed", "cheap")

    if is_true("validation_system"):
        add_command("make", "changed-surfaces-check", "validation routing changed", "cheap")
        add_command("make", "affected-crates-check", "validation routing changed", "cpu")
        add_command("make", "validation-lock-self-test", "validation routing changed", "cheap")
        add_command("make", "verify-check", "validation routing changed", "cpu")

    if is_true("duplication"):
        add_command("make", "duplication-check", "Rust source, clone admission, or detector inputs changed", "cpu")

    if is_true("architecture"):
        add_command("make", "architecture-check", "declared graph, boundary policy, or checker inputs changed", "cheap")

    if is_true("initializer_runtime"):
        add_command("make", "template-init-check", "canonical projections and the initializer runtime graph inventory", "cpu", docker=True)
    elif is_true("module_initializer"):
        add_command("make", "template-init-projections", "projected text changed and no runtime input did", "cpu")

    # template:begin grpc:verify-grpc-schema
    if is_true("grpc_schema"):
        add_command("make", "grpc-check", "protobuf schema, generator, or committed output changed", "cpu", heavy=True)
        if ci_mode == "none":
            add_command("make", "grpc-generator-deny", "no CI checks the standalone generator's dependency policy", "cpu")
    # template:end grpc:verify-grpc-schema

    workspace_rust = is_true("cargo_dependencies") or (ci_mode == "none" and is_true("rust_source"))
    affected_pkgs: list[str] = []
    affected_fallback = False
    if is_true("rust_source") and not workspace_rust:
        mode, affected_pkgs = _affected_crates.select_affected_crates(repo, files)
        if mode == "workspace":
            workspace_rust = True
            affected_fallback = True

    if is_true("rust_source") or is_true("cargo_dependencies"):
        add_command("make", "unused-deps", "dependency declarations or their users changed", "cpu")

    if is_true("cargo_dependencies") or is_true("dependency_policy"):
        add_command("make", "deny", "dependency graph or policy changed", "cpu")
        add_command("make", "dependency-drift", "dependency graph or policy changed", "cheap")
        # template:begin grpc:verify-grpc-generator-deny
        if (repo / "tools/grpc-codegen/Cargo.toml").is_file():
            add_command("make", "grpc-generator-deny", "standalone generator dependency graph or policy changed", "cpu")
        # template:end grpc:verify-grpc-generator-deny

    if is_true("rust_source") or is_true("lint_config"):
        add_command("make", "fmt-check", "Rust source or formatting configuration changed", "cheap")
    elif ci_mode == "none" and is_true("cargo_dependencies"):
        add_command("make", "fmt-check", "no CI formats the workspace after a dependency change", "cheap")

    if workspace_rust:
        reason = "dependency, lockfile, or toolchain changes can affect every crate"
        if not is_true("cargo_dependencies"):
            if affected_fallback:
                reason = "Rust changes need the workspace oracle (workspace_fallback)"
            elif ci_mode == "none":
                reason = "no CI runs the workspace route of a push"
        add_command("make", "lint", reason, "cpu")
        add_command("make", "build", reason, "cpu")
        add_command("make", "test", reason, "cpu")
    else:
        pkgs_str = " ".join(affected_pkgs)
        if is_true("lint_config"):
            add_command("make", "lint", "the complete lint configuration changed", "cpu")
        elif pkgs_str:
            add_command("lint", pkgs_str, "affected crates and their dependents changed", "cpu", display=f"make lint-changed PKGS='{pkgs_str}'")
        if pkgs_str:
            add_command("test", pkgs_str, "affected crates and their dependents changed", "cpu", display=f"make test-changed PKGS='{pkgs_str}'")

    if is_true("quality_tooling") or is_true("template_engine"):
        add_command("make", "quality-check-self-test", "readability policy, checker, or template projection inputs changed", "cpu")
        if (repo / "make/source.mk").is_file():
            add_command("make", "template-quality-projections", "readability checks must remain usable after profile projection and rename", "cpu")

    if is_true("cargo_dependencies") and (repo / "scripts/checks/native_transport.py").is_file():
        add_command("make", "native-transport-regressions", "patched native transport graph or source changed", "cpu", heavy=True)

    if ci_mode == "none" and (is_true("rust_source") or is_true("cargo_dependencies")):
        add_command("make", "profiling-check", "no CI compiles the optional profiling configurations", "cpu")

    if is_true("openapi"):
        add_command("make", "openapi-check", "OpenAPI document or its lint configuration changed", "cpu")
        if ci_mode == "none":
            add_command("make-vars", f"openapi-breaking-since BASE_REF={merge_base_sha}", "no CI compares the document with the merge base", "cpu")

    if is_true("github_workflows"):
        add_command("make", "actionlint", "GitHub workflow or action source changed", "cheap")
        add_command("make", "zizmor", "GitHub workflow or action source changed", "cheap")

    if is_true("dependency_automation"):
        if ci_mode == "none":
            add_na("dependency_automation", "no CI validates the Dependabot schema or runs Dependency Review")
        else:
            add_na("dependency_automation", "GitHub validates the Dependabot schema; Dependency Review stays a CI gate")

    if is_true("shell"):
        existing_shell = [f for f in files if f.endswith(".sh") and (repo / f).is_file()]
        if existing_shell:
            joined = " ".join(existing_shell)
            add_command("shell", joined, "shell sources changed", "docker", display=f"make shellcheck SHELL_FILES='{joined}'", docker=True, host_fallback="shellcheck")
        else:
            add_na("shell", "no changed shell source remains")

    if is_true("python"):
        add_command("make", "python-lint", "Python source or its lint configuration changed", "cheap")

    if is_true("agent_instructions"):
        add_command("make", "check-instructions", "agent instructions, skills, roles, or carriers changed", "cheap")

    if is_true("publication_metadata"):
        add_command("make", "publish-image-metadata-check", "publication naming or promotion changed", "cheap")

    if ci_mode == "none":
        add_command("make-vars", f"secret-scan BASE_REF={merge_base_sha}", "no CI scans the commits since the merge base", "cpu")
    elif is_true("secret_scanning"):
        add_command("make", "secret-scan", "secret scanning policy changed", "cpu")

    if is_true("migrations"):
        add_command("make", "migration-check", "migration set or its runner changed", "cpu")
        add_command("make", "migration-history-self-test", "migration set or its runner changed", "cheap")

    if is_true("db_integration"):
        add_command("make", "test-integration-db", "database adapter, runner, or database proof changed", "docker", heavy=True, docker=True)
        if ci_mode == "none":
            add_command("make", "integration-compile", "no CI compiles the integration suites the change does not run", "cpu")
        add_command("make", "sqlx-check", "a checked statement, a migration, or the query metadata changed", "docker", heavy=True, docker=True)

    if is_true("messaging_integration"):
        add_command("make", "test-integration-messaging", "JetStream adapter, bridge, Compose, or messaging proof changed", "docker", heavy=True, docker=True)
        if ci_mode == "none":
            add_command("make", "messaging-go-compat", "no CI compares the wire with the pinned Go service", "cpu", heavy=True)

    if is_true("cache_integration"):
        add_command("make", "test-integration-cache", "cache adapter, Compose, or cache proof changed", "docker", heavy=True, docker=True)

    if is_true("object_storage_integration"):
        add_command("make", "test-integration-object-storage", "object storage adapter, Compose, or emulator proof changed", "docker", heavy=True, docker=True)

    if is_true("oauth_integration"):
        add_command("make", "test-integration-oauth", "OAuth adapter, its transport, or Keycloak proof changed", "docker", heavy=True, docker=True)

    # template:begin runtime-progress:verify-runtime-progress
    if is_true("runtime_progress"):
        add_command("make", "runtime-progress-proof", "the finite production-runtime CPU quota envelope changed", "docker", heavy=True, docker=True)
    # template:end runtime-progress:verify-runtime-progress

    if is_true("runtime_image") or is_true("migrations"):
        image = os.environ.get("VERIFY_RUNTIME_IMAGE") or "service:verify"
        if is_true("runtime_image"):
            add_command("make", "dockerfile-check", "runtime image inputs changed", "docker", docker=True)
        add_command("image-build", image, "one image is shared by the selected runtime gates", "docker", display=f"make runtime-image-build RUNTIME_IMAGE={image}", heavy=True, docker=True)
        if is_true("migrations"):
            add_command("migration-validate", image, "migration set or its rehearsal changed", "docker", display=f"make migration-validate RUNTIME_IMAGE={image}", heavy=True, docker=True)
        else:
            add_command("image-check", image, "runtime image lifecycle changed", "docker", display=f"make runtime-image-check RUNTIME_IMAGE={image}", heavy=True, docker=True)
        if is_true("runtime_image"):
            add_command("image-security", image, "runtime image inputs changed", "docker", display=f"make container-security CONTAINER_IMAGE={image}", heavy=True, docker=True)
            add_command("image-sbom", image, "admitted runtime inventory conversion", "docker", display=f"make container-sbom CONTAINER_IMAGE={image}", heavy=True, docker=True)

    if is_true("initializer_artifacts"):
        artifact_graphs = _select_artifact_graphs(repo, merge_base_sha, files)
        if artifact_graphs:
            add_command("artifacts", artifact_graphs, "selected initialized release binary sets", "docker", display=f"make template-init-artifacts ARTIFACT_GRAPHS={artifact_graphs}", heavy=True, docker=True)

    if is_true("documentation"):
        add_command("make", "docs-check", "Markdown changed", "docker", docker=True, host_fallback="lychee")

    if ci_mode == "none":
        if is_true("rust_source") or is_true("cargo_dependencies") or is_true("github_workflows"):
            add_na("codeql", "no code scanning; GitHub code scanning of a private repository needs a paid plan")
        if is_true("cargo_dependencies") or is_true("tool_manifest") or is_true("github_workflows") or is_true("runtime_image"):
            add_na("dependency_review", "make deny covers Rust advisories and licenses and container-security the image; workflow actions do not run")
        if is_true("runtime_image") or is_true("publication_metadata"):
            add_na("publication", "cd.yml publication, signing, and attestation run only from GitHub Actions")
        add_na("history_secret_scan", "the scheduled full-history scan does not run; run ALLOW_HEAVY=1 make secret-scan-history before a release")

    return steps, ci_owned, not_applicable


def format_plan(
    *,
    ci_mode: str,
    files: Sequence[str],
    report: _surfaces.ClassificationReport,
    steps: Sequence[PlannedStep],
    ci_owned: Sequence[CiOwnedStep],
    not_applicable: Sequence[str],
) -> str:
    lines: list[str] = []
    if ci_mode == "none":
        lines.append("ci_mode: none")
    lines.append("files:")
    lines.extend(f"  {f}" for f in files)
    lines.append("surfaces:")
    lines.extend(f"  {s_line}" for s_line in _surfaces.format_report(report).splitlines())
    lines.append("commands:")
    if not steps:
        lines.append("  none")
    else:
        for step in steps:
            req_heavy = "true" if step.requires_heavy else "false"
            req_docker = "true" if step.requires_docker else "false"
            lines.append(f"  {step.display}")
            lines.append(f"    because {step.reason}")
            lines.append(f"    cost_class={step.cost_class} requires_heavy={req_heavy} requires_docker={req_docker}")
    if ci_owned:
        lines.append("ci-owned:")
        for item in ci_owned:
            lines.append(f"  {item.display}")
            lines.append(item.detail)
    if not_applicable:
        lines.append("not applicable:")
        lines.extend(f"  {na}" for na in not_applicable)
    return "\n".join(lines) + "\n"


def _blocked(reason: str) -> RunnerError:
    return RunnerError(
        2,
        "claim: surface-aware verification\n"
        "result: blocked\n"
        "status: blocked\n"
        f"gap_or_next_owner: {reason}",
    )


def _cmd_version(binary: str, *args: str) -> str:
    try:
        res = subprocess.run([binary, *args], capture_output=True, text=True, check=False)
    except OSError:
        return "unavailable"
    if res.returncode != 0:
        return "unavailable"
    first = res.stdout.strip().splitlines()
    return first[0].strip() if first else "unavailable"


def _preflight_environment(
    repo: Path,
    report: _surfaces.ClassificationReport,
    steps: Sequence[PlannedStep],
    *,
    ci_mode: str,
) -> str:
    verdicts = report.surfaces

    def is_true(name: str) -> bool:
        return bool(verdicts.get(name, False))

    for binary in ("git", "make", "python3"):
        if shutil.which(binary) is None:
            raise _blocked(f"required binary is unavailable: {binary}")

    if any(
        is_true(s)
        for s in (
            "rust_source", "cargo_dependencies", "dependency_policy", "lint_config",
            "openapi", "validation_system", "module_initializer", "tool_manifest",
            "duplication", "architecture", "quality_tooling", "template_engine",
        )
    ):
        cargo_bin = os.environ.get("CARGO", "cargo")
        if shutil.which(cargo_bin) is None:
            raise _blocked(f"required binary is unavailable: {cargo_bin}")

    if any(is_true(s) for s in ("openapi", "migrations", "duplication", "quality_tooling", "template_engine")):
        if shutil.which("npx") is None:
            raise _blocked("required binary is unavailable: npx")

    if is_true("github_workflows") or is_true("secret_scanning") or ci_mode == "none":
        if shutil.which("go") is None:
            raise _blocked("required binary is unavailable: go")

    docker_steps = [step for step in steps if step.requires_docker]
    docker_command = os.environ.get("VERIFY_DOCKER_COMMAND") or "docker"
    docker_environment = "not-used"

    if docker_steps:
        has_docker_bin = shutil.which(docker_command) is not None
        daemon_ok = False
        if has_docker_bin:
            info_res = subprocess.run(
                [docker_command, "info"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
            daemon_ok = info_res.returncode == 0

        if daemon_ok:
            docker_environment = _cmd_version(docker_command, "version", "--format", "{{.Client.Version}}/{{.Server.Version}}")
        else:
            is_ci = os.environ.get("CI") == "true"
            fallback_versions: list[str] = []
            for step in docker_steps:
                if is_ci or step.host_fallback is None:
                    if not has_docker_bin:
                        raise _blocked("Docker is required")
                    raise _blocked("Docker is required and the daemon is unavailable")
                if shutil.which(step.host_fallback) is None:
                    if not has_docker_bin:
                        raise _blocked(f"Docker is required (and host {step.host_fallback} is unavailable)")
                    raise _blocked(f"Docker is required and the daemon is unavailable (and host {step.host_fallback} is unavailable)")
                fallback_versions.append(f"{step.host_fallback}:{_cmd_version(step.host_fallback, '--version')}")
            docker_environment = f"host-fallback({','.join(sorted(set(fallback_versions)))})"

    uname = platform.uname()
    uname_str = f"{uname.system} {uname.release} {uname.machine}"
    rust_env = _cmd_version("rustc", "--version")
    cargo_env = _cmd_version(os.environ.get("CARGO", "cargo"), "--version")
    versions_file = repo / "tools/versions.env"
    tool_manifest_hash = (
        hashlib.sha256(versions_file.read_bytes()).hexdigest()[:12]
        if versions_file.is_file()
        else "unavailable"
    )
    allow_full = os.environ.get("ALLOW_FULL", "")
    allow_heavy = os.environ.get("ALLOW_HEAVY", "")
    detail = (
        f"{uname_str}; {rust_env}; {cargo_env}; docker={docker_environment}; "
        f"ALLOW_FULL={allow_full}; ALLOW_HEAVY={allow_heavy}; tools={tool_manifest_hash}"
    )
    if ci_mode == "none":
        detail += "; REPOSITORY_CI=none"
    return detail


def _resolve_verify_dir(repo: Path) -> Path:
    common = _git_output(repo, "rev-parse", "--path-format=absolute", "--git-common-dir")
    if common is not None:
        return Path(common) / "codex" / "verify"
    fallback = _git_output(repo, "rev-parse", "--git-common-dir")
    if fallback is not None:
        p = Path(fallback)
        return (p if p.is_absolute() else (repo / p).resolve()) / "codex" / "verify"
    return repo / ".git" / "codex" / "verify"


def prune_verify_dir(receipt_dir: Path, *, max_receipts: int = 20, max_attempts: int = 20) -> None:
    if not receipt_dir.is_dir():
        return
    receipts: list[tuple[int, str, Path]] = []
    attempts: list[tuple[int, str, Path]] = []
    for entry in receipt_dir.iterdir():
        try:
            st = entry.lstat()
        except OSError:
            continue
        if not stat.S_ISREG(st.st_mode):
            continue
        if entry.name.endswith(".receipt"):
            receipts.append((st.st_mtime_ns, entry.name, entry))
        elif entry.name.startswith("attempt-") and not entry.name.endswith(".receipt.tmp"):
            attempts.append((st.st_mtime_ns, entry.name, entry))
    receipts.sort(reverse=True)
    for _, _, stale in receipts[max_receipts:]:
        stale.unlink(missing_ok=True)
    attempts.sort(reverse=True)
    for _, _, stale in attempts[max_attempts:]:
        stale.unlink(missing_ok=True)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=None, help="repository root directory")
    parser.add_argument("--plan", action="store_true", help="print verification plan without running")
    parser.add_argument("--base", metavar="REF", default=None, help="override BASE_REF")
    parser.add_argument("--files", nargs="+", metavar="PATH", default=None, help="explicit changed paths")
    return parser


def run(argv: Sequence[str] | None = None) -> int:
    raw_argv = list(sys.argv[1:] if argv is None else argv)
    args = build_parser().parse_args(raw_argv)
    repo = args.repo.resolve() if args.repo is not None else _changed_paths.resolve_repo_root()
    base_ref = args.base or os.environ.get("BASE_REF") or "origin/main"

    if args.files is not None:
        files = sorted({f for f in args.files if f})
        resolved = _git_output(repo, "rev-parse", "--verify", "--end-of-options", f"{base_ref}^{{commit}}")
        if resolved is not None:
            resolved_base_sha = resolved
            merge_base_sha = _git_output(repo, "merge-base", "HEAD", resolved_base_sha) or resolved_base_sha
        else:
            head_sha = _git_output(repo, "rev-parse", "HEAD")
            if head_sha is None:
                print("HEAD commit is unavailable", file=sys.stderr)
                return 2
            resolved_base_sha = merge_base_sha = head_sha
    else:
        try:
            resolved_base_sha = _changed_paths.require_commit(repo, base_ref)
            merge_base_sha = _git_output(repo, "merge-base", "HEAD", resolved_base_sha) or resolved_base_sha
            files = _changed_paths.changed_paths(repo, union=base_ref)
        except _changed_paths.ChangedPathsError as error:
            print(str(error), file=sys.stderr)
            return 2

    if not files:
        print("no changed files; verification is not applicable")
        return 0
    if any(any(ch.isspace() for ch in path) for path in files):
        print("verification does not support changed paths containing whitespace", file=sys.stderr)
        return 2

    try:
        ci_mode = _resolve_ci_mode(repo)
    except RunnerError as error:
        if str(error):
            print(str(error), file=sys.stderr)
        return error.exit_code

    try:
        if _surfaces.has_base_classifier(repo, merge_base_sha):
            report = _surfaces.classify_union_report(repo, merge_base_sha, files)
        else:
            report = _surfaces.classify_report(repo, files)
    except _surfaces.SurfaceConfigError as error:
        print(str(error), file=sys.stderr)
        return error.exit_code

    if not report.classified:
        sys.stderr.write(_surfaces.format_report(report))
        return 1

    try:
        steps, ci_owned, not_applicable = _build_plan(
            repo, files, report, ci_mode=ci_mode, merge_base_sha=merge_base_sha
        )
    except RunnerError as error:
        if str(error):
            print(str(error), file=sys.stderr)
        return error.exit_code

    plan_text = format_plan(
        ci_mode=ci_mode, files=files, report=report, steps=steps, ci_owned=ci_owned, not_applicable=not_applicable
    )
    if args.plan:
        sys.stdout.write(plan_text)
        return 0

    ci_owned_summary = ";".join(item.display for item in ci_owned)
    if not steps:
        sys.stdout.write(plan_text)
        if ci_owned_summary:
            print(f"verification not applicable locally: CI owns {ci_owned_summary}")
        else:
            print("verification not applicable: no executable checks for changed surfaces")
        return 0

    try:
        environment_detail = _preflight_environment(repo, report, steps, ci_mode=ci_mode)
    except RunnerError as error:
        print(str(error), file=sys.stderr)
        return error.exit_code

    include_quality_configs = bool(
        report.surfaces.get("duplication")
        or report.surfaces.get("architecture")
        or report.surfaces.get("quality_tooling")
    )

    try:
        lock_ctx = _lock.hold([sys.executable, os.fspath(SELF), *raw_argv], cwd=repo)
    except _lock.LockError as error:
        print(str(error), file=sys.stderr)
        return error.exit_code

    try:
        with lock_ctx as child_env:
            candidate = fingerprint_candidate(
                repo, files, base_ref=base_ref, resolved_base_sha=resolved_base_sha,
                merge_base_sha=merge_base_sha, include_quality_configs=include_quality_configs,
            )
            execution_head = _git_output(repo, "rev-parse", "HEAD")
            if execution_head is None:
                print("HEAD commit is unavailable", file=sys.stderr)
                return 2

            command_summary = ";".join(step.display for step in steps)
            plan_lines = "".join(
                f"{step.display}|{step.cost_class}|{'true' if step.requires_heavy else 'false'}|{'true' if step.requires_docker else 'false'}\n"
                for step in steps
            )
            plan_hash = hashlib.sha256(plan_lines.encode("utf-8")).hexdigest()
            env_hash = hashlib.sha256(f"{environment_detail}\n".encode("utf-8")).hexdigest()

            receipt_dir = _resolve_verify_dir(repo)
            receipt_path = receipt_dir / f"{candidate}-{plan_hash}-{env_hash}.receipt"

            sys.stdout.write(plan_text)
            sys.stdout.flush()

            if receipt_path.is_file() and os.environ.get("VERIFY_FORCE") != "1":
                os.utime(receipt_path, None)
                prune_verify_dir(receipt_dir)
                print(f"reusing exact passing receipt: {receipt_path}")
                sys.stdout.write(receipt_path.read_text(encoding="utf-8"))
                return 0

            receipt_dir.mkdir(parents=True, exist_ok=True)
            fd, attempt_str = tempfile.mkstemp(prefix=f"attempt-{candidate[:12]}.", dir=receipt_dir)
            attempt_path = Path(attempt_str)
            try:
                header_lines = [
                    f"candidate: {candidate}\n",
                    f"base_ref: {base_ref}\nresolved_base_sha: {resolved_base_sha}\nmerge_base_sha: {merge_base_sha}\n",
                    f"environment: {environment_detail}\nattempt_state: running\n",
                    plan_text,
                ]
                for idx, step in enumerate(steps, start=1):
                    _, step_display = prepare_command(step.kind, step.argument, execution_head=execution_head, ci_mode=ci_mode)
                    header_lines.append(f"step: {idx}\ncommand: {step_display}\nstep_state: {idx} pending\n")
                with os.fdopen(fd, "w", encoding="utf-8") as handle:
                    handle.writelines(header_lines)

                print(f"verification attempt: {attempt_path}", flush=True)
                scope_csv = ",".join(files)
                started = int(time.time())

                for idx, step in enumerate(steps, start=1):
                    cmd_started = int(time.time())
                    with attempt_path.open("a", encoding="utf-8") as handle:
                        handle.write(f"step_state: {idx} running started_at={cmd_started}\n")
                    print(f"==> {step.display}", flush=True)
                    step_cmd, _ = prepare_command(step.kind, step.argument, execution_head=execution_head, ci_mode=ci_mode)
                    proc = subprocess.run(step_cmd, cwd=repo, env=child_env, check=False)
                    step_duration = int(time.time()) - cmd_started

                    if proc.returncode == 0:
                        candidate_after = fingerprint_candidate(
                            repo, files, base_ref=base_ref, resolved_base_sha=resolved_base_sha,
                            merge_base_sha=merge_base_sha, include_quality_configs=include_quality_configs,
                        )
                        if candidate_after != candidate:
                            with attempt_path.open("a", encoding="utf-8") as handle:
                                handle.write(f"step_state: {idx} invalidated\nattempt_state: invalidated\n")
                            print(f"verification candidate changed during {step.display}; see {attempt_path}", file=sys.stderr)
                            return 1
                        with attempt_path.open("a", encoding="utf-8") as handle:
                            handle.write(f"step_state: {idx} passed duration={step_duration}s\n")
                        print(f"<== {step.display} ({step_duration}s)", flush=True)
                    else:
                        total_duration = int(time.time()) - started
                        with attempt_path.open("a", encoding="utf-8") as handle:
                            handle.write(
                                f"step_state: {idx} failed exit_code={proc.returncode} duration={step_duration}s\n"
                                "attempt_state: failed\n"
                            )
                        print(
                            "claim: surface-aware verification\n"
                            "result: fail\n"
                            f"candidate: {candidate}\n"
                            f"scope: {scope_csv}\n"
                            f"command: {step.display}\n"
                            f"inputs: {scope_csv}\n"
                            f"environment: {environment_detail}\n"
                            f"duration: {total_duration}s\n"
                            "status: not_verified\n"
                            "gap_or_next_owner: failed command",
                            file=sys.stderr,
                        )
                        print(f"partial results and pending plan: {attempt_path}", file=sys.stderr)
                        return 1

                total_duration = int(time.time()) - started
                candidate_after = fingerprint_candidate(
                    repo, files, base_ref=base_ref, resolved_base_sha=resolved_base_sha,
                    merge_base_sha=merge_base_sha, include_quality_configs=include_quality_configs,
                )
                if candidate_after != candidate:
                    with attempt_path.open("a", encoding="utf-8") as handle:
                        handle.write("attempt_state: invalidated\n")
                    print(
                        "claim: surface-aware verification\n"
                        "result: invalidated\n"
                        f"candidate: {candidate}\n"
                        "status: not_verified\n"
                        "gap_or_next_owner: candidate changed during verification",
                        file=sys.stderr,
                    )
                    return 1

                receipt_lines = [
                    "claim: surface-aware verification\n",
                    "result: pass\n",
                    f"candidate: {candidate}\n",
                    f"base_ref: {base_ref}\nresolved_base_sha: {resolved_base_sha}\nmerge_base_sha: {merge_base_sha}\n",
                    f"scope: {scope_csv}\n",
                    f"command: make verify [{command_summary}]\n",
                    f"inputs: {scope_csv}\n",
                    f"environment: {environment_detail}\n",
                    f"duration: {total_duration}s\n",
                ]
                if ci_mode == "none":
                    receipt_lines.append("ci_mode: none\n")
                receipt_lines.append("status: partially_verified\n" if ci_owned_summary else "status: verified\n")
                if not_applicable:
                    receipt_lines.append(f"not_applicable: {';'.join(not_applicable)}\n")
                if ci_owned_summary:
                    receipt_lines.append(f"ci_owned: {ci_owned_summary}\ngap_or_next_owner: CI\n")
                else:
                    receipt_lines.append("gap_or_next_owner: none\n")

                receipt_text = "".join(receipt_lines)
                receipt_tmp = Path(f"{attempt_path}.receipt.tmp")
                try:
                    receipt_tmp.write_text(receipt_text, encoding="utf-8")
                    os.replace(receipt_tmp, receipt_path)
                finally:
                    receipt_tmp.unlink(missing_ok=True)

                with attempt_path.open("a", encoding="utf-8") as handle:
                    handle.write(f"attempt_state: passed\nreceipt: {receipt_path}\n")
                sys.stdout.write(receipt_text)
                return 0
            finally:
                prune_verify_dir(receipt_dir)
    except _lock.LockError as error:
        print(str(error), file=sys.stderr)
        return error.exit_code


def main(argv: Sequence[str] | None = None) -> int:
    return run(argv)


if __name__ == "__main__":
    raise SystemExit(main())
