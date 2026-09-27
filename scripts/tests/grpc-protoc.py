#!/usr/bin/env python3
"""Focused behavior tests for the managed gRPC protoc resolver."""

from __future__ import annotations

import hashlib
import importlib.util
import stat
import tempfile
import unittest
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("grpc_protoc", ROOT / "scripts" / "grpc-protoc.py")
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def archive(path: Path) -> str:
    with zipfile.ZipFile(path, "w") as output:
        output.writestr("bin/protoc", "#!/bin/sh\necho libprotoc 36.2\n")
        output.writestr("include/google/protobuf/descriptor.proto", "syntax = \"proto3\";\n")
    return hashlib.sha256(path.read_bytes()).hexdigest()


class ManagedProtocTests(unittest.TestCase):
    def test_selects_each_supported_build_host(self) -> None:
        pins = {"PROTOC_VERSION": "36.2"}
        for host, digest in (
            ("LINUX_X86_64", "a" * 64),
            ("LINUX_AARCH_64", "b" * 64),
            ("OSX_X86_64", "c" * 64),
            ("OSX_AARCH_64", "d" * 64),
        ):
            pins[f"PROTOC_SHA256_{host}"] = digest
        self.assertEqual(MODULE.selection(pins, "linux", "x86_64")[2], "protoc-36.2-linux-x86_64.zip")
        self.assertEqual(MODULE.selection(pins, "linux", "aarch64")[2], "protoc-36.2-linux-aarch_64.zip")
        self.assertEqual(MODULE.selection(pins, "darwin", "x86_64")[2], "protoc-36.2-osx-x86_64.zip")
        self.assertEqual(MODULE.selection(pins, "darwin", "arm64")[2], "protoc-36.2-osx-aarch_64.zip")
        with self.assertRaisesRegex(MODULE.ResolverError, "unsupported protoc build host"):
            MODULE.host_spec("win32", "x86_64")

    def test_refuses_wrong_checksum_without_creating_entry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "protoc.zip"
            archive(source)
            entry = root / "cache" / "linux-x86_64"
            entry.parent.mkdir()
            with self.assertRaisesRegex(MODULE.ResolverError, "checksum mismatch"):
                MODULE.install_archive(source, "0" * 64, entry)
            self.assertFalse(entry.exists())

    def test_installs_complete_entry_once(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "protoc.zip"
            digest = archive(source)
            entry = root / "cache" / "linux-x86_64"
            entry.parent.mkdir()
            first = MODULE.install_archive(source, digest, entry)
            initial = first.read_bytes()
            second = MODULE.install_archive(source, digest, entry)
            self.assertEqual(first, second)
            self.assertEqual(second.read_bytes(), initial)
            self.assertTrue(second.stat().st_mode & stat.S_IXUSR)
            self.assertTrue((entry / "include" / "google" / "protobuf" / "descriptor.proto").is_file())


if __name__ == "__main__":
    unittest.main()
