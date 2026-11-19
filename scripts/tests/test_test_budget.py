"""Unit tests for advisory PR test-line growth budget measurement."""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from scripts.checks import test_budget


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


class TestBudgetTests(unittest.TestCase):
    def test_dedicated_test_path_classification(self) -> None:
        self.assertTrue(test_budget.is_dedicated_test_path("test/tests/postgres.rs"))
        self.assertTrue(test_budget.is_dedicated_test_path("crates/service/tests/openapi.rs"))
        self.assertTrue(test_budget.is_dedicated_test_path("crates/health/src/tests.rs"))
        self.assertTrue(test_budget.is_dedicated_test_path("crates/tls-test-support/src/lib.rs"))
        self.assertFalse(test_budget.is_dedicated_test_path("crates/health/src/lib.rs"))
        self.assertFalse(test_budget.is_dedicated_test_path("docs/build-speed.md"))

    def test_measures_dedicated_and_inline_cfg_test_lines_accurately(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            _git(repo, "init")
            _git(repo, "config", "user.email", "test@example.com")
            _git(repo, "config", "user.name", "Test")
            (repo / "crates/demo/src").mkdir(parents=True)
            (repo / "crates/demo/tests").mkdir(parents=True)
            (repo / "crates/demo/src/lib.rs").write_text(
                "pub fn prod() -> u32 { 1 }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn old() { assert_eq!(1, 1); }\n}\n",
                encoding="utf-8",
            )
            (repo / "crates/demo/tests/contract.rs").write_text("#[test]\nfn base() {}\n", encoding="utf-8")
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "base")

            (repo / "crates/demo/src/lib.rs").write_text(
                "pub fn prod() -> u32 {\n    2\n}\n\n#[cfg(test)]\nmod tests {\n    // comment ignored\n    #[test]\n    fn new_a() { assert_eq!(2, 2); }\n    #[test]\n    fn new_b() { assert_eq!(2, 2); }\n}\n",
                encoding="utf-8",
            )
            (repo / "crates/demo/tests/contract.rs").write_text(
                "#[test]\nfn base() {}\n\n#[test]\nfn added_contract() {\n    assert!(true);\n}\n",
                encoding="utf-8",
            )
            _git(repo, "add", ".")
            _git(repo, "commit", "-m", "candidate")

            report = test_budget.measure_test_delta(repo, "HEAD~1", budget=5)
            self.assertEqual(report.added, 7)
            self.assertEqual(report.removed, 1)
            self.assertEqual(report.net, 6)
            self.assertTrue(report.over_budget)

            (repo / "crates/demo/tests/contract.rs").unlink()
            _git(repo, "add", "-A")
            _git(repo, "commit", "-m", "delete test file")
            del_report = test_budget.measure_test_delta(repo, "HEAD~1", budget=5)
            self.assertEqual(del_report.added, 0)
            self.assertEqual(del_report.removed, 6)
            self.assertEqual(del_report.net, -6)
            self.assertFalse(del_report.over_budget)


if __name__ == "__main__":
    unittest.main()
