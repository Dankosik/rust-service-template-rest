#!/usr/bin/env python3
"""Verify or refresh JetStream wire-compatibility fixtures against the pinned Go template codec (single clone)."""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import tempfile
from collections.abc import Sequence
from pathlib import Path

from scripts import lib

COMPAT_SOURCE_URL = "https://github.com/Dankosik/go-service-template-rest"
SOURCE_REVISION = "a50f43cf869b86accb3fdba24d758cc2d15a02df"
PINNED_HASHES = {
    "internal/infra/natsjs/message_wire.go": "c98d7c1db43f515e072e91416cf1baa6d0da5c898337d62be9fef3173ed9fe88",
    "internal/infra/natsjs/message_deadletter.go": "4063580a0322974331c13f3de19e3296b0c40d1785cbbf98d695dc74ef5b953c",
    "internal/domainevent/event.go": "add1371aa3ef73b26a9f1eb08d9b992f48969d4a8f6dbd256d81d6d1a058e556",
}
BRIDGE_SOURCE = lib.ROOT / "crates/infra-messaging/tests/fixtures/go-wire/go_wire_compat_bridge_test.go"
COMMITTED_FIXTURES = lib.ROOT / "crates/infra-messaging/tests/fixtures/go-wire/records.json"
COMMITTED_RUST_EXPORT = lib.ROOT / "crates/infra-messaging/tests/fixtures/go-wire/rust-export.json"


def _run_go_bridge(
    source_dir: Path,
    *,
    mode: str,
    fixture_file: Path,
    rust_export_file: Path | None = None,
) -> None:
    env = {
        **os.environ,
        "COMPAT_SOURCE_URL": COMPAT_SOURCE_URL,
        "COMPAT_SOURCE_REVISION": SOURCE_REVISION,
        "COMPAT_WIRE_HASH": PINNED_HASHES["internal/infra/natsjs/message_wire.go"],
        "COMPAT_DEAD_LETTER_HASH": PINNED_HASHES["internal/infra/natsjs/message_deadletter.go"],
        "COMPAT_EVENT_HASH": PINNED_HASHES["internal/domainevent/event.go"],
        "GO_WIRE_FIXTURES": str(fixture_file),
        "GO_WIRE_MODE": mode,
        "GO_WIRE_RUST_EXPORT": str(rust_export_file) if rust_export_file else "",
    }
    lib.run(
        ["go", "-C", str(source_dir), "test", "./internal/infra/natsjs", "-run", "^TestGoWireCompatibilityBridge$", "-count=1"],
        env=env,
    )


def run_compat(*, update: bool, work_dir: Path | None) -> None:
    if not BRIDGE_SOURCE.is_file():
        raise lib.ScriptError(f"missing Go bridge fixture: {BRIDGE_SOURCE}")
    source_url = os.environ.get("GO_WIRE_SOURCE_URL") or f"{COMPAT_SOURCE_URL}.git"

    with tempfile.TemporaryDirectory(prefix="go-wire-") as scratch_raw:
        scratch = Path(scratch_raw)
        source_dir = scratch / "source"
        lib.run(["git", "clone", "--quiet", "--filter=blob:none", "--no-checkout", source_url, str(source_dir)])
        lib.run(["git", "-C", str(source_dir), "checkout", "--quiet", "--detach", SOURCE_REVISION])
        actual_rev = lib.git("rev-parse", "HEAD", cwd=source_dir).strip()
        if actual_rev != SOURCE_REVISION:
            raise lib.ScriptError(f"pinned Go revision mismatch: got {actual_rev}, want {SOURCE_REVISION}")

        for rel_path, expected in PINNED_HASHES.items():
            actual = hashlib.sha256((source_dir / rel_path).read_bytes()).hexdigest()
            if actual != expected:
                raise lib.ScriptError(f"Go source hash mismatch for {rel_path}: got {actual}, want {expected}")

        shutil.copy2(BRIDGE_SOURCE, source_dir / "internal/infra/natsjs/go_wire_compat_bridge_test.go")

        out_dir = (work_dir or scratch).resolve()
        out_dir.mkdir(parents=True, exist_ok=True)
        records = out_dir / "records.json"
        rust_export = out_dir / "rust-export.json"

        _run_go_bridge(source_dir, mode="generate", fixture_file=records)
        lib.run(
            [
                "cargo", "test", "--locked", "-p", "infra-messaging", "--test", "wire_compat",
                "--", "--ignored", "--exact", "rust_production_wire_exports_for_go",
            ],
            env={**os.environ, "GO_WIRE_FIXTURES": str(records), "GO_WIRE_RUST_EXPORT": str(rust_export)},
        )
        _run_go_bridge(source_dir, mode="verify-rust", fixture_file=records, rust_export_file=rust_export)
        _run_go_bridge(source_dir, mode="check", fixture_file=records)

        if update:
            shutil.copy2(records, COMMITTED_FIXTURES)
            shutil.copy2(rust_export, COMMITTED_RUST_EXPORT)
            print(
                f"updated golden fixtures: {COMMITTED_FIXTURES.relative_to(lib.ROOT)}, "
                f"{COMMITTED_RUST_EXPORT.relative_to(lib.ROOT)}"
            )
        else:
            for committed, fresh in ((COMMITTED_FIXTURES, records), (COMMITTED_RUST_EXPORT, rust_export)):
                if not committed.is_file() or committed.read_bytes() != fresh.read_bytes():
                    raise lib.ScriptError(
                        f"committed {committed.relative_to(lib.ROOT)} differs from live Go output; run with --update"
                    )
        print("messaging-go-compat: passed")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--update", action="store_true", help="refresh committed golden records.json")
    parser.add_argument("--work-dir", type=Path, default=None, help="optional directory to keep generated artifacts")
    args = parser.parse_args(argv)
    return lib.run_cli(lambda: run_compat(update=args.update, work_dir=args.work_dir))


if __name__ == "__main__":
    raise SystemExit(main())
