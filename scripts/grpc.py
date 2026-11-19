#!/usr/bin/env python3
"""Generate and verify protobuf Rust contracts from api/proto/."""

from __future__ import annotations

import argparse
import filecmp
import os
import shutil
import tempfile
from collections.abc import Sequence
from pathlib import Path

from scripts import lib


def _codegen_env() -> dict[str, str]:
    env = os.environ.copy()
    env.setdefault("CARGO_TARGET_DIR", str(lib.ROOT / "target/grpc-codegen"))
    return env


def cmd_generate(output_dir: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="grpc-codegen-") as tmp:
        staging = Path(tmp)
        descriptors = staging / "descriptors.binpb"
        rust_out = staging / "rust"
        lib.run(["buf", "build", str(lib.ROOT / "api/proto"), "--as-file-descriptor-set", "--output", str(descriptors)])
        lib.run(
            ["cargo", "run", "--locked", "--manifest-path", str(lib.ROOT / "tools/grpc-codegen/Cargo.toml"), "--", str(descriptors), str(rust_out)],
            env=_codegen_env(),
        )
        shutil.copy2(descriptors, rust_out / "file_descriptor_set.binpb")
        if output_dir.exists():
            shutil.rmtree(output_dir)
        output_dir.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(rust_out, output_dir)


def _dirs_match(left: Path, right: Path) -> list[str]:
    cmp = filecmp.dircmp(left, right)
    diffs: list[str] = []
    diffs.extend(f"only in {left}: {name}" for name in cmp.left_only)
    diffs.extend(f"only in {right}: {name}" for name in cmp.right_only)
    diffs.extend(f"differs: {name}" for name in cmp.diff_files)
    diffs.extend(f"funny: {name}" for name in cmp.funny_files)
    for name in cmp.common_dirs:
        diffs.extend(_dirs_match(left / name, right / name))
    return diffs


def cmd_check(base_ref: str) -> None:
    probe = lib.run(["git", "rev-parse", "--verify", f"{base_ref}^{{commit}}"], capture=True, check=False)
    if probe.returncode != 0:
        raise lib.ScriptError(f"grpc-check: comparison base is unavailable: {base_ref}", exit_code=2)
    base_commit = probe.stdout.strip()

    lib.run(["buf", "format", "--exit-code", "--diff"])
    lib.run(["buf", "lint"])

    with tempfile.TemporaryDirectory(prefix="grpc-check-") as tmp:
        generated = Path(tmp) / "generated"
        cmd_generate(generated)
        diffs = _dirs_match(lib.ROOT / "crates/grpc-contracts/src/generated", generated)
        if diffs:
            raise lib.ScriptError("generated protobuf code is stale:\n" + "\n".join(diffs))

    base_proto = lib.git("ls-tree", "--name-only", base_commit, "--", "api/proto").strip()
    if base_proto:
        lib.run(["buf", "breaking", "--against", f".git#ref={base_commit}", "--exclude-imports"])
    else:
        print(f"grpc-check: {base_ref} has no protobuf contract; compatibility baseline absent")
    print("grpc-check: passed")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    gen = sub.add_parser("generate")
    gen.add_argument("output_dir", nargs="?", type=Path, default=lib.ROOT / "crates/grpc-contracts/src/generated")
    chk = sub.add_parser("check")
    chk.add_argument("--base", default=os.environ.get("GRPC_BASE_REF") or os.environ.get("BASE_REF") or "origin/main")
    args = parser.parse_args(argv)

    def _dispatch() -> None:
        if args.command == "generate":
            cmd_generate(args.output_dir)
        else:
            cmd_check(args.base)

    return lib.run_cli(_dispatch)


if __name__ == "__main__":
    raise SystemExit(main())
