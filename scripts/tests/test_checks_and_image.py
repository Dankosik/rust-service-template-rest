#!/usr/bin/env python3
"""Tests for tool pins, npx wrapper policy, and dependency drift."""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[2]


def _write_executable(path: Path, body: str) -> str:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\n" + body + "\n", encoding="utf-8")
    path.chmod(0o755)
    return str(path)


class NpxWrapperTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="npx-test-")))
        self.bin_dir = self.temp / "bin"
        self.bin_dir.mkdir()
        self.capture = self.temp / "captured.json"
        npx_stub = self.bin_dir / "npx"
        npx_stub.write_text(
            f'''#!/usr/bin/env python3
import json, os, pathlib, sys
keys = (
    "npm_config_prefer_offline",
    "npm_config_ignore_scripts",
    "npm_config_audit",
    "npm_config_fund",
    "npm_config_update_notifier",
    "npm_config_before",
)
payload = {{"argv": sys.argv[1:], "env": {{k: os.environ.get(k) for k in keys}}}}
pathlib.Path({str(self.capture)!r}).write_text(json.dumps(payload), encoding="utf-8")
''',
            encoding="utf-8",
        )
        npx_stub.chmod(0o755)

    def test_sets_npm_hardening_and_cooldown_before_exec(self) -> None:
        env = {
            **os.environ,
            "PATH": f"{self.bin_dir}{os.pathsep}{os.environ['PATH']}",
        }
        env.pop("NPX_COOLDOWN_DAYS", None)
        started = datetime.now(timezone.utc)
        result = subprocess.run(
            [sys.executable, str(ROOT / "scripts/checks/npx.py"), "jscpd@5.4.2", "--config", ".jscpd.json", "."],
            env=env,
            capture_output=True,
            text=True,
        )
        ended = datetime.now(timezone.utc)
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(self.capture.read_text(encoding="utf-8"))
        self.assertEqual(data["argv"], ["--yes", "jscpd@5.4.2", "--config", ".jscpd.json", "."])
        self.assertEqual(data["env"]["npm_config_prefer_offline"], "true")
        self.assertEqual(data["env"]["npm_config_ignore_scripts"], "true")
        self.assertEqual(data["env"]["npm_config_audit"], "false")
        self.assertEqual(data["env"]["npm_config_fund"], "false")
        self.assertEqual(data["env"]["npm_config_update_notifier"], "false")
        before = datetime.strptime(data["env"]["npm_config_before"], "%Y-%m-%dT%H:%M:%SZ").replace(
            tzinfo=timezone.utc
        )
        self.assertGreaterEqual(before, started.replace(microsecond=0) - timedelta(days=7, seconds=2))
        self.assertLessEqual(before, ended + timedelta(seconds=2) - timedelta(days=7))

    def test_honors_cooldown_override_and_refuses_bad_inputs(self) -> None:
        base_env = {**os.environ, "PATH": f"{self.bin_dir}{os.pathsep}{os.environ['PATH']}"}
        custom = subprocess.run(
            [sys.executable, str(ROOT / "scripts/checks/npx.py"), "@redocly/cli@2.53.3", "lint"],
            env={**base_env, "NPX_COOLDOWN_DAYS": "14"},
            capture_output=True,
            text=True,
        )
        self.assertEqual(custom.returncode, 0, custom.stderr)
        data = json.loads(self.capture.read_text(encoding="utf-8"))
        self.assertEqual(data["argv"], ["--yes", "@redocly/cli@2.53.3", "lint"])

        for bad_days in ("-1", "abc", ""):
            with self.subTest(bad_days=bad_days):
                bad = subprocess.run(
                    [sys.executable, str(ROOT / "scripts/checks/npx.py"), "jscpd@5.4.2"],
                    env={**base_env, "NPX_COOLDOWN_DAYS": bad_days},
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(bad.returncode, 2)

        no_args = subprocess.run(
            [sys.executable, str(ROOT / "scripts/checks/npx.py")],
            env=base_env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(no_args.returncode, 2)

        no_npx = subprocess.run(
            [sys.executable, str(ROOT / "scripts/checks/npx.py"), "jscpd@5.4.2"],
            env={**base_env, "PATH": str(self.temp / "empty-path")},
            capture_output=True,
            text=True,
        )
        self.assertEqual(no_npx.returncode, 2)
        self.assertIn("Node.js (npx) is required", no_npx.stderr)


class ToolsCheckTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="tools-check-")))
        for path in (
            "tools/versions.env",
            "build/docker/Dockerfile",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            ".github/dependabot.yml",
            "scripts/checks/npx.py",
            "scripts/checks/tools.py",
        ):
            target = self.root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / path, target)
        if (ROOT / "env/docker-compose.yml").is_file():
            compose_dst = self.root / "env/docker-compose.yml"
            compose_dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / "env/docker-compose.yml", compose_dst)
        drift_dst = self.root / "scripts/checks/dependency_drift.sh"
        drift_dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / "scripts/checks/dependency_drift.sh", drift_dst)
        mk = self.root / "make/template.mk"
        mk.parent.mkdir(parents=True, exist_ok=True)
        mk.write_text(
            "NPX := python3 scripts/checks/npx.py\n"
            "openapi-lint:\n"
            '\t@command -v npx >/dev/null 2>&1 || { echo "requires Node.js (npx)" >&2; exit 2; }\n'
            "\t$(NPX) @redocly/cli@$(REDOCLY_CLI_VERSION) lint\n",
            encoding="utf-8",
        )
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        self.env = {
            **os.environ,
            "PATH": f"{self.bin_dir}{os.pathsep}{os.environ['PATH']}",
            "EMPTY_BANNER": "0",
            "VERSION_STATUS": "0",
        }
        for var in ("CARGO_DENY", "CARGO_NEXTEST", "CARGO_SHEAR", "TYPOS", "ZIZMOR"):
            banner = f'. "{self.root}/tools/versions.env"\nprintf "tool v%s (build 1.0.0)\\n" "${{{var}_VERSION}}"'
            if var == "CARGO_DENY":
                banner = f'[ "$EMPTY_BANNER" = 1 ] || {{ {banner}; }}\nexit "$VERSION_STATUS"'
            self.env[var] = _write_executable(self.bin_dir / var.lower(), banner)

    def run_tools(self, **overrides: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(self.root / "scripts/checks/tools.py"), "--repo", str(self.root)],
            cwd=self.root,
            env={**self.env, **overrides},
            capture_output=True,
            text=True,
        )

    def test_valid_fixture_and_cargo_tool_version_banners(self) -> None:
        for empty, status, success in (("0", "0", True), ("0", "1", False), ("1", "0", False)):
            with self.subTest(empty=empty, status=status):
                result = self.run_tools(EMPTY_BANNER=empty, VERSION_STATUS=status)
                self.assertEqual(result.returncode == 0, success, result.stderr)

        env_without_deny = {k: v for k, v in self.env.items() if k != "CARGO_DENY"}
        unset_res = subprocess.run(
            [sys.executable, str(self.root / "scripts/checks/tools.py"), "--repo", str(self.root)],
            cwd=self.root,
            env=env_without_deny,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(unset_res.returncode, 0)
        self.assertIn("CARGO_DENY is unset", unset_res.stderr)

        root_mk = self.root / "Makefile"
        root_mk.write_text("lint:\n\tnpx --yes jscpd@5.4.2 .\n", encoding="utf-8")
        raw_root_mk = self.run_tools()
        self.assertNotEqual(raw_root_mk.returncode, 0)
        self.assertIn("Makefile:2 invokes npx directly", raw_root_mk.stderr)
        root_mk.unlink()

    def test_dockerfile_scratch_stage_alias_digest_and_channel_parity(self) -> None:
        dockerfile = self.root / "build/docker/Dockerfile"
        original = dockerfile.read_text(encoding="utf-8")
        dockerfile.write_text(original + "\nFROM scratch AS empty-stage\nFROM empty-stage\n", encoding="utf-8")
        self.assertEqual(self.run_tools().returncode, 0)

        dockerfile.write_text(original + "\nFROM debian:trixie-slim\n", encoding="utf-8")
        unpinned = self.run_tools()
        self.assertNotEqual(unpinned.returncode, 0)
        self.assertIn("not pinned by digest", unpinned.stderr)

        dockerfile.write_text(original.replace("rust:1.", "rust:9."), encoding="utf-8")
        wrong_channel = self.run_tools()
        self.assertNotEqual(wrong_channel.returncode, 0)
        self.assertIn("rust-toolchain.toml pins", wrong_channel.stderr)

        dockerfile.write_text(
            original.replace("CARGO_CHEF_VERSION=", "CARGO_CHEF_VERSION=0.0.0"),
            encoding="utf-8",
        )
        wrong_arg = self.run_tools()
        self.assertNotEqual(wrong_arg.returncode, 0)
        self.assertIn("CARGO_CHEF_VERSION", wrong_arg.stderr)

    def test_compose_image_digests_when_present(self) -> None:
        compose = self.root / "env/docker-compose.yml"
        compose.parent.mkdir(parents=True, exist_ok=True)
        compose.write_text("services:\n  redis:\n    image: valkey/valkey:9.1.3-alpine\n", encoding="utf-8")
        unpinned = self.run_tools()
        self.assertNotEqual(unpinned.returncode, 0)
        self.assertIn("env/docker-compose.yml", unpinned.stderr)

        compose.unlink()
        self.assertEqual(self.run_tools().returncode, 0)

    def test_cooldown_parity_and_raw_npx_bypass_detection(self) -> None:
        npx_py = self.root / "scripts/checks/npx.py"
        original_npx = npx_py.read_text(encoding="utf-8")
        npx_py.write_text(
            original_npx.replace("DEFAULT_COOLDOWN_DAYS = 7", "DEFAULT_COOLDOWN_DAYS = 3"),
            encoding="utf-8",
        )
        drift = self.run_tools()
        self.assertNotEqual(drift.returncode, 0)
        self.assertIn("DEFAULT_COOLDOWN_DAYS", drift.stderr)
        npx_py.write_text(original_npx, encoding="utf-8")

        cargo_cfg = self.root / ".cargo/config.toml"
        original_cargo = cargo_cfg.read_text(encoding="utf-8")
        cargo_cfg.write_text(original_cargo.replace('"7 days"', '"3 days"'), encoding="utf-8")
        bad_cargo = self.run_tools()
        self.assertNotEqual(bad_cargo.returncode, 0)
        self.assertIn("global-min-publish-age", bad_cargo.stderr)
        cargo_cfg.write_text(original_cargo, encoding="utf-8")

        for path in (".railway/package.json", ".railway/.npmrc"):
            target = self.root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / path, target)
        self.assertEqual(self.run_tools().returncode, 0)
        npmrc = self.root / ".railway/.npmrc"
        original_npmrc = npmrc.read_text(encoding="utf-8")
        npmrc.write_text("ignore-scripts=true\nmin-release-age=3\n", encoding="utf-8")
        bad_age = self.run_tools()
        self.assertNotEqual(bad_age.returncode, 0)
        self.assertIn("min-release-age", bad_age.stderr)
        npmrc.write_text("ignore-scripts=false\nmin-release-age=7\n", encoding="utf-8")
        bad_scripts = self.run_tools()
        self.assertNotEqual(bad_scripts.returncode, 0)
        self.assertIn("ignore-scripts", bad_scripts.stderr)
        npmrc.write_text(original_npmrc, encoding="utf-8")

        if (ROOT / "tools/grpc-codegen/Cargo.toml").is_file():
            for path in ("Cargo.toml", "tools/grpc-codegen/Cargo.toml"):
                target = self.root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / path, target)
            codegen_toml = self.root / "tools/grpc-codegen/Cargo.toml"
            original_codegen = codegen_toml.read_text(encoding="utf-8")
            codegen_toml.write_text(
                re.sub(
                    r'(tonic-prost-build\s*=\s*\{\s*version\s*=\s*)"[^"]+"',
                    r'\1"=0.0.1"',
                    original_codegen,
                ),
                encoding="utf-8",
            )
            bad_codegen = self.run_tools()
            self.assertNotEqual(bad_codegen.returncode, 0)
            self.assertIn("tonic-prost-build", bad_codegen.stderr)
            codegen_toml.write_text(
                original_codegen.replace('bare_urls = "warn"', 'bare_urls = "allow"'),
                encoding="utf-8",
            )
            bad_lints = self.run_tools()
            self.assertNotEqual(bad_lints.returncode, 0)
            self.assertIn("[lints] differs", bad_lints.stderr)
            codegen_toml.write_text(original_codegen, encoding="utf-8")

        bypass_sh = self.root / "scripts/profiles/bypass.sh"
        bypass_sh.parent.mkdir(parents=True, exist_ok=True)
        bypass_sh.write_text('#!/usr/bin/env bash\nnpx --yes squawk-cli@2.70.0\n', encoding="utf-8")
        sh_result = self.run_tools()
        self.assertNotEqual(sh_result.returncode, 0)
        self.assertIn("scripts/profiles/bypass.sh", sh_result.stderr)
        bypass_sh.unlink()

        bypass_py = self.root / "scripts/verify/bypass.py"
        bypass_py.parent.mkdir(parents=True, exist_ok=True)
        bypass_py.write_text('import subprocess\nsubprocess.run(["npx", "--yes", "jscpd@5.4.2"])\n', encoding="utf-8")
        py_result = self.run_tools()
        self.assertNotEqual(py_result.returncode, 0)
        self.assertIn("scripts/verify/bypass.py", py_result.stderr)
        bypass_py.unlink()

        benign_py = self.root / "scripts/verify/benign.py"
        benign_py.write_text(
            'import shutil\nfor tool in ("npx", "go"):\n    shutil.which(tool)\n',
            encoding="utf-8",
        )
        self.assertEqual(self.run_tools().returncode, 0)


class DependencyDriftTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="dependency-drift-")))
        script = self.root / "scripts/checks/dependency_drift.sh"
        script.parent.mkdir(parents=True)
        shutil.copy2(ROOT / "scripts/checks/dependency_drift.sh", script)
        (self.root / "Cargo.toml").write_text(
            '[patch.crates-io]\ntransport = { package = "example", path = "vendor/example" }\n', encoding="utf-8"
        )
        self.lock = self.root / "Cargo.lock"
        self.lock.write_text('[[package]]\nname = "example"\nversion = "1.2.3"\n', encoding="utf-8")
        fork = self.root / "vendor/example"
        fork.mkdir(parents=True)
        (fork / "Cargo.toml").write_text('[package]\nname = "example"\nversion = "1.2.3"\n', encoding="utf-8")
        self.card = fork / "PATCHES.md"
        self.card.write_text("Upstream reviewed: 1.2.3\n", encoding="utf-8")
        self.pins = self.root / "tools/versions.env"
        self.pins.parent.mkdir()
        self.pins.write_text("RUFF_VERSION=0.17.4\n", encoding="utf-8")
        self.env = {
            **os.environ,
            "PATH": f"{self.root / 'bin'}{os.pathsep}{os.environ['PATH']}",
            "OSV_RESPONSE": "{}",
            "CRATE_RESPONSE": json.dumps({
                "crate": {"name": "example"},
                "versions": [{"num": "1.2.3", "yanked": False, "created_at": "2024-01-01T00:00:00Z"}],
            }),
            "PYPI_RESPONSE": json.dumps({
                "info": {"version": "0.17.4"},
                "urls": [{"upload_time_iso_8601": "2024-01-01T00:00:00.000000Z"}],
            }),
        }
        _write_executable(
            self.root / "bin/curl",
            'case "$*" in\n'
            '  *api.osv.dev/v1/query*) response="$OSV_RESPONSE" ;;\n'
            '  *crates.io/api/v1/crates/example*) response="$CRATE_RESPONSE" ;;\n'
            '  *pypi.org/pypi/ruff/json*) response="$PYPI_RESPONSE" ;;\n'
            '  *) exit 22 ;;\n'
            'esac\n'
            '[ "$response" != unavailable ] || exit 1\n'
            'printf "%s" "$response"',
        )

    def run_drift(self, *args: str, **responses: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", "scripts/checks/dependency_drift.sh", *args],
            cwd=self.root,
            env={**self.env, **responses},
            capture_output=True,
            text=True,
        )

    def test_patch_requires_card(self) -> None:
        self.card.unlink()
        result = self.run_drift()
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("vendor/example/PATCHES.md", result.stderr)

    def test_patch_requires_exact_locked_version(self) -> None:
        self.lock.write_text('[[package]]\nname = "example"\nversion = "1.2.2"\n', encoding="utf-8")
        result = self.run_drift()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("Cargo.lock does not resolve the vendored version", result.stdout)

    def test_incomplete_fork_queries_fail(self) -> None:
        for source, overrides in (
            ("OSV", {"OSV_RESPONSE": "unavailable"}),
            ("OSV", {"OSV_RESPONSE": '{"error":"temporary"}'}),
            ("OSV", {"OSV_RESPONSE": '{"next_page_token":"incomplete"}'}),
            ("crates.io", {"CRATE_RESPONSE": '{"crate":{"name":"example"},"versions":[{"num":"1.2.3"}]}'}),
        ):
            with self.subTest(overrides=overrides):
                result = self.run_drift(**overrides)
                self.assertEqual(result.returncode, 2, result.stdout)
                self.assertIn(source.lower(), result.stderr.lower())

    def test_incomplete_weekly_poll_has_no_report_even_after_a_finding(self) -> None:
        advisory = '{"vulns":[{"id":"RUSTSEC-2024-0001"}]}'
        complete = self.run_drift("--upstream", OSV_RESPONSE=advisory)
        self.assertEqual(complete.returncode, 1, complete.stderr)
        self.assertIn("RUSTSEC-2024-0001", complete.stdout)

        invalid_time = self.env["PYPI_RESPONSE"].replace("2024-01-01", "2024-02-30")
        for response in ("unavailable", "", "{}", invalid_time):
            with self.subTest(response=response):
                result = self.run_drift("--upstream", OSV_RESPONSE=advisory, PYPI_RESPONSE=response)
                self.assertEqual(result.returncode, 2, result.stdout)
                self.assertEqual(result.stdout, "")
                self.assertIn("ruff", result.stderr)

    def test_profile_without_forks_or_tool_pins_needs_no_network(self) -> None:
        (self.root / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
        self.pins.write_text("", encoding="utf-8")
        result = self.run_drift("--upstream", OSV_RESPONSE="unavailable", CRATE_RESPONSE="unavailable")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no finding", result.stdout)


if __name__ == "__main__":
    unittest.main()
