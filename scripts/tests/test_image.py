"""Unit tests for scripts.image."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from scripts import image, lib


class ImageTests(unittest.TestCase):
    def test_dockerfile_toolchain_pin_check(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "build/docker").mkdir(parents=True)
            (root / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.100.0"\n', encoding="utf-8")
            (root / "build/docker/Dockerfile").write_text("FROM rust:1.100.0-slim-trixie AS chef\n", encoding="utf-8")
            image.check_dockerfile_toolchain(root)

            (root / "build/docker/Dockerfile").write_text("FROM rust:1.99.0-slim-trixie AS chef\n", encoding="utf-8")
            with self.assertRaises(lib.ScriptError):
                image.check_dockerfile_toolchain(root)


if __name__ == "__main__":
    unittest.main()
