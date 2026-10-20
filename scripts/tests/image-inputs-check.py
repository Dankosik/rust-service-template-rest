#!/usr/bin/env python3
"""Exercise policy admission with changed real context and documented policy inputs."""

from __future__ import annotations

import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
POLICY = "docs/railway-deployment-profile.md"
IAC = ".railway/railway.ts"


class Coverage(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="image-inputs-check-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for relative in (POLICY, IAC, ".dockerignore", "build/docker/Dockerfile", "scripts/ci/image-inputs-check.py", "make/template.mk", "tools/versions.env"):
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, target)
        (self.root / "Makefile").write_text("include make/template.mk\n")

    def replace(self, relative: str, before: str, after: str) -> None:
        path = self.root / relative
        original = path.read_text()
        self.assertIn(before, original, "scenario does not reach its intended input")
        path.write_text(original.replace(before, after))

    def run_check(self, expected: str | None = None) -> str:
        result = subprocess.run([sys.executable, str(self.root / "scripts/ci/image-inputs-check.py"), "--repo", str(self.root)], capture_output=True, text=True)
        if expected is None:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn(expected, result.stderr)
        return result.stdout

    def remove_watch(self, pattern: str) -> None:
        self.replace(POLICY, f'`{pattern}`, ', '')
        path = self.root / IAC
        original = path.read_text()
        updated = re.sub(r'"' + re.escape(pattern) + r'",\s*', "", original, count=1)
        self.assertNotEqual(updated, original, "scenario does not reach its intended input")
        path.write_text(updated)

    def snippet_policy(self) -> None:
        """A repository initialized before the IaC file keeps its form in the document."""

        program = (self.root / IAC).read_text()
        (self.root / IAC).unlink()
        with (self.root / POLICY).open("a") as stream:
            stream.write(f"\n```ts\n{program}```\n")

    def test_reads_the_iac_file_and_names_it(self) -> None:
        self.assertIn(f"and {IAC} cover", self.run_check())

    def test_document_snippet_stands_in_without_the_iac_file(self) -> None:
        self.snippet_policy()
        self.assertIn("IaC snippet cover", self.run_check())
        self.replace(POLICY, '"migrations/**", ', '')
        self.run_check("IaC snippet: watch forms disagree")

    def test_the_iac_file_wins_over_a_stale_snippet(self) -> None:
        with (self.root / POLICY).open("a") as stream:
            stream.write('\n```ts\nconst stale = { builder: "NIXPACKS", watchPatterns: ["docs/**"] };\n```\n')
        self.assertIn(f"and {IAC} cover", self.run_check())

    def test_current_context_and_additional_exclusions(self) -> None:
        with (self.root / ".dockerignore").open("a") as stream:
            stream.write("\ncrates/*/benches/\n")
        self.run_check()

    def test_previously_omitted_inputs_cannot_pass(self) -> None:
        for pattern in (
            "migrations/**", ".sqlx/**", "vendor/**", "test/Cargo.toml", "test/src/**", "test/tests/**",
            "test/fixtures/**", "env/config/**",
        ):
            with self.subTest(pattern=pattern):
                originals = {relative: (self.root / relative).read_text() for relative in (POLICY, IAC)}
                self.remove_watch(pattern)
                self.run_check(pattern + ": image input is uncovered")
                for relative, text in originals.items():
                    (self.root / relative).write_text(text)

    def test_future_family_requires_watch_before_any_file_exists(self) -> None:
        with (self.root / ".dockerignore").open("a") as stream:
            stream.write("\n!assets/\n")
        self.run_check("assets/**: image input is uncovered")
        self.replace(POLICY, '`Cargo.toml`,', '`assets/**`, `Cargo.toml`,')
        self.replace(IAC, '"Cargo.toml",', '"assets/**", "Cargo.toml",')
        self.run_check()

    def test_forms_cannot_drift(self) -> None:
        self.replace(IAC, '"migrations/**", ', '')
        self.run_check("watch forms disagree")

    def test_unknown_inclusion_and_watch_grammar_refuse(self) -> None:
        original_ignore = (self.root / ".dockerignore").read_text()
        with (self.root / ".dockerignore").open("a") as stream:
            stream.write("\n!assets/*.json\n")
        self.run_check("unsupported inclusion '!assets/*.json'")
        (self.root / ".dockerignore").write_text(original_ignore)
        self.replace(POLICY, 'crates/**', 'crates/*')
        self.run_check("unsupported watch syntax 'crates/*'")

    def test_context_must_start_with_exclusion(self) -> None:
        self.replace(".dockerignore", "\n*\n", "\n")
        self.run_check("supported coverage starts with '*'")

    def test_specific_ignore_override_refuses(self) -> None:
        (self.root / "build/docker/Dockerfile.dockerignore").write_text("!secrets/\n")
        self.run_check("Dockerfile-specific ignore overrides are unsupported")

    def test_changed_context_selection_refuses(self) -> None:
        self.replace(IAC, 'builder: "DOCKERFILE",', 'builder: "DOCKERFILE",\n      rootDirectory: "subdir",')
        self.run_check("unsupported rootDirectory")

    def test_dynamic_or_duplicate_policy_cannot_hide_an_override(self) -> None:
        original = (self.root / IAC).read_text()
        self.replace(IAC, 'watchPatterns: [', '...overrides, watchPatterns: [')
        self.run_check("unsupported computed/spread IaC policy")
        (self.root / IAC).write_text(original)
        self.replace(IAC, 'watchPatterns: [', 'watchPatterns: dynamicPatterns(), watchPatterns: [')
        self.run_check("expected exactly one IaC watchPatterns setting")

    def test_named_context_refuses(self) -> None:
        with (self.root / "build/docker/Dockerfile").open("a") as stream:
            stream.write("\nCOPY --from=assets /file /file\n")
        self.run_check("unsupported external/named COPY context 'assets'")

    def test_docs_gate_refuses_before_link_checker(self) -> None:
        # Make resolves the source database profile before executing any recipe.
        for relative in ("scripts/lib/template_state.py", "make/profile-postgres.mk"):
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, target)
        self.remove_watch("migrations/**")
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        marker = self.root / "docker-called"
        docker = bin_dir / "docker"
        docker.write_text(f'#!/bin/sh\ntouch "{marker}"\nexit 0\n')
        docker.chmod(0o755)
        result = subprocess.run(["make", "docs-check"], cwd=self.root, env={**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}"}, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("migrations/**: image input is uncovered", result.stderr)
        self.assertFalse(marker.exists(), "link checker ran after policy refusal")


if __name__ == "__main__":
    unittest.main()
