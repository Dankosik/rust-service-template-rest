"""Unit tests for scripts.checks.migrations."""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from scripts.checks import migrations


class MigrationCheckTests(unittest.TestCase):
    @staticmethod
    def _git(repo: Path, *args: str) -> None:
        subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)

    def test_append_only_monotonic_enforcement(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self._git(repo, "init", "-q", "-b", "main")
            self._git(repo, "config", "user.email", "test@example.invalid")
            self._git(repo, "config", "user.name", "Test")
            (repo / "migrations").mkdir()
            (repo / "migrations/20260101000001_init.sql").write_text("SELECT 1;\n", encoding="utf-8")
            self._git(repo, "add", ".")
            self._git(repo, "commit", "-qm", "base")

            (repo / "migrations/20260102000001_next.sql").write_text("SELECT 2;\n", encoding="utf-8")
            added, _ = migrations.check_history(repo, "migrations", None)
            self.assertEqual(added, ["migrations/20260102000001_next.sql"])

            (repo / "migrations/20260100000001_old.sql").write_text("SELECT 0;\n", encoding="utf-8")
            with self.assertRaises(migrations.MigrationCheckError):
                migrations.check_history(repo, "migrations", None)
            (repo / "migrations/20260100000001_old.sql").unlink()

            (repo / "migrations/20260101000001_init.sql").write_text("SELECT 99;\n", encoding="utf-8")
            with self.assertRaises(migrations.MigrationCheckError):
                migrations.check_history(repo, "migrations", None)


if __name__ == "__main__":
    unittest.main()
