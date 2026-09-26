#!/usr/bin/env python3
"""Resolve the pinned gRPC profile compiler without a system-protoc fallback."""

from __future__ import annotations

import argparse
import hashlib
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import zipfile
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator


ROOT = Path(__file__).resolve().parent.parent
PINS = ROOT / "tools" / "versions.env"
DOWNLOAD_ROOT = "https://github.com/protocolbuffers/protobuf/releases/download"


class ResolverError(RuntimeError):
    """A managed compiler cannot be safely selected or installed."""


def load_pins(path: Path = PINS) -> dict[str, str]:
    pins: dict[str, str] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        name, separator, value = line.partition("=")
        if not separator or not name or not value:
            raise ResolverError(f"invalid tool pin line in {path}: {raw}")
        pins[name] = value
    return pins


def host_spec(system: str | None = None, machine: str | None = None) -> tuple[str, str]:
    system = system or sys.platform
    machine = machine or platform.machine()
    systems = {"linux": "linux", "darwin": "osx"}
    architectures = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch_64", "arm64": "aarch_64"}
    host_os = systems.get(system.lower())
    host_arch = architectures.get(machine.lower())
    if host_os is None or host_arch is None:
        raise ResolverError(f"unsupported protoc build host: {system}/{machine}")
    return host_os, host_arch


def tool_root() -> Path:
    configured = os.environ.get("TOOLS_ROOT")
    if configured:
        root = Path(configured)
        if not root.is_absolute():
            raise ResolverError("TOOLS_ROOT must be an absolute managed tool cache path")
        return root
    result = subprocess.run(
        ["git", "-C", os.fspath(ROOT), "rev-parse", "--git-common-dir"],
        check=False,
        capture_output=True,
        text=True,
    )
    common = result.stdout.strip()
    if result.returncode != 0 or not common:
        raise ResolverError("cannot resolve the Git common tool cache; set absolute TOOLS_ROOT")
    common_path = Path(common)
    if not common_path.is_absolute():
        common_path = ROOT / common_path
    return common_path.resolve() / "tools"


def selection(pins: dict[str, str], system: str | None = None, machine: str | None = None) -> tuple[str, str, str, str]:
    host_os, host_arch = host_spec(system, machine)
    version = pins["PROTOC_VERSION"]
    checksum = pins[f"PROTOC_SHA256_{host_os.upper()}_{host_arch.upper()}"]
    archive = f"protoc-{version}-{host_os}-{host_arch}.zip"
    return host_os, host_arch, archive, checksum


def cache_entry(pins: dict[str, str]) -> Path:
    host_os, host_arch, _, _ = selection(pins)
    return tool_root() / f"protoc-{pins['PROTOC_VERSION']}" / f"{host_os}-{host_arch}"


def cached_protoc(pins: dict[str, str]) -> Path:
    entry = cache_entry(pins)
    executable = entry / "bin" / "protoc"
    includes = entry / "include"
    if executable.is_file() and os.access(executable, os.X_OK) and includes.is_dir():
        return executable
    raise ResolverError("managed protoc is not provisioned; run `make grpc-tools`")


@contextmanager
def entry_lock(entry: Path) -> Iterator[None]:
    entry.parent.mkdir(parents=True, exist_ok=True)
    lock_path = entry.with_suffix(".lock")
    with lock_path.open("a+", encoding="utf-8") as lock:
        try:
            import fcntl
        except ImportError as error:  # Supported hosts are Unix; retain a clear failure if that changes.
            raise ResolverError("managed protoc requires an advisory file-locking host") from error
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


def download(url: str, destination: Path, expected_sha256: str) -> None:
    digest = hashlib.sha256()
    try:
        with urllib.request.urlopen(url, timeout=60) as response, destination.open("wb") as output:
            while chunk := response.read(1024 * 1024):
                digest.update(chunk)
                output.write(chunk)
    except OSError as error:
        raise ResolverError(f"cannot download managed protoc from {url}: {error}") from error
    actual = digest.hexdigest()
    if actual != expected_sha256:
        raise ResolverError(f"managed protoc checksum mismatch: expected {expected_sha256}, got {actual}")


def install_archive(archive: Path, expected_sha256: str, entry: Path) -> Path:
    actual = hashlib.sha256(archive.read_bytes()).hexdigest()
    if actual != expected_sha256:
        raise ResolverError(f"managed protoc checksum mismatch: expected {expected_sha256}, got {actual}")
    if entry.exists():
        return cached_protoc_for(entry)
    temporary = Path(tempfile.mkdtemp(prefix=f".{entry.name}.", dir=entry.parent)).resolve()
    try:
        with zipfile.ZipFile(archive) as contents:
            for member in contents.infolist():
                target = (temporary / member.filename).resolve()
                if target != temporary and temporary not in target.parents:
                    raise ResolverError("managed protoc archive contains an unsafe path")
            contents.extractall(temporary)
        executable = cached_protoc_for(temporary)
        executable.chmod(executable.stat().st_mode | 0o111)
        os.replace(temporary, entry)
        return entry / "bin" / "protoc"
    except (OSError, zipfile.BadZipFile) as error:
        raise ResolverError(f"cannot install managed protoc: {error}") from error
    finally:
        if temporary.exists():
            shutil.rmtree(temporary)


def cached_protoc_for(entry: Path) -> Path:
    executable = entry / "bin" / "protoc"
    includes = entry / "include"
    if not executable.is_file() or not includes.is_dir():
        raise ResolverError(f"managed protoc archive has no complete compiler entry: {entry}")
    return executable


def provision(pins: dict[str, str]) -> Path:
    entry = cache_entry(pins)
    _, _, archive, checksum = selection(pins)
    if entry.exists():
        return cached_protoc_for(entry)
    with entry_lock(entry):
        if entry.exists():
            return cached_protoc_for(entry)
        temporary = Path(tempfile.mkdtemp(prefix=f".{entry.name}.download.", dir=entry.parent))
        try:
            path = temporary / archive
            url = f"{DOWNLOAD_ROOT}/v{pins['PROTOC_VERSION']}/{archive}"
            download(url, path, checksum)
            return install_archive(path, checksum, entry)
        finally:
            shutil.rmtree(temporary, ignore_errors=True)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--provision", action="store_true", help="download and verify the managed compiler for this build host")
    parser.add_argument("--print-host", action="store_true", help="print the selected official archive for this build host")
    args, remaining = parser.parse_known_args(argv)
    if args.provision and remaining:
        parser.error("--provision accepts no compiler arguments")
    if args.print_host and (args.provision or remaining):
        parser.error("--print-host cannot be combined with compiler arguments")
    try:
        pins = load_pins()
        if args.print_host:
            _, _, archive, _ = selection(pins)
            print(archive)
            return 0
        if args.provision:
            print(provision(pins))
            return 0
        os.execv(os.fspath(cached_protoc(pins)), [os.fspath(cached_protoc(pins)), *remaining])
    except ResolverError as error:
        print(f"grpc-protoc: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
