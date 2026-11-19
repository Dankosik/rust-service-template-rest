#!/usr/bin/env python3
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tomllib


HERE = Path(__file__).resolve().parent
PACKAGE = "infra-oauth2-client-credentials"
EXTRA = {
    "http-body-util": '{ workspace = true }',
    "opentelemetry": '{ workspace = true, features = ["trace"] }',
    "opentelemetry_sdk": '{ workspace = true, features = ["trace"] }',
    "prost": '{ workspace = true, features = ["derive"] }',
    "rustix": '{ version = "=1.1.6", default-features = false, features = ["std", "time"] }',
    "tracing-opentelemetry": '{ workspace = true }',
}


def command(*args, cwd=None, capture=True, env=None):
    return subprocess.run(args, cwd=cwd, check=True, text=True, capture_output=capture, env=env).stdout


def digest(path):
    with path.open("rb") as file:
        checksum = hashlib.sha256()
        for block in iter(lambda: file.read(1024 * 1024), b""):
            checksum.update(block)
    return checksum.hexdigest()


def mount_example(source):
    package = source / "crates" / PACKAGE
    examples = package / "examples" / "outbound_perf"
    shutil.copytree(HERE / "caller", examples)
    manifest = package / "Cargo.toml"
    content = manifest.read_text()
    if content.count("[dev-dependencies]\n") != 1:
        raise ValueError("the experiment mount needs one dev-dependencies table")
    original = tomllib.loads(content)
    if set(EXTRA) & set(original["dev-dependencies"]):
        raise ValueError("experiment dependencies already present in caller package")
    additions = "".join(f"{key} = {value}\n" for key, value in EXTRA.items())
    manifest.write_text(content.replace("[dev-dependencies]\n", "[dev-dependencies]\n" + additions))

    lock = source / "Cargo.lock"
    original_lock = lock.read_text()
    packages = tomllib.loads(original_lock)["package"]
    for name in EXTRA:
        if sum(package["name"] == name for package in packages) != 1:
            raise ValueError(f"expected one prelocked version of {name}")
    pattern = re.compile(r'(\[\[package\]\]\nname = "' + PACKAGE + r'"\nversion = "[^"]+"\ndependencies = \[\n)(.*?)(\n\])', re.S)
    match = pattern.search(original_lock)
    if not match:
        raise ValueError("caller package missing from original lock")
    entries = list(match.group(2).splitlines())
    entries.extend(f' "{name}",' for name in EXTRA if f' "{name}",' not in entries)
    mounted_lock = original_lock[:match.start(2)] + "\n".join(sorted(entries)) + original_lock[match.end(2):]
    lock.write_text(mounted_lock)
    return hashlib.sha256(original_lock.encode()).hexdigest(), digest(lock)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="Git repository containing the exact frozen revision")
    parser.add_argument("--revision", required=True)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--variant", choices=[item.stem for item in sorted((HERE / "variants").glob("*.patch"))])
    parser.add_argument("--check", action="store_true", help="Compile-check the mounted source without building a performance binary")
    parser.add_argument("--retry-probe", action="store_true", help="Inject one unstarted send for a behavior-only Host comparison")
    parser.add_argument("--heap-symbols", action="store_true", help="Use an independent matched release build with line tables and symbol stripping disabled")
    args = parser.parse_args()
    if not args.check and sys.platform != "linux":
        parser.error("release experiment artifacts must be built on Linux")
    original = args.source.resolve(strict=True)
    revision = command("git", "rev-parse", "--verify", f"{args.revision}^{{commit}}", cwd=original).strip()
    destination = args.destination.resolve()
    destination.mkdir(parents=True)
    source = destination / "source"
    source.mkdir()
    archive = destination / "source.tar"
    command("git", "archive", "--format=tar", "--output", str(archive), revision, cwd=original)
    command("tar", "-xf", str(archive), "-C", str(source), capture=False)
    archive.unlink()
    patch = HERE / "variants" / f"{args.variant}.patch" if args.variant else None
    if patch:
        command("git", "apply", "--check", "--whitespace=error", str(patch), cwd=source)
        command("git", "apply", "--whitespace=error", str(patch), cwd=source)
    retry_probe = HERE / "retry-probe.patch" if args.retry_probe else None
    if retry_probe:
        command("git", "apply", "--whitespace=error", str(retry_probe), cwd=source)
    lock_sha, mounted_lock_sha = mount_example(source)
    env = os.environ.copy()
    target = Path(env.get("CARGO_TARGET_DIR", str(destination / "target"))).resolve()
    env["CARGO_TARGET_DIR"] = str(target)
    # Exclude path names from source-location metadata and keep matched builds comparable.
    env["RUSTFLAGS"] = f"--remap-path-prefix={source}=/experiment-source"
    env.pop("CARGO_ENCODED_RUSTFLAGS", None)
    if args.heap_symbols:
        env["CARGO_PROFILE_RELEASE_DEBUG"] = "line-tables-only"
        env["CARGO_PROFILE_RELEASE_STRIP"] = "none"
    options = ["check"] if args.check else ["build", "--release"]
    cargo_args = ["cargo", *options, "--locked", "-p", PACKAGE, "--example", "outbound_perf"]
    rustc = command("rustc", "-Vv", cwd=source).strip()
    cargo = command("cargo", "-Vv", cwd=source).strip()
    sha = {
        "source_commit": revision,
        "variant": args.variant,
        "variant_sha256": digest(patch) if patch else None,
        "retry_probe_sha256": digest(retry_probe) if retry_probe else None,
        "original_lock_sha256": lock_sha,
        "mounted_lock_sha256": mounted_lock_sha,
        "overlay_sha256": {
            str(path.relative_to(HERE)): digest(path)
            for path in sorted(HERE.rglob("*"))
            if path.is_file() and path.suffix != ".pyc" and "__pycache__" not in path.parts
        },
        "rustc": rustc,
        "cargo": cargo,
        "host_platform": platform.platform(),
        "cargo_args": cargo_args,
        "rustflags": env["RUSTFLAGS"],
        "heap_symbols": args.heap_symbols,
        "profile_overrides": {key: value for key, value in env.items() if key.startswith("CARGO_PROFILE_")},
        "target": env.get("CARGO_BUILD_TARGET"),
        "cargo_target_dir": str(target),
        "allocator": "Rust System allocator",
    }
    receipt = destination / "build.json"
    receipt.write_text(json.dumps(sha, indent=2) + "\n")
    command(*cargo_args, cwd=source, capture=False, env=env)
    if not args.check:
        binary_dir = target / env["CARGO_BUILD_TARGET"] if "CARGO_BUILD_TARGET" in env else target
        binary = destination / "outbound_perf"
        shutil.copy2(binary_dir / "release" / "examples" / "outbound_perf", binary)
        sha["binary_sha256"] = digest(binary)
    sha["build_succeeded"] = True
    receipt.write_text(json.dumps(sha, indent=2) + "\n")


if __name__ == "__main__":
    main()
