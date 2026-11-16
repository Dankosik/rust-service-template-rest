#!/usr/bin/env python3
"""Self-tests for scripts/profiles/migration_history.sh."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/profiles/migration_history.sh"


def _git(repo: Path, *args: str) -> str:
    return subprocess.check_output(
        [
            "git",
            "-C",
            str(repo),
            "-c",
            "user.name=migration-history",
            "-c",
            "user.email=migration-history@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "gc.auto=0",
            *args,
        ],
        text=True,
    ).strip()


@unittest.skipUnless(SCRIPT.is_file(), "PostgreSQL profile removed")
class MigrationHistoryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="migration-history-")))
        self.linter_log = self.temp / "linter.log"
        self.linter = self.temp / "linter.sh"
        self.linter.write_text(
            f'#!/usr/bin/env bash\nprintf "%s\\n" "$@" >>"{self.linter_log}"\n',
            encoding="utf-8",
        )
        self.linter.chmod(0o755)

    def run_script(self, repo: Path, **overrides: str) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env.pop("BASE_REF", None)
        env.pop("MIGRATION_DIR", None)
        env["MIGRATION_REPO_ROOT"] = str(repo)
        env["MIGRATION_LINTER"] = str(self.linter)
        env.update(overrides)
        return subprocess.run(
            ["bash", str(SCRIPT)],
            cwd=repo,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_append_only_history_lint_modes_and_secondary_directories(self) -> None:
        fixture = self.temp / "repo"
        fixture.mkdir()
        _git(fixture, "init", "-q")
        migrations = fixture / "migrations"
        migrations.mkdir()
        (migrations / "20260918000001_create.sql").write_text("SELECT 1;\n", encoding="utf-8")
        (migrations / "20260918000003_extend.sql").write_text("SELECT 3;\n", encoding="utf-8")
        (migrations / "README.md").write_text("# notes\n", encoding="utf-8")
        _git(fixture, "add", ".")
        _git(fixture, "commit", "-qm", "baseline")

        res = self.run_script(fixture)
        self.assertEqual(res.returncode, 0, res.stderr)

        with (migrations / "20260918000001_create.sql").open("a", encoding="utf-8") as handle:
            handle.write("-- rewritten\n")
        self.assertNotEqual(self.run_script(fixture).returncode, 0)
        _git(fixture, "restore", "migrations/20260918000001_create.sql")

        for old_name, new_name in (
            ("20260918000001_create.sql", "former migration.txt"),
            ("README.md", "20260918000004_from notes.sql"),
        ):
            old_rel = f"migrations/{old_name}"
            new_rel = f"migrations/{new_name}"
            _git(fixture, "mv", old_rel, new_rel)
            refusal = self.run_script(fixture)
            self.assertNotEqual(refusal.returncode, 0)
            self.assertIn("applied migration files are append-only", refusal.stderr)
            _git(fixture, "mv", new_rel, old_rel)

        copy_rel = "migrations/20260918000004_copy with space.sql"
        shutil.copy2(migrations / "README.md", fixture / copy_rel)
        _git(fixture, "add", copy_rel)
        res = self.run_script(fixture)
        self.assertEqual(res.returncode, 0, res.stderr)
        self.assertIn(f"--assume-in-transaction\n{copy_rel}", self.linter_log.read_text(encoding="utf-8"))

        with (migrations / "README.md").open("a", encoding="utf-8") as handle:
            handle.write("# more notes\n")
        # With its source modified, Git reports the destination as C100.
        refusal = self.run_script(fixture)
        self.assertNotEqual(refusal.returncode, 0)
        self.assertIn("applied migration files are append-only", refusal.stderr)
        self.assertIn("C100", refusal.stderr)

        _git(fixture, "restore", "--staged", "--", copy_rel)
        (fixture / copy_rel).unlink()
        res = self.run_script(fixture)
        self.assertEqual(res.returncode, 0, res.stderr)

        (migrations / "20260918000004_add.sql").write_text("SELECT 4;\n", encoding="utf-8")
        self.linter_log.write_text("", encoding="utf-8")
        res = self.run_script(fixture)
        self.assertEqual(res.returncode, 0, res.stderr)
        self.assertTrue(
            self.linter_log.read_text(encoding="utf-8")
            .rstrip("\n")
            .endswith("--assume-in-transaction\nmigrations/20260918000004_add.sql")
        )

        self.assertNotEqual(self.run_script(fixture, MIGRATION_LINTER="false").returncode, 0)

        (migrations / "20260918000005_online.sql").write_text("-- no-transaction\nSELECT 5;\n", encoding="utf-8")
        self.linter_log.write_text("", encoding="utf-8")
        res = self.run_script(fixture)
        self.assertEqual(res.returncode, 0, res.stderr)
        logged = self.linter_log.read_text(encoding="utf-8")
        tx_idx = logged.find("--assume-in-transaction\nmigrations/20260918000004_add.sql")
        notx_idx = logged.find("--no-assume-in-transaction\nmigrations/20260918000005_online.sql")
        self.assertGreaterEqual(tx_idx, 0, logged)
        self.assertGreater(notx_idx, tx_idx, logged)
        (migrations / "20260918000005_online.sql").unlink()

        _git(fixture, "add", "migrations/20260918000004_add.sql")
        self.assertEqual(self.run_script(fixture).returncode, 0)

        (migrations / "20260918000002_late.sql").write_text("SELECT 2;\n", encoding="utf-8")
        self.assertNotEqual(self.run_script(fixture).returncode, 0)
        (migrations / "20260918000002_late.sql").unlink()

        (migrations / "20260918000006_duplicate_a.sql").write_text("SELECT 6;\n", encoding="utf-8")
        (migrations / "20260918000006_duplicate_b.sql").write_text("SELECT 6;\n", encoding="utf-8")
        self.assertNotEqual(self.run_script(fixture).returncode, 0)
        (migrations / "20260918000006_duplicate_a.sql").unlink()
        (migrations / "20260918000006_duplicate_b.sql").unlink()

        _git(fixture, "rm", "-q", "migrations/20260918000003_extend.sql")
        self.assertNotEqual(self.run_script(fixture).returncode, 0)
        _git(fixture, "restore", "--staged", "--worktree", "migrations/20260918000003_extend.sql")

        _git(fixture, "commit", "-qam", "addition")
        self.assertEqual(self.run_script(fixture, BASE_REF="HEAD~1").returncode, 0)

        # Amending a migration added in the same range remains an addition relative to base.
        with (migrations / "20260918000004_add.sql").open("a", encoding="utf-8") as handle:
            handle.write("-- amended before merge\n")
        _git(fixture, "commit", "-qam", "amend")
        self.assertEqual(self.run_script(fixture, BASE_REF="HEAD~2").returncode, 0)

        with (migrations / "20260918000001_create.sql").open("a", encoding="utf-8") as handle:
            handle.write("-- rewritten\n")
        _git(fixture, "commit", "-qam", "rewrite")
        self.assertNotEqual(self.run_script(fixture, BASE_REF="HEAD~1").returncode, 0)

        missing = self.run_script(fixture, MIGRATION_DIR="missing")
        self.assertEqual(missing.returncode, 2)

        # Secondary history configured via scripts/service/database.sh.
        secondary = fixture / "migrations-secondary"
        secondary.mkdir()
        hook_dir = fixture / "scripts/service"
        hook_dir.mkdir(parents=True)
        (hook_dir / "database.sh").write_text(
            "service_migration_directories() {\n\tmigration_directories=(migrations migrations-secondary)\n}\n",
            encoding="utf-8",
        )
        (secondary / "20261020000000_create.sql").write_text("SELECT 1;\n", encoding="utf-8")
        _git(fixture, "add", "migrations-secondary")
        _git(fixture, "commit", "-qm", "secondary-baseline")
        self.assertEqual(self.run_script(fixture).returncode, 0)

        with (secondary / "20261020000000_create.sql").open("a", encoding="utf-8") as handle:
            handle.write("SELECT 2;\n")
        self.assertNotEqual(self.run_script(fixture).returncode, 0)
        _git(fixture, "restore", "migrations-secondary")

        (secondary / "20261021000000_extend.sql").write_text("SELECT 3;\n", encoding="utf-8")
        self.linter_log.write_text("", encoding="utf-8")
        res = self.run_script(fixture)
        self.assertEqual(res.returncode, 0, res.stderr)
        self.assertIn(
            "migrations-secondary/20261021000000_extend.sql",
            self.linter_log.read_text(encoding="utf-8"),
        )

    def test_signed_64_bit_version_bounds_and_integer_duplicates(self) -> None:
        bounds = self.temp / "bounds-repo"
        bounds.mkdir()
        _git(bounds, "init", "-q")
        migrations = bounds / "migrations"
        migrations.mkdir()
        (migrations / "README.md").write_text("# empty baseline\n", encoding="utf-8")
        _git(bounds, "add", ".")
        _git(bounds, "commit", "-qm", "empty-baseline")

        (migrations / "0_init.sql").write_text("SELECT 0;\n", encoding="utf-8")
        res = self.run_script(bounds)
        self.assertEqual(res.returncode, 0, res.stderr)

        (migrations / "01_a.sql").write_text("SELECT 1;\n", encoding="utf-8")
        (migrations / "1_b.sql").write_text("SELECT 1;\n", encoding="utf-8")
        self.assertNotEqual(self.run_script(bounds).returncode, 0)
        (migrations / "01_a.sql").unlink()
        (migrations / "1_b.sql").unlink()

        (migrations / "9223372036854775806_near_max.sql").write_text("SELECT 1;\n", encoding="utf-8")
        _git(bounds, "add", "migrations/0_init.sql", "migrations/9223372036854775806_near_max.sql")
        _git(bounds, "commit", "-qm", "near-max")

        (migrations / "9223372036854775807_i64_max.sql").write_text("SELECT 2;\n", encoding="utf-8")
        res = self.run_script(bounds)
        self.assertEqual(res.returncode, 0, res.stderr)
        (migrations / "9223372036854775807_i64_max.sql").unlink()

        (migrations / "9223372036854775808_overflow.sql").write_text("SELECT 3;\n", encoding="utf-8")
        self.assertNotEqual(self.run_script(bounds).returncode, 0)
        (migrations / "9223372036854775808_overflow.sql").unlink()


if __name__ == "__main__":
    unittest.main()
