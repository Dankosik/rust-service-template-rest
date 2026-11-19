#!/usr/bin/env python3
"""Container image verification, build, hardened smoke-test (+ migration rehearsal), Trivy scan, and SBOM."""

from __future__ import annotations

import argparse
import io
import json
import os
import re
import shutil
import tarfile
import tempfile
import time
import tomllib
from collections.abc import Sequence
from pathlib import Path

from scripts import lib

_JEMALLOC_OPT = b"background_thread:true,thp:never"


def _builder_args() -> list[str]:
    name = os.environ.get("VALIDATION_BUILDER_NAME", "").strip()
    return ["--builder", name] if name else []


def check_dockerfile_toolchain(root: Path = lib.ROOT) -> None:
    toolchain = tomllib.loads((root / "rust-toolchain.toml").read_text(encoding="utf-8"))["toolchain"]["channel"]
    dockerfile = (root / "build/docker/Dockerfile").read_text(encoding="utf-8")
    match = re.search(r"^FROM\s+rust:([0-9]+\.[0-9]+\.[0-9]+)-", dockerfile, re.M)
    found = match.group(1) if match else "<missing>"
    if found != toolchain:
        raise lib.ScriptError(f"Dockerfile FROM rust:{found} disagrees with rust-toolchain.toml ({toolchain})")


def cmd_dockerfile_check() -> None:
    check_dockerfile_toolchain(lib.ROOT)
    lib.run(["docker", "buildx", "build", *_builder_args(), "--check", "-f", "build/docker/Dockerfile", "."])


def cmd_build(image: str) -> None:
    service_package, app_version = lib.read_service_meta()
    vcs_ref = os.environ.get("VCS_REF") or lib.run(["git", "rev-parse", "HEAD"], capture=True, check=False).stdout.strip() or "unknown"
    epoch = os.environ.get("SOURCE_DATE_EPOCH") or lib.run(["git", "log", "-1", "--format=%ct"], capture=True, check=False).stdout.strip() or "0"

    args = [
        "--build-arg", f"SERVICE_PACKAGE={os.environ.get('SERVICE_PACKAGE') or service_package}",
        "--build-arg", f"SERVICE_BIN={os.environ.get('SERVICE_BIN') or os.environ.get('SERVICE_PACKAGE') or service_package}",
        "--build-arg", f"APP_VERSION={os.environ.get('APP_VERSION') or app_version}",
        "--build-arg", f"VCS_REF={vcs_ref}",
        "--build-arg", f"SOURCE_URL={os.environ.get('SOURCE_URL', '')}",
        "--build-arg", f"SOURCE_DATE_EPOCH={epoch}",
        "--build-arg", f"IMAGE_TESTS={os.environ.get('IMAGE_TESTS', 'none')}",
        "--build-arg", f"PGO={os.environ.get('PGO', 'none')}",
        "-f", "build/docker/Dockerfile",
    ]
    if cache_from := os.environ.get("RUNTIME_IMAGE_CACHE_FROM"):
        args.extend(["--cache-from", cache_from])
    if cache_to := os.environ.get("RUNTIME_IMAGE_CACHE_TO"):
        lib.run(["docker", "buildx", "build", *_builder_args(), *args, "--target", "cooked", "--cache-to", cache_to, "."])
    if metadata_file := os.environ.get("RUNTIME_IMAGE_METADATA_FILE"):
        args.extend(["--metadata-file", metadata_file])
    lib.run(["docker", "buildx", "build", *_builder_args(), "--load", *args, "-t", image, "."])


def _extract_from_container(container: str, path: str) -> bytes:
    raw = lib.run(["docker", "cp", f"{container}:{path}", "-"], capture=True, binary=True).stdout
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:*") as archive:
        for member in archive.getmembers():
            if member.isfile():
                extracted = archive.extractfile(member)
                return extracted.read() if extracted is not None else b""
    raise lib.ScriptError(f"{path} not found as regular file in {container}")


def _retained_and_pruned_binaries() -> tuple[list[str], list[str]]:
    retained, pruned = ["/service"], []
    for crate, binary in (("crates/migrate", "/migrate"), ("crates/jobs-worker", "/jobs-worker")):
        (retained if (lib.ROOT / crate).is_dir() else pruned).append(binary)
    return retained, pruned


def _check_expected_binaries(image_id: str) -> None:
    container = f"image-bins-check-{os.getpid()}-{int(time.time())}"
    retained, pruned = _retained_and_pruned_binaries()
    try:
        lib.run(["docker", "create", "--pull", "never", "--name", container, "--entrypoint", "/service", image_id], capture=True)
        with tempfile.TemporaryDirectory(prefix="image-bins-") as tmp:
            dst = Path(tmp) / "bin"
            for binary in retained:
                lib.run(["docker", "cp", f"{container}:{binary}", str(dst)], capture=True)
                if not dst.is_file() or dst.stat().st_size == 0 or not os.access(dst, os.X_OK):
                    raise lib.ScriptError(f"{binary} is not an executable file")
                dst.unlink()
            for binary in pruned:
                probe = lib.run(["docker", "cp", f"{container}:{binary}", str(dst)], capture=True, check=False)
                if probe.returncode == 0:
                    raise lib.ScriptError(f"pruned binary unexpectedly present in image: {binary}")
    finally:
        lib.run(["docker", "rm", "-f", "-v", container], capture=True, check=False)


def _json_log_lines(text: str) -> list[dict[str, object]]:
    events: list[dict[str, object]] = []
    for line in text.splitlines():
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict):
            events.append(obj)
    return events


def _smoke_check(
    image_id: str,
    expected_commit: str,
    *,
    network: str | None = None,
    postgres_dsn: str | None = None,
    app_env: str | None = None,
    allow_insecure_dev: bool = False,
) -> None:
    _check_expected_binaries(image_id)
    container = f"service-runtime-check-{os.getpid()}-{int(time.time())}"
    worker = f"{container}-jobs-worker"
    worker_created = False
    try:
        docker_args = [
            "docker", "run", "-d", "--name", container,
            "--label", f"runtime-check={container}",
            "-p", "127.0.0.1::8080",
            "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges",
        ]
        if network:
            docker_args.extend(["--network", network])
        if postgres_dsn:
            docker_args.extend([
                "-e", "APP__POSTGRES__ENABLED=true",
                "-e", f"APP__APP__ALLOW_INSECURE_DEVELOPMENT={'true' if allow_insecure_dev else 'false'}",
                "-e", f"APP__POSTGRES__DSN={postgres_dsn}",
            ])
            if app_env:
                docker_args.extend(["-e", f"APP__APP__ENV={app_env}"])
        docker_args.append(image_id)
        lib.run(docker_args, capture=True)

        if not _extract_from_container(container, "/usr/share/licenses/service/LICENSE").strip():
            raise lib.ScriptError("runtime image missing LICENSE")
        notices = _extract_from_container(container, "/usr/share/licenses/service/THIRD-PARTY-RUST.txt").decode("utf-8", errors="replace")
        if "Copyright (c) Tokio Contributors" not in notices.splitlines():
            raise lib.ScriptError("runtime image missing THIRD-PARTY-RUST.txt notices")

        addr = lib.run(["docker", "port", container, "8080/tcp"], capture=True).stdout.strip()
        port = addr.splitlines()[0].rsplit(":", 1)[-1] if addr else ""
        if not port.isdigit():
            raise lib.ScriptError(f"failed to resolve service port for {container}")

        try:
            lib.wait_http_ready(f"http://127.0.0.1:{port}/health/ready", container=container, timeout_s=45, req_timeout_s=2.0)
        except lib.ScriptError:
            logs = lib.run(["docker", "logs", container], capture=True, check=False)
            raise lib.ScriptError(f"runtime image did not become ready\n{logs.stdout}{logs.stderr}")

        logs_proc = lib.run(["docker", "logs", container], capture=True)
        logs = f"{logs_proc.stdout}\n{logs_proc.stderr}"
        events = _json_log_lines(logs)
        starting = next((ev for ev in events if ev.get("message") == "service_starting"), None)
        if starting is None:
            raise lib.ScriptError(f"missing service_starting log\n{logs}")
        if expected_commit and starting.get("app.commit") != expected_commit:
            raise lib.ScriptError(f"app.commit mismatch: expected {expected_commit}, got {starting}")
        if postgres_dsn and not any(ev.get("message") == "postgres_pool_opened" for ev in events):
            raise lib.ScriptError(f"runtime image did not open postgres pool\n{logs}")
        print(f"runtime image ready: {json.dumps({'version': starting.get('app.version'), 'commit': starting.get('app.commit')})}")

        if _JEMALLOC_OPT not in _extract_from_container(container, "/service"):
            raise lib.ScriptError("/service does not carry jemalloc options")
        buildkit_image = lib.mise_env("BUILDKIT_IMAGE")
        lib.run([
            "docker", "run", "--rm", "--read-only", "--network", "none",
            "--cap-drop=ALL", "--security-opt=no-new-privileges",
            "--user", "65532:65532", f"--pid=container:{container}",
            "--entrypoint", "/bin/sh", buildkit_image,
            "-c", "grep -Fxq jemalloc_bg_thd /proc/[0-9]*/task/*/comm",
        ])

        started = time.monotonic()
        lib.run(["docker", "stop", "--time", "45", container], capture=True)
        elapsed = int(time.monotonic() - started)
        exit_code = lib.run(["docker", "inspect", "-f", "{{.State.ExitCode}}", container], capture=True).stdout.strip()
        if exit_code != "0":
            raise lib.ScriptError(f"non-zero SIGTERM exit ({exit_code})")
        print(f"runtime image stopped cleanly in {elapsed}s")

        if (lib.ROOT / "crates/jobs-worker").is_dir():
            worker_created = True
            lib.run([
                "docker", "create", "--name", worker,
                "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges",
                "--network", "none", "--entrypoint", "/jobs-worker", image_id,
            ], capture=True)
            if _JEMALLOC_OPT not in _extract_from_container(worker, "/jobs-worker"):
                raise lib.ScriptError("/jobs-worker does not carry jemalloc options")
            res = lib.run(["docker", "start", "--attach", worker], capture=True, check=False)
            worker_out = f"{res.stdout}{res.stderr}"
            worker_exit = lib.run(["docker", "inspect", "-f", "{{.State.ExitCode}}", worker], capture=True).stdout.strip()
            expected_msg = (
                "postgres.enabled must be true to run the jobs worker"
                if (lib.ROOT / "crates/infra-webhooks").is_dir() or (lib.ROOT / "crates/infra-messaging/src/outbox.rs").is_file()
                else "is registered: register this service's retained capabilities"
            )
            if worker_exit != "1" or expected_msg not in worker_out:
                raise lib.ScriptError(f"jobs-worker unexpected startup refusal (exit {worker_exit})\n{worker_out}")
            print("jobs-worker refused before dependency I/O")
    finally:
        if worker_created:
            lib.run(["docker", "rm", "-f", "-v", worker], capture=True, check=False)
        lib.run(["docker", "rm", "-f", "-v", container], capture=True, check=False)


def cmd_check(image: str, expected_commit: str, *, with_migrations: bool) -> None:
    lib.require_docker()
    image_id = lib.run(["docker", "image", "inspect", "--format", "{{.Id}}", image], capture=True).stdout.strip()
    if not with_migrations:
        _smoke_check(image_id, expected_commit)
        return

    with lib.ComposeStack("service-migration") as stack:
        stack.up("postgres", 5432)
        dsn = "postgres://app:app@postgres:5432/app?sslmode=disable"
        pg_env = [
            "-e", "APP__APP__ENV=local",
            "-e", "APP__APP__ALLOW_INSECURE_DEVELOPMENT=true",
            "-e", "APP__POSTGRES__ENABLED=true",
            "-e", f"APP__POSTGRES__DSN={dsn}",
        ]
        if list((lib.ROOT / "migrations").glob("*.sql")):
            hist = f"service-migration-history-{os.getpid()}"
            try:
                lib.run([
                    "docker", "run", "-d", "--name", hist, "--network", stack.network,
                    "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges", *pg_env, image_id,
                ], capture=True)
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    if lib.run(["docker", "inspect", "-f", "{{.State.Running}}", hist], capture=True).stdout.strip() != "true":
                        break
                    time.sleep(0.1)
                else:
                    raise lib.ScriptError("service did not refuse pending migrations within 30s")
                hist_exit = lib.run(["docker", "inspect", "-f", "{{.State.ExitCode}}", hist], capture=True).stdout.strip()
                hist_logs = lib.run(["docker", "logs", hist], capture=True)
                combined = f"{hist_logs.stdout}{hist_logs.stderr}"
                if hist_exit == "0" or "postgres migration history: embedded migrations are pending" not in combined:
                    raise lib.ScriptError(f"service did not refuse missing migration history (exit {hist_exit})\n{combined}")
                print("service refused missing migration history before migration")
            finally:
                lib.run(["docker", "rm", "-f", "-v", hist], capture=True, check=False)

        for label, allowed_outcomes in (("first", {"success", "no_change"}), ("second", {"no_change"})):
            out = lib.run([
                "docker", "run", "--rm", "--name", f"service-migrate-{os.getpid()}-{label}",
                "--network", stack.network,
                "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges",
                *pg_env, "--entrypoint", "/migrate", image_id,
            ], capture=True)
            combined = f"{out.stdout}{out.stderr}"
            print(combined.strip())
            if not any(ev.get("message") == "migration_run" and ev.get("outcome") in allowed_outcomes for ev in _json_log_lines(combined)):
                raise lib.ScriptError(f"{label} migration run did not produce {sorted(allowed_outcomes)}")
        print("migration rehearsal: embedded set applied, replay is no_change")

        _smoke_check(image_id, expected_commit, network=stack.network, postgres_dsn=dsn, app_env="local", allow_insecure_dev=True)


def cmd_trivy(mode: str, image: str, output: Path) -> None:
    lib.require_docker()
    trivy_image = lib.mise_env("TRIVY_IMAGE")
    image_id = lib.run(["docker", "image", "inspect", "--format", "{{.Id}}", image], capture=True).stdout.strip()
    _check_expected_binaries(image_id)

    trivy_cache = os.environ.get("TRIVY_CACHE_VOLUME", "trivy-cache")
    trivy_base = [
        "docker", "run", "--rm", "--read-only", "--cap-drop=ALL",
        "--security-opt=no-new-privileges", "--tmpfs", "/tmp:rw,noexec,nosuid,nodev",
    ]
    with tempfile.TemporaryDirectory(prefix="image-scan-") as tmp:
        tmp_dir = Path(tmp)
        tar_path = tmp_dir / "image.tar"
        tar_path.write_bytes(lib.run(["docker", "image", "save", image_id], capture=True, binary=True).stdout)
        tar_path.chmod(0o444)

        dl_cmd = [*trivy_base, "-v", f"{trivy_cache}:/root/.cache/trivy"]
        if db_repo := os.environ.get("TRIVY_DB_REPOSITORY"):
            dl_cmd.extend(["-e", f"TRIVY_DB_REPOSITORY={db_repo}"])
        dl_cmd.extend([
            trivy_image, "image", "--cache-dir", "/root/.cache/trivy", "--quiet",
            "--skip-version-check", "--disable-telemetry", "--scanners", "vuln", "--download-db-only",
        ])
        lib.run(dl_cmd)

        native_bytes = lib.run([
            *trivy_base, "--network", "none",
            "-v", f"{trivy_cache}:/root/.cache/trivy:ro",
            "-v", f"{tar_path}:/input/image.tar:ro",
            trivy_image, "image", "--cache-dir", "/root/.cache/trivy", "--quiet",
            "--skip-version-check", "--disable-telemetry", "--cache-backend", "memory",
            "--skip-db-update", "--skip-java-db-update", "--offline-scan",
            "--scanners", "vuln", "--list-all-pkgs=true", "--format", "json", "--input", "/input/image.tar",
        ], capture=True, binary=True).stdout

        report = json.loads(native_bytes)
        retained, _ = _retained_and_pruned_binaries()
        results = report.get("Results") or []
        for binary in retained:
            name = binary.lstrip("/")
            found = any(
                r.get("Type") == "rustbinary"
                and str(r.get("Target") or "").lstrip("/") == name
                and len(r.get("Packages") or []) >= 1
                for r in results
            )
            if not found:
                raise lib.ScriptError(f"Trivy report missing cargo-auditable rustbinary inventory for /{name}")

        if mode == "scan":
            lib.run(
                [*trivy_base, "--network", "none", "-i", trivy_image, "convert",
                 "--quiet", "--scanners", "vuln", "--severity", "HIGH,CRITICAL", "--format", "table", "/dev/stdin"],
                input_bytes=native_bytes,
                binary=True,
            )
            fixable = [
                v for r in results for v in (r.get("Vulnerabilities") or [])
                if v.get("Severity") in {"HIGH", "CRITICAL"} and v.get("FixedVersion")
            ]
            if fixable:
                raise lib.ScriptError(f"Security scan found {len(fixable)} fixable HIGH/CRITICAL vulnerabilities in {image_id}")
        else:
            sbom_bytes = lib.run(
                [*trivy_base, "--network", "none", "-i", trivy_image, "convert", "--quiet", "--format", "cyclonedx", "/dev/stdin"],
                input_bytes=native_bytes,
                capture=True,
                binary=True,
            ).stdout
            out_tmp = tmp_dir / "sbom.cdx.json"
            out_tmp.write_bytes(sbom_bytes)
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(out_tmp), str(output))


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("dockerfile-check")
    b = sub.add_parser("build")
    b.add_argument("image", nargs="?", default="service:ci")
    c = sub.add_parser("check")
    c.add_argument("image")
    c.add_argument("expected_commit", nargs="?", default="")
    c.add_argument("--with-migrations", action="store_true")
    s = sub.add_parser("scan")
    s.add_argument("image")
    sb = sub.add_parser("sbom")
    sb.add_argument("image")
    sb.add_argument("output", nargs="?", type=Path, default=Path("sbom.cdx.json"))

    args = parser.parse_args(argv)

    def _dispatch() -> None:
        if args.command == "dockerfile-check":
            cmd_dockerfile_check()
        elif args.command == "build":
            cmd_build(args.image)
        elif args.command == "check":
            cmd_check(args.image, args.expected_commit, with_migrations=args.with_migrations)
        elif args.command == "scan":
            cmd_trivy("scan", args.image, Path("sbom.cdx.json"))
        elif args.command == "sbom":
            cmd_trivy("sbom", args.image, args.output)

    return lib.run_cli(_dispatch)


if __name__ == "__main__":
    raise SystemExit(main())
