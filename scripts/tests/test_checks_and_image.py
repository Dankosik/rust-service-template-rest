#!/usr/bin/env python3
"""Tests for tooling, Make gates, image scripts, and harness generation."""

from __future__ import annotations

import hashlib
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
sys.path.insert(0, str(ROOT / "scripts/template"))
from state import TEMPLATE_REPOSITORY


def _write_executable(path: Path, body: str) -> str:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\n" + body + "\n", encoding="utf-8")
    path.chmod(0o755)
    return str(path)


class MakeGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="make-gate-test-")))
        shutil.copytree(ROOT / "make", self.root / "make", ignore=shutil.ignore_patterns("source.mk"))
        (self.root / "tools").mkdir()
        shutil.copy2(ROOT / "tools/versions.env", self.root / "tools/versions.env")
        shutil.copy2(ROOT / "Makefile", self.root / "Makefile")

    def run_make(self, *args: str) -> subprocess.CompletedProcess[str]:
        env = {**os.environ, "LC_ALL": "C", "PATH": f"{self.root / 'bin'}{os.pathsep}{os.environ['PATH']}"}
        env.pop("DATABASE", None)
        return subprocess.run(["make", *args], cwd=self.root, env=env, capture_output=True, text=True)

    def install_source_make(self) -> Path:
        source = ROOT / "make/source.mk"
        if not source.is_file():
            self.skipTest("source template only")
        shutil.copy2(source, self.root / "make/source.mk")
        scripts = self.root / "scripts/template"
        scripts.mkdir(parents=True)
        return scripts

    def test_verify_check_propagates_initializer_matrix_failure(self) -> None:
        scripts = self.install_source_make()
        _write_executable(
            self.root / "bin/python3",
            'case "$*" in\n'
            '  "-m unittest "*) exit 0 ;;\n'
            '  "scripts/template/matrix.py --self-test") echo "matrix self-test"; exit "$MATRIX_STATUS" ;;\n'
            '  *) exit 2 ;;\n'
            'esac',
        )
        (scripts / "check_init.sh").write_text('echo "recorder self-test ran"\n', encoding="utf-8")

        failed = self.run_make("verify-check", "MATRIX_STATUS=7")
        self.assertIn("matrix self-test", failed.stdout, failed.stderr)
        self.assertNotEqual(failed.returncode, 0, failed.stdout)
        self.assertNotIn("recorder self-test ran", failed.stdout)

        passed = self.run_make("verify-check", "MATRIX_STATUS=0")
        self.assertEqual(passed.returncode, 0, passed.stderr)
        self.assertIn("recorder self-test ran", passed.stdout)

    def test_generated_make_has_no_initializer(self) -> None:
        result = self.run_make("template-init")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("No rule to make target", result.stderr)

    def test_source_make_captures_initializer_inputs_literally(self) -> None:
        scripts = self.install_source_make()
        (scripts / "init.py").write_text(
            'import os\nprint(os.environ["DESCRIPTION"])\nprint(os.environ["DATABASE"])\n',
            encoding="utf-8",
        )
        description = "Catalog ($5) $(SERVICE_NAME)"
        result = self.run_make("template-init", "SERVICE_NAME=catalog-api", f"DESCRIPTION={description}")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [description, "none"])


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


class CargoArtifactTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="cargo-artifact-")))
        self.manifest = self.temp / "crates/service/Cargo.toml"
        self.manifest.parent.mkdir(parents=True, exist_ok=True)
        self.manifest.write_text('[package]\nname = "service"\n', encoding="utf-8")

    def run_selector(self, kind: str, name: str, records: list[dict | str]) -> subprocess.CompletedProcess[str]:
        stream = "\n".join(item if isinstance(item, str) else json.dumps(item) for item in records) + "\n"
        return subprocess.run(
            [
                sys.executable,
                str(ROOT / "scripts/image/cargo_artifact.py"),
                "--manifest",
                str(self.manifest),
                "--name",
                name,
                "--kind",
                kind,
            ],
            input=stream,
            capture_output=True,
            text=True,
        )

    def _artifact(self, *, name: str, kind: str, test_profile: bool, executable: str | None) -> dict:
        return {
            "reason": "compiler-artifact",
            "manifest_path": str(self.manifest.resolve()),
            "target": {"name": name, "kind": [kind]},
            "profile": {"test": test_profile},
            "executable": executable,
        }

    def test_selects_bin_example_and_test_executables_cleanly(self) -> None:
        bin_exe = str((self.temp / "target/debug/openapi").resolve())
        bin_unit_test = str((self.temp / "target/debug/deps/openapi-1234").resolve())
        ex_exe = str((self.temp / "target/release/examples/runtime_progress").resolve())
        test_exe = str((self.temp / "target/release/deps/runtime_progress-9999").resolve())

        records: list[dict | str] = [
            "not-json",
            {"reason": "build-script-executed"},
            self._artifact(name="openapi", kind="bin", test_profile=False, executable=bin_exe),
            self._artifact(name="openapi", kind="bin", test_profile=True, executable=bin_unit_test),
            self._artifact(name="runtime_progress", kind="example", test_profile=False, executable=ex_exe),
            self._artifact(name="runtime_progress", kind="test", test_profile=True, executable=test_exe),
        ]

        selected_bin = self.run_selector("bin", "openapi", records)
        self.assertEqual(selected_bin.returncode, 0, selected_bin.stderr)
        self.assertEqual(selected_bin.stdout.strip(), bin_exe)

        selected_example = self.run_selector("example", "runtime_progress", records)
        self.assertEqual(selected_example.returncode, 0, selected_example.stderr)
        self.assertEqual(selected_example.stdout.strip(), ex_exe)

        selected_test = self.run_selector("test", "runtime_progress", records)
        self.assertEqual(selected_test.returncode, 0, selected_test.stderr)
        self.assertEqual(selected_test.stdout.strip(), test_exe)

    def test_refuses_missing_duplicate_or_relative_executables(self) -> None:
        exe = str((self.temp / "target/release/deps/runtime_progress-1").resolve())
        self.assertNotEqual(self.run_selector("test", "runtime_progress", []).returncode, 0)
        duplicate = self.run_selector(
            "test",
            "runtime_progress",
            [
                self._artifact(name="runtime_progress", kind="test", test_profile=True, executable=exe),
                self._artifact(name="runtime_progress", kind="test", test_profile=True, executable=exe),
            ],
        )
        self.assertNotEqual(duplicate.returncode, 0)
        relative = self.run_selector(
            "bin",
            "openapi",
            [self._artifact(name="openapi", kind="bin", test_profile=False, executable="relative/openapi")],
        )
        self.assertNotEqual(relative.returncode, 0)


class PublishMetadataTests(unittest.TestCase):
    SHA = "0123456789abcdef0123456789abcdef01234567"
    CONFIG_ID = "sha256:" + "a" * 64
    OTHER_ID = "sha256:" + "b" * 64
    RUN_IMAGE = "ghcr.io/owner/service:build-42-1"

    def setUp(self) -> None:
        self.temp = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="publish-image-")))
        self.bin_dir = self.temp / "bin"
        self.bin_dir.mkdir()
        self.summary = self.temp / "summary.md"
        self.github_output = self.temp / "github_output"
        self.export_json = self.temp / "export.json"
        self.manifest = self.temp / "manifest"
        self.state_file = self.temp / "docker_state.json"

        docker_stub = self.bin_dir / "docker"
        docker_stub.write_text(
            f'''#!/usr/bin/env python3
import json, pathlib, sys
state = json.loads(pathlib.Path({str(self.state_file)!r}).read_text(encoding="utf-8"))
args = sys.argv[1:]
if args[0] == "image":
    print(state["loaded_id"])
elif args[0] == "tag":
    sys.exit(0 if args[1] == state["expected_tag_source"] else 1)
elif args[0] == "push":
    if args[1] == state["run_image"] and state.get("push_lines"):
        print(state["push_lines"])
    sys.exit(1 if args[1] == state.get("fail_tag") else 0)
elif args[0] == "buildx":
    if args[3] == "--raw":
        if args[4] != state["run_image"]:
            sys.exit(1)
        sys.stdout.buffer.write(pathlib.Path(state["manifest_path"]).read_bytes())
    else:
        print(state["confirmed_digest"])
    sys.exit(1 if state.get("readback_failed") else 0)
else:
    sys.exit(1)
''',
            encoding="utf-8",
        )
        docker_stub.chmod(0o755)
        self.env = {
            **os.environ,
            "PATH": f"{self.bin_dir}{os.pathsep}{os.environ['PATH']}",
        }

    def run_publish(self, *args: str, extra_env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        merged = dict(self.env)
        if extra_env:
            merged.update(extra_env)
        return subprocess.run(
            [sys.executable, str(ROOT / "scripts/image/publish_metadata.py"), *args],
            cwd=self.temp,
            env=merged,
            capture_output=True,
            text=True,
        )

    def write_docker_state(self, **overrides: object) -> None:
        base = {
            "loaded_id": self.CONFIG_ID,
            "expected_tag_source": self.CONFIG_ID,
            "run_image": self.RUN_IMAGE,
            "manifest_path": str(self.manifest),
            "push_lines": "",
            "confirmed_digest": "",
            "fail_tag": "",
            "readback_failed": False,
        }
        base.update(overrides)
        self.state_file.write_text(json.dumps(base), encoding="utf-8")

    def test_prepare_and_metadata_modes_and_refusals(self) -> None:
        main_res = self.run_publish("prepare", "main", self.SHA, "Owner/Service", "main", "", "", "42", "3", "0.1.0")
        self.assertEqual(main_res.returncode, 0, main_res.stderr)
        expected_main = (
            "image_repo=ghcr.io/owner/service\n"
            "primary_image=ghcr.io/owner/service:build-42-3\n"
            "version=0.1.0\n"
            "promote_tags=sha-0123456789ab main\n"
            "artifact_name=sbom-main-0123456789ab-42-3\n"
            "identity=https://github.com/Owner/Service/.github/workflows/cd.yml@refs/heads/main\n"
        )
        self.assertEqual(main_res.stdout, expected_main)

        (self.temp / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8")
        svc_toml = self.temp / "crates/service/Cargo.toml"
        svc_toml.parent.mkdir(parents=True, exist_ok=True)
        svc_toml.write_text('[package]\nname = "service"\nversion.workspace = true\n', encoding="utf-8")

        prep_res = self.run_publish(
            "prepare",
            extra_env={
                "MODE": "release",
                "CANDIDATE_SHA": self.SHA,
                "GITHUB_REPOSITORY": "Owner/Service",
                "DEFAULT_BRANCH": "main",
                "GITHUB_REF_NAME": "v1.2.3",
                "GITHUB_REF": "refs/tags/v1.2.3",
                "GITHUB_RUN_ID": "42",
                "GITHUB_RUN_ATTEMPT": "4",
                "GITHUB_OUTPUT": str(self.github_output),
                "PUBLISH_REPO_ROOT": str(self.temp),
            },
        )
        self.assertEqual(prep_res.returncode, 0, prep_res.stderr)
        expected_release = (
            "image_repo=ghcr.io/owner/service\n"
            "primary_image=ghcr.io/owner/service:build-42-4\n"
            "version=1.2.3\n"
            "promote_tags=v1.2.3 latest\n"
            "artifact_name=sbom-v1.2.3-42-4\n"
            "identity=https://github.com/Owner/Service/.github/workflows/cd.yml@refs/tags/v1.2.3\n"
        )
        self.assertEqual(self.github_output.read_text(encoding="utf-8"), expected_release)

        for bad_args in (
            ("prepare", "main", "invalid", "Owner/Service", "main", "", "", "42", "1", "0.1.0"),
            ("prepare", "release", self.SHA, "Owner/Service", "main", "v1/bad", "refs/tags/v1/bad", "42", "1", "1.0.0"),
            ("prepare", "release", self.SHA, "Owner/Service", "main", "v1.2.4", "refs/tags/v1.2.4", "42", "1", "1.2.3"),
            ("prepare", "main", self.SHA, "Owner/Service", "main", "", "", "42", "1", "not-a-version"),
            ("prepare", "unknown", self.SHA, "Owner/Service", "main", "", "", "42", "1", "0.1.0"),
        ):
            with self.subTest(bad_args=bad_args):
                self.assertNotEqual(self.run_publish(*bad_args).returncode, 0)

    def test_identify_push_and_promote_verification(self) -> None:
        manifest_bytes = f'{{"schemaVersion":2,"config":{{"digest":"{self.CONFIG_ID}"}},"layers":[]}}\n\n'.encode()
        self.manifest.write_bytes(manifest_bytes)
        digest = "sha256:" + hashlib.sha256(manifest_bytes).hexdigest()
        self.export_json.write_text(
            json.dumps(
                {
                    "containerimage.digest": digest,
                    "containerimage.descriptor": {
                        "digest": digest,
                        "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    },
                    "containerimage.config.digest": self.CONFIG_ID,
                }
            ),
            encoding="utf-8",
        )
        push_line = f"build-42-1: digest: {digest} size: 123"

        for kind, loaded in (("config", self.CONFIG_ID), ("descriptor", digest)):
            with self.subTest(kind=kind):
                self.write_docker_state(loaded_id=loaded, push_lines=push_line, confirmed_digest=digest)
                ident = self.run_publish("identify", self.RUN_IMAGE, str(self.export_json))
                self.assertEqual(ident.returncode, 0, ident.stderr)
                self.assertEqual(ident.stdout, f"image_id={loaded}\nid_kind={kind}\n")
                pushed = self.run_publish("push", self.RUN_IMAGE, loaded, kind)
                self.assertEqual(pushed.returncode, 0, pushed.stderr)
                self.assertEqual(pushed.stdout.strip(), digest)

        # Replaced run tag or mismatched subject refuses.
        self.write_docker_state(loaded_id=self.OTHER_ID, push_lines=push_line, confirmed_digest=digest)
        self.assertNotEqual(self.run_publish("identify", self.RUN_IMAGE, str(self.export_json)).returncode, 0)
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.OTHER_ID, "config").returncode, 0)
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.OTHER_ID, "descriptor").returncode, 0)

        # Failed push / readback / duplicate or wrong-tag push line refuses.
        self.write_docker_state(
            loaded_id=self.CONFIG_ID,
            push_lines=push_line,
            confirmed_digest=digest,
            fail_tag=self.RUN_IMAGE,
        )
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)
        self.write_docker_state(
            loaded_id=self.CONFIG_ID,
            push_lines=push_line,
            confirmed_digest=digest,
            readback_failed=True,
        )
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)
        self.write_docker_state(
            loaded_id=self.CONFIG_ID,
            push_lines=f"{push_line}\n{push_line}",
            confirmed_digest=digest,
        )
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)
        self.write_docker_state(
            loaded_id=self.CONFIG_ID,
            push_lines=f"another-tag: digest: {digest} size: 123",
            confirmed_digest=digest,
        )
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)

        # Altered registry bytes refuse.
        self.manifest.write_bytes(manifest_bytes + b"\n")
        self.write_docker_state(loaded_id=self.CONFIG_ID, push_lines=push_line, confirmed_digest=digest)
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)

        # OCI image index refuses config ID, admits descriptor ID.
        index_bytes = (
            f'{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json",'
            f'"manifests":[{{"digest":"{digest}"}}]}}'
        ).encode()
        self.manifest.write_bytes(index_bytes)
        index_digest = "sha256:" + hashlib.sha256(index_bytes).hexdigest()
        index_push_line = f"build-42-1: digest: {index_digest} size: 155"
        self.export_json.write_text(
            json.dumps(
                {
                    "containerimage.digest": index_digest,
                    "containerimage.descriptor": {
                        "digest": index_digest,
                        "mediaType": "application/vnd.oci.image.index.v1+json",
                    },
                    "containerimage.config.digest": self.CONFIG_ID,
                }
            ),
            encoding="utf-8",
        )
        self.write_docker_state(loaded_id=self.CONFIG_ID, push_lines=index_push_line, confirmed_digest=index_digest)
        self.assertNotEqual(self.run_publish("identify", self.RUN_IMAGE, str(self.export_json)).returncode, 0)
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)

        self.write_docker_state(loaded_id=index_digest, push_lines=index_push_line, confirmed_digest=index_digest)
        self.assertEqual(self.run_publish("identify", self.RUN_IMAGE, str(self.export_json)).returncode, 0)
        self.assertEqual(self.run_publish("push", self.RUN_IMAGE, index_digest, "descriptor").returncode, 0)

        # Promote and promote-summary verification & partial failure receipts.
        self.write_docker_state(confirmed_digest=index_digest)
        ok_promote = self.run_publish(
            "promote-summary",
            extra_env={
                "IMAGE_REPO": "ghcr.io/owner/service",
                "IMAGE_ID": self.CONFIG_ID,
                "DIGEST": index_digest,
                "PROMOTE_TAGS": "sha-0123456789ab main",
                "GITHUB_STEP_SUMMARY": str(self.summary),
            },
        )
        self.assertEqual(ok_promote.returncode, 0, ok_promote.stderr)
        self.assertIn(f"Confirmed tag: ghcr.io/owner/service:main at {index_digest}", self.summary.read_text())

        self.summary.write_text("", encoding="utf-8")
        self.write_docker_state(confirmed_digest=index_digest, fail_tag="ghcr.io/owner/service:main")
        second_fail = self.run_publish(
            "promote-summary",
            "ghcr.io/owner/service",
            self.CONFIG_ID,
            index_digest,
            "sha-0123456789ab main",
            str(self.summary),
        )
        self.assertNotEqual(second_fail.returncode, 0)
        self.assertIn("Promoted tags before failure: sha-0123456789ab", self.summary.read_text(encoding="utf-8"))

        self.summary.write_text("", encoding="utf-8")
        self.write_docker_state(confirmed_digest=index_digest, fail_tag="ghcr.io/owner/service:sha-0123456789ab")
        first_fail = self.run_publish(
            "promote-summary",
            "ghcr.io/owner/service",
            self.CONFIG_ID,
            index_digest,
            "sha-0123456789ab main",
            str(self.summary),
        )
        self.assertNotEqual(first_fail.returncode, 0)
        self.assertIn("Promoted tags before failure: none", self.summary.read_text(encoding="utf-8"))

        self.summary.write_text("", encoding="utf-8")
        self.write_docker_state(confirmed_digest=self.OTHER_ID)
        wrong_digest = self.run_publish(
            "promote-summary",
            "ghcr.io/owner/service",
            self.CONFIG_ID,
            index_digest,
            "sha-0123456789ab main",
            str(self.summary),
        )
        self.assertNotEqual(wrong_digest.returncode, 0)


class HarnessSyncTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="harness-sync-")))
        for subdir in ("roles", "role-classes", "adapters"):
            src = ROOT / ".agents" / subdir
            if src.is_dir():
                shutil.copytree(src, self.repo / ".agents" / subdir)
        codex_proj = ROOT / ".agents/codex-project.toml"
        if codex_proj.is_file():
            shutil.copy2(codex_proj, self.repo / ".agents/codex-project.toml")
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

    def test_preflight_flag_is_removed(self) -> None:
        res = self.run_sync("--preflight")
        self.assertEqual(res.returncode, 2)

    def test_apply_check_and_skill_validation_without_license(self) -> None:
        check_unmaterialized = self.run_sync("--check", "--harness", "claude")
        self.assertEqual(check_unmaterialized.returncode, 0, check_unmaterialized.stderr)

        require_mat = self.run_sync("--check", "--harness", "claude", "--require-materialized")
        self.assertNotEqual(require_mat.returncode, 0)

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


if __name__ == "__main__":
    unittest.main()
