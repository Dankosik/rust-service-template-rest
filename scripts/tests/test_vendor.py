"""Unit tests for scripts.checks.vendor."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from scripts.checks import vendor


class VendorDriftTests(unittest.TestCase):
    def test_offline_drift_detects_missing_reviewed_and_bypassed_lock(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "vendor/foo").mkdir(parents=True)
            (root / "vendor/foo/Cargo.toml").write_text('[package]\nname = "foo"\nversion = "1.2.3"\n', encoding="utf-8")
            (root / "vendor/foo/PATCHES.md").write_text("Upstream reviewed: 1.2.3\n", encoding="utf-8")
            (root / "Cargo.toml").write_text('[patch.crates-io]\nfoo = { path = "vendor/foo" }\n', encoding="utf-8")
            (root / "Cargo.lock").write_text('[[package]]\nname = "foo"\nversion = "1.2.3"\n', encoding="utf-8")

            self.assertEqual(vendor.check_drift(root, online=False, upstream=False), [])

            (root / "vendor/foo/PATCHES.md").write_text("No reviewed line\n", encoding="utf-8")
            findings = vendor.check_drift(root, online=False, upstream=False)
            self.assertTrue(any("Upstream reviewed" in f for f in findings), findings)

            (root / "vendor/foo/PATCHES.md").write_text("Upstream reviewed: 1.2.3\n", encoding="utf-8")
            (root / "Cargo.lock").write_text(
                '[[package]]\nname = "foo"\nversion = "1.2.3"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n',
                encoding="utf-8",
            )
            bypass = vendor.check_drift(root, online=False, upstream=False)
            self.assertTrue(any("bypassing the patch" in f for f in bypass), bypass)


if __name__ == "__main__":
    unittest.main()
