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

    # template:begin temporal:test-image-temporal
    def test_retained_and_pruned_binaries_tracks_temporal_worker(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            retained, pruned = image._retained_and_pruned_binaries(root)
            self.assertEqual(retained, ["/service"])
            self.assertEqual(pruned, ["/migrate", "/jobs-worker", "/temporal-worker"])

            (root / "crates/temporal-worker").mkdir(parents=True)
            retained, pruned = image._retained_and_pruned_binaries(root)
            self.assertEqual(retained, ["/service", "/temporal-worker"])
            self.assertEqual(pruned, ["/migrate", "/jobs-worker"])
    # template:end temporal:test-image-temporal


if __name__ == "__main__":
    unittest.main()
