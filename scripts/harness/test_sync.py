"""Tests for harness materialization and skill validation."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[2]
from scripts.template.state import TEMPLATE_REPOSITORY  # noqa: E402


class HarnessSyncTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="harness-sync-")))
        roles = self.repo / ".agents/roles"
        roles.mkdir(parents=True)
        (roles / "fixture-worker.toml").write_text(
            'name = "fixture-worker"\n'
            'description = "Run the assigned work."\n'
            'class = "mutable-worker"\n'
            'claude_model = "inherit"\n'
            'cursor_model = "inherit"\n'
            'grok_model = "inherit"\n'
            'grok_effort = "inherit"\n'
            'output_schema = "delegated-result-v1"\n'
            'instructions = "Return the result."\n',
            encoding="utf-8",
        )
        classes = self.repo / ".agents/role-classes"
        classes.mkdir()
        (classes / "mutable-worker.md").write_text("Work within the assigned scope.\n", encoding="utf-8")
        (classes / "mutable-worker-fallback.md").write_text("Report any blocker.\n", encoding="utf-8")
        # Build clean decision + workflow skills without .agents/skills/*/LICENSE.
        para = " ".join(["word"] * 60)
        decision = self.repo / ".agents/skills/sample-decision"
        (decision / "references").mkdir(parents=True)
        (decision / "references/guide.md").write_text("# Guide\n\nDetails.\n", encoding="utf-8")
        (decision / "SKILL.md").write_text(
            "---\n"
            "name: sample-decision\n"
            'description: "Focus. Use when validating canonical skill budgets and reference links."\n'
            "---\n\n"
            "# Sample Decision\n\n"
            f"**First concept.** See [guide](references/guide.md) {para}.\n\n"
            f"**Second concept.** {para}.\n\n"
            f"**Third concept.** {para}.\n",
            encoding="utf-8",
        )
        workflow = self.repo / ".agents/skills/sample-workflow"
        (workflow / "agents").mkdir(parents=True)
        (workflow / "agents/openai.yaml").write_text(
            "policy:\n  allow_implicit_invocation: false\n",
            encoding="utf-8",
        )
        (workflow / "SKILL.md").write_text(
            "---\n"
            "name: sample-workflow\n"
            'description: "Run the sample workflow."\n'
            "metadata:\n"
            "  invocation: user\n"
            "  kind: workflow\n"
            "disable-model-invocation: true\n"
            "---\n\n"
            "# Sample Workflow\n\n"
            "Execute the bounded workflow steps.\n",
            encoding="utf-8",
        )

    def run_sync(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(ROOT / "scripts/harness/sync.py"), "--repo", str(self.repo), *args],
            capture_output=True,
            text=True,
        )

    def test_apply_check_and_skill_validation_without_license(self) -> None:
        adapter = self.repo / ".agents/adapters/claude"
        adapter.mkdir(parents=True)
        (adapter / "CLAUDE.md").write_text("# Fixture harness\n", encoding="utf-8")
        (adapter / "settings.json").write_text(
            '{"env":{"CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH":"1"}}\n', encoding="utf-8",
        )

        check_unmaterialized = self.run_sync("--check", "--harness", "claude")
        self.assertEqual(check_unmaterialized.returncode, 0, check_unmaterialized.stderr)

        require_mat = self.run_sync("--check", "--harness", "claude", "--require-materialized")
        self.assertNotEqual(require_mat.returncode, 0)
        self.assertIn(".claude/agents is missing", require_mat.stderr)

        applied = self.run_sync("--apply", "--harness", "claude")
        self.assertEqual(applied.returncode, 0, applied.stderr)
        self.assertTrue((self.repo / ".claude/skills/sample-decision").is_symlink())

        checked = self.run_sync("--check", "--harness", "claude", "--require-materialized")
        self.assertEqual(checked.returncode, 0, checked.stderr)

        # Unexpected entry (including retired per-skill LICENSE) is rejected.
        stray_license = self.repo / ".agents/skills/sample-decision/LICENSE"
        stray_license.write_text("MIT\n", encoding="utf-8")
        bad_entry = self.run_sync("--check", "--harness", "core")
        self.assertNotEqual(bad_entry.returncode, 0)
        self.assertIn("unexpected entry LICENSE", bad_entry.stderr)
        stray_license.unlink()

        # Unselected reference file is rejected.
        orphan_ref = self.repo / ".agents/skills/sample-decision/references/orphan.md"
        orphan_ref.write_text("# Orphan\n", encoding="utf-8")
        unselected = self.run_sync("--check", "--harness", "core")
        self.assertNotEqual(unselected.returncode, 0)
        self.assertIn("no selecting link", unselected.stderr)
        orphan_ref.unlink()

        # Broken internal Markdown link is rejected.
        skill_md = self.repo / ".agents/skills/sample-decision/SKILL.md"
        original_md = skill_md.read_text(encoding="utf-8")
        skill_md.write_text(original_md.replace("references/guide.md", "references/missing.md"), encoding="utf-8")
        broken_link = self.run_sync("--check", "--harness", "core")
        self.assertNotEqual(broken_link.returncode, 0)
        self.assertIn("broken skill Markdown link", broken_link.stderr)
        skill_md.write_text(original_md, encoding="utf-8")

    def test_service_owned_skill_requires_complete_lock(self) -> None:
        svc_skill = self.repo / ".agents/skills/local-service-skill"
        svc_skill.mkdir(parents=True)
        (svc_skill / ".service-owned").write_text("", encoding="utf-8")
        (svc_skill / "SKILL.md").write_text(
            "---\nname: local-service-skill\ndescription: local\n---\n",
            encoding="utf-8",
        )
        no_lock = self.run_sync("--check", "--harness", "core")
        self.assertNotEqual(no_lock.returncode, 0)
        self.assertIn("canonical source must not contain .service-owned", no_lock.stderr)

        (self.repo / "template.lock").write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "state": "complete",
                    "identity": {
                        "service_name": "orders-api",
                        "repository": "https://github.com/example/orders-api",
                        "description": "Orders API",
                        "codeowner": "@example/platform",
                    },
                    "profiles": {"database": "none", "agent_harness": "core"},
                    "source": {
                        "repository": TEMPLATE_REPOSITORY,
                        "checkout_revision": "c" * 40,
                        "provenance": "local-checkout",
                    },
                }
            ),
            encoding="utf-8",
        )
        missing_trigger = self.run_sync("--check", "--harness", "core")
        self.assertNotEqual(missing_trigger.returncode, 0)
        self.assertIn("Use ", missing_trigger.stderr)

        (svc_skill / "SKILL.md").write_text(
            '---\nname: local-service-skill\ndescription: "Local. Use when testing service-owned skills."\n---\n',
            encoding="utf-8",
        )
        with_lock = self.run_sync("--check", "--harness", "core")
        self.assertEqual(with_lock.returncode, 0, with_lock.stderr)
