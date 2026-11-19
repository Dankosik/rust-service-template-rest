#!/usr/bin/env python3
"""Run the Linux cgroup v2 CPU-quota runtime progress proof (source-template benchmark)."""

from __future__ import annotations

import hashlib
import json
import os
import platform
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from scripts import lib


def _sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run_proof() -> int:
    if platform.system() != "Linux":
        raise lib.ScriptError("runtime progress proof requires Linux", exit_code=2)
    for cmd in ("cargo", "rustc", "docker", "timeout"):
        if shutil.which(cmd) is None:
            raise lib.ScriptError(f"runtime progress proof requires {cmd}", exit_code=2)

    if lib.run(["git", "diff", "--quiet", "HEAD", "--"], check=False).returncode != 0:
        raise lib.ScriptError("freeze tracked changes before quota proof", exit_code=2)
    untracked = lib.git(
        "ls-files", "--others", "--exclude-standard", "--",
        "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "crates", "vendor", "test", ".cargo", ".sqlx", "migrations",
    ).strip()
    if untracked:
        raise lib.ScriptError("freeze untracked build inputs before quota proof", exit_code=2)

    source_rev = lib.git("rev-parse", "HEAD").strip()
    run_id = f"{source_rev[:12]}-{int(time.time())}-{os.getpid()}"
    results = Path(os.environ.get("RUNTIME_PROGRESS_RESULTS") or (lib.ROOT / f".artifacts/runtime-progress/{run_id}"))
    inputs = Path(f"{results}.inputs")
    if not results.is_absolute() or results.exists() or inputs.exists():
        raise lib.ScriptError("RUNTIME_PROGRESS_RESULTS must be a new absolute path", exit_code=2)

    (inputs / "image").mkdir(parents=True)
    image = f"runtime-progress-proof:{run_id}"
    image_id = ""
    container_prefix = f"runtime-progress-{run_id}"
    containers = [f"{container_prefix}-{s}" for s in ("ordinary", "blocked", "primary", "negative")]
    status = 0
    prev_handlers: dict[int, Any] = {}

    def _on_signal(signum: int, _frame: Any) -> None:
        raise SystemExit(128 + signum)

    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        prev_handlers[sig] = signal.getsignal(sig)
        signal.signal(sig, _on_signal)

    try:
        cgroup = lib.run(["timeout", "10s", "docker", "info", "--format", "{{.CgroupVersion}}"], capture=True).stdout.strip()
        (inputs / "cgroup-version").write_text(f"{cgroup}\n", encoding="utf-8")
        if cgroup != "2":
            raise lib.ScriptError("Docker cgroup v2 is required", exit_code=2)

        (inputs / "source-commit").write_text(f"{source_rev}\n", encoding="utf-8")
        archive_bytes = lib.git("archive", "--format=tar", "HEAD", binary=True)
        (inputs / "source-archive.sha256").write_text(f"{hashlib.sha256(archive_bytes).hexdigest()}  -\n", encoding="utf-8")
        (inputs / "build-inputs.sha256").write_text(
            f"{_sha256_file(lib.ROOT / 'Cargo.lock')}  Cargo.lock\n"
            f"{_sha256_file(lib.ROOT / 'rust-toolchain.toml')}  rust-toolchain.toml\n",
            encoding="utf-8",
        )
        (inputs / "rustc-version").write_text(lib.run(["rustc", "--version", "--verbose"], capture=True).stdout, encoding="utf-8")
        (inputs / "cargo-version").write_text(lib.run(["cargo", "--version"], capture=True).stdout, encoding="utf-8")
        (inputs / "kernel").write_text(lib.run(["uname", "-a"], capture=True).stdout, encoding="utf-8")

        dockerfile = (lib.ROOT / "build/docker/Dockerfile").read_text(encoding="utf-8")
        base_match = re.search(r"^FROM\s+(rust:[^\s]+@sha256:[0-9a-f]+)", dockerfile, re.M)
        if not base_match:
            raise lib.ScriptError("Dockerfile has no pinned Rust base", exit_code=2)
        base_image = base_match.group(1)
        (inputs / "base-image").write_text(f"{base_image}\n", encoding="utf-8")
        package, _ = lib.read_service_meta()

        build_env = {**os.environ, "SQLX_OFFLINE": "true", "VERGEN_GIT_SHA": source_rev}
        spec_test = lib.run(["cargo", "test", "--locked", "--package", package, "--example", "runtime_progress"], env=build_env, capture=True)
        (inputs / "specimen-tests.log").write_text(spec_test.stdout + spec_test.stderr, encoding="utf-8")

        build = lib.run(
            ["cargo", "build", "--locked", "--release", "--package", package, "--example", "runtime_progress", "--test", "runtime_progress", "--message-format=json"],
            env=build_env,
            capture=True,
        )
        (inputs / "release-build.jsonl").write_text(build.stdout, encoding="utf-8")
        (inputs / "release-build.stderr").write_text(build.stderr, encoding="utf-8")

        manifest_abs = str((lib.ROOT / "crates/service/Cargo.toml").resolve())
        specimen = driver = ""
        for line in build.stdout.splitlines():
            obj = json.loads(line)
            if obj.get("reason") == "compiler-artifact" and obj.get("manifest_path") == manifest_abs and obj.get("executable"):
                target = obj.get("target", {})
                if target.get("name") == "runtime_progress":
                    kinds = target.get("kind", [])
                    if "example" in kinds:
                        specimen = obj["executable"]
                    elif "test" in kinds:
                        driver = obj["executable"]
        if not specimen or not driver:
            raise lib.ScriptError("failed to locate runtime_progress executables")

        dst_bin = inputs / "image/runtime_progress"
        shutil.copy2(specimen, dst_bin)
        (inputs / "binary.sha256").write_text(f"{_sha256_file(dst_bin)}  {dst_bin}\n", encoding="utf-8")

        builder_args = ["--builder", b] if (b := os.environ.get("VALIDATION_BUILDER_NAME", "").strip()) else []
        img_build = lib.run([
            "docker", "buildx", "build", *builder_args, "--load",
            "--file", "build/docker/runtime-progress.Dockerfile",
            "--build-arg", f"RUNTIME_PROGRESS_BASE_IMAGE={base_image}",
            "--build-arg", f"RUNTIME_PROGRESS_SOURCE={source_rev}",
            "--build-arg", f"RUNTIME_PROGRESS_RUN={run_id}",
            "--tag", image, str(inputs / "image"),
        ], capture=True)
        (inputs / "image-build.log").write_text(img_build.stdout + img_build.stderr, encoding="utf-8")
        image_id = lib.run(["docker", "image", "inspect", "--format", "{{.Id}}", image], capture=True).stdout.strip()
        (inputs / "image-inspect.json").write_text(lib.run(["docker", "image", "inspect", image_id], capture=True).stdout, encoding="utf-8")

        test_name = "bounded_sources_preserve_process_progress_under_one_cpu_quota"
        listed = lib.run([driver, "--list", "--ignored"], capture=True).stdout
        (inputs / "driver-tests").write_text(listed, encoding="utf-8")
        if f"{test_name}: test" not in listed.splitlines():
            raise lib.ScriptError(f"driver missing expected ignored test {test_name}")

        driver_env = {
            **build_env,
            "RUNTIME_PROGRESS_CONTAINER_PREFIX": container_prefix,
            "RUNTIME_PROGRESS_IMAGE": image_id,
            "RUNTIME_PROGRESS_SOURCE": source_rev,
            "RUNTIME_PROGRESS_RESULTS": str(results),
        }
        res = subprocess.run(
            ["timeout", "--signal=TERM", "--kill-after=15s", "12m", driver, "--ignored", "--exact", test_name, "--no-capture"],
            cwd=lib.ROOT,
            env=driver_env,
            capture_output=True,
            text=True,
            check=False,
        )
        (inputs / "driver.log").write_text(res.stdout + res.stderr, encoding="utf-8")
        print(res.stdout, end="")
        if res.returncode != 0:
            raise lib.ScriptError(f"runtime progress driver exited {res.returncode}\n{res.stderr}")
    except lib.ScriptError as exc:
        print(f"error: {exc}", file=sys.stderr)
        status = exc.exit_code
    except SystemExit as exc:
        status = exc.code if isinstance(exc.code, int) else 1
    except BaseException:
        status = 1
        raise
    finally:
        cleanup_failed = False
        for container in containers:
            insp = lib.run(["timeout", "10s", "docker", "inspect", container], capture=True, check=False)
            if insp.returncode == 0:
                (inputs / f"{container}.cleanup-inspect.json").write_text(insp.stdout, encoding="utf-8")
                logs = lib.run(["timeout", "15s", "docker", "logs", container], capture=True, check=False)
                (inputs / f"{container}.stdout").write_text(logs.stdout, encoding="utf-8")
                (inputs / f"{container}.stderr").write_text(logs.stderr, encoding="utf-8")
                if logs.returncode != 0:
                    cleanup_failed = True
            rm_c = lib.run(["docker", "rm", "-f", "-v", container], capture=True, check=False)
            if insp.returncode == 0 and rm_c.returncode != 0:
                cleanup_failed = True
        if image_id and not cleanup_failed:
            rm = lib.run(["timeout", "10s", "docker", "image", "rm", image], capture=True, check=False)
            (inputs / "image-cleanup.log").write_text(rm.stdout + rm.stderr, encoding="utf-8")
            if rm.returncode != 0:
                cleanup_failed = True
        if cleanup_failed and status == 0:
            status = 1
        (inputs / "runner-exit-code").write_text(f"{status}\n", encoding="utf-8")
        for sig, handler in prev_handlers.items():
            signal.signal(sig, handler)
    return status


def main() -> int:
    return lib.run_cli(run_proof)


if __name__ == "__main__":
    raise SystemExit(main())
