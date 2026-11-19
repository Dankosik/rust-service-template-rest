#!/usr/bin/env python3
"""Unit tests for scripts/checks/image_inputs.py and docs-check ordering."""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[2]
IAC = ".railway/railway.ts"
WATCHES = (
    "Cargo.toml",
    "rust-toolchain.toml",
    "crates/**",
    "migrations/**",
    "build/docker/**",
    ".dockerignore",
)
IAC_WATCHES = ", ".join(f'"{watch}"' for watch in WATCHES)
FIXTURE = {
    "rust-toolchain.toml": '[toolchain]\nchannel = "1.95.0"\n',
    IAC: f"""export default {{
  build: {{
    builder: "DOCKERFILE",
    dockerfilePath: "build/docker/Dockerfile",
    watchPatterns: [
      {IAC_WATCHES},
    ],
  }},
}};
""",
    ".dockerignore": """# Fixture context
*
!Cargo.toml
!crates/
!migrations/
""",
    "build/docker/Dockerfile": """FROM rust AS build
COPY . .
FROM debian
COPY --from=build /app /app
""",
}


class Coverage(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="image-inputs-check-")))
        for relative, text in FIXTURE.items():
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text, encoding="utf-8")
        checker = self.root / "scripts/checks/image_inputs.py"
        checker.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / "scripts/checks/image_inputs.py", checker)

    def replace(self, relative: str, before: str, after: str) -> None:
        path = self.root / relative
        original = path.read_text(encoding="utf-8")
        self.assertIn(before, original, "scenario does not reach its intended input")
        path.write_text(original.replace(before, after), encoding="utf-8")

    def run_check(self, expected: str | None = None) -> str:
        result = subprocess.run(
            [sys.executable, str(self.root / "scripts/checks/image_inputs.py"), "--repo", str(self.root)],
            capture_output=True,
            text=True,
        )
        if expected is None:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn(expected, result.stderr)
        return result.stdout

    def remove_watch(self, pattern: str) -> None:
        path = self.root / IAC
        original = path.read_text(encoding="utf-8")
        updated = re.sub(r'"' + re.escape(pattern) + r'",\s*', "", original, count=1)
        self.assertNotEqual(updated, original, "scenario does not reach its intended input")
        path.write_text(updated, encoding="utf-8")

    def test_accepts_covered_inputs_and_ignores_exclusions(self) -> None:
        with (self.root / ".dockerignore").open("a", encoding="utf-8") as stream:
            stream.write("\ncrates/*/benches/\n")
        self.assertIn(f"{IAC} covers", self.run_check())

    def test_uncovered_or_future_family_refuses(self) -> None:
        for pattern in ("migrations/**", "rust-toolchain.toml"):
            with self.subTest(pattern=pattern):
                original = (self.root / IAC).read_text(encoding="utf-8")
                self.remove_watch(pattern)
                self.run_check(f"{pattern}: image input is uncovered")
                (self.root / IAC).write_text(original, encoding="utf-8")

        with (self.root / ".dockerignore").open("a", encoding="utf-8") as stream:
            stream.write("\n!assets/\n")
        self.run_check("assets/**: image input is uncovered")
        self.replace(IAC, '"Cargo.toml",', '"assets/**", "Cargo.toml",')
        self.run_check()

    def test_invalid_ignore_or_watch_syntax_refuses(self) -> None:
        original_ignore = (self.root / ".dockerignore").read_text(encoding="utf-8")
        with (self.root / ".dockerignore").open("a", encoding="utf-8") as stream:
            stream.write("\n!assets/*.json\n")
        self.run_check("unsupported inclusion '!assets/*.json'")
        (self.root / ".dockerignore").write_text(original_ignore, encoding="utf-8")

        self.replace(IAC, "crates/**", "crates/*")
        self.run_check("unsupported watch syntax 'crates/*'")

    def test_dockerfile_context_escapes_and_mounts_refuse(self) -> None:
        (self.root / "build/docker/Dockerfile.dockerignore").write_text("!secrets/\n", encoding="utf-8")
        self.run_check("Dockerfile-specific ignore overrides are unsupported")
        (self.root / "build/docker/Dockerfile.dockerignore").unlink()

        path = self.root / "build/docker/Dockerfile"
        original = path.read_text(encoding="utf-8")
        path.write_text(
            original + "\n# trailing backslash in comment \\\nCOPY --from=assets /file /file\n",
            encoding="utf-8",
        )
        self.run_check("unsupported external/named COPY context 'assets'")

        path.write_text(
            original + "RUN --mount=type=secret,id=credential,required=true test -s /run/secrets/credential\n",
            encoding="utf-8",
        )
        self.run_check()
        path.write_text(original + "RUN --mount=type=cache,target=/cache true\n", encoding="utf-8")
        self.run_check("unsupported RUN mount context")

        path.write_text(
            original + "ADD --checksum=sha256:00 https://example.com/tool.tgz /tmp/tool.tgz\n",
            encoding="utf-8",
        )
        self.run_check()
        path.write_text(original + "ADD tools/tool.tgz /tmp/\n", encoding="utf-8")
        self.run_check("unsupported context instruction")

    def test_published_image_and_forbidden_iac_overrides(self) -> None:
        image = 'export default service("api", { source: image(process.env.IMAGE) });\n'
        (self.root / IAC).write_text(image, encoding="utf-8")
        self.assertIn(f"{IAC} deploys a published image", self.run_check())
        (self.root / IAC).write_text(image.replace("source:", 'rootDirectory: "sub", source:'), encoding="utf-8")
        self.run_check("unsupported rootDirectory")
        (self.root / IAC).write_text(image.replace("source:", "...overrides, source:"), encoding="utf-8")
        self.run_check("unsupported computed/spread")

    def test_docs_gate_propagates_image_input_and_repository_link_failures(self) -> None:
        shutil.copytree(ROOT / "make", self.root / "make", ignore=shutil.ignore_patterns("source.mk"))
        (self.root / "tools").mkdir()
        shutil.copy2(ROOT / "tools/versions.env", self.root / "tools/versions.env")
        shutil.copy2(ROOT / "Makefile", self.root / "Makefile")
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True, capture_output=True)
        (self.root / "README.md").write_text("# Fixture\n", encoding="utf-8")
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        docker = bin_dir / "docker"
        docker.write_text(
            '#!/bin/sh\n'
            'case "$*" in\n'
            '  info) exit 0 ;;\n'
            '  *" -- README.md") echo "repository link check"; exit "$LYCHEE_STATUS" ;;\n'
            '  *) exit 2 ;;\n'
            'esac\n',
            encoding="utf-8",
        )
        docker.chmod(0o755)
        env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}"}

        def check(lychee_status: int) -> subprocess.CompletedProcess[str]:
            return subprocess.run(
                ["make", "docs-check"],
                cwd=self.root,
                env={**env, "LYCHEE_STATUS": str(lychee_status)},
                capture_output=True,
                text=True,
            )

        self.remove_watch("migrations/**")
        failed = check(0)
        self.assertNotEqual(failed.returncode, 0)
        self.assertIn("migrations/**: image input is uncovered", failed.stderr)
        self.assertNotIn("repository link check", failed.stdout)

        (self.root / IAC).write_text(FIXTURE[IAC], encoding="utf-8")
        passed = check(0)
        self.assertEqual(passed.returncode, 0, passed.stderr)
        self.assertIn("repository link check", passed.stdout)

        failed = check(7)
        self.assertIn("repository link check", failed.stdout)
        self.assertNotEqual(failed.returncode, 0, failed.stdout)


if __name__ == "__main__":
    unittest.main()
