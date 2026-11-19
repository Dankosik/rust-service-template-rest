"""Unit tests for template initializer, harness sync, CI change detection, and image publication."""

from __future__ import annotations

import importlib.util
import os
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

from template import harness, init as template_init, runtime_progress

ROOT = Path(__file__).resolve().parents[2]


def _load(rel_path: str, module_name: str) -> Any:
    spec = importlib.util.spec_from_file_location(module_name, ROOT / rel_path)
    assert spec is not None and spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = mod
    spec.loader.exec_module(mod)
    return mod


DETECT = _load(".github/actions/detect-changes/detect.py", "_test_detect")
PUB = _load(".github/actions/publish-image/publish.py", "_test_pub")


class DetectChangesTests(unittest.TestCase):
    def test_classifies_surfaces_and_pruned_profiles(self) -> None:
        res = DETECT.classify(ROOT, ["crates/service/src/main.rs"])
        self.assertTrue(res["rust"])
        self.assertTrue(res["openapi"])
        self.assertFalse(res["grpc"])
        self.assertFalse(res["migrations"])

        with tempfile.TemporaryDirectory() as tmp:
            empty = Path(tmp)
            all_empty = DETECT.classify(empty, None)
            self.assertFalse(all_empty["grpc"])
            self.assertFalse(all_empty["migrations"])
            self.assertFalse(all_empty["template"])
            self.assertTrue(all_empty["rust"])

            (empty / "env").mkdir()
            (empty / "env/docker-compose.yml").write_text("services: {}\n", encoding="utf-8")
            (empty / "crates/infra-oauth2-client-credentials").mkdir(parents=True)
            oauth_only = DETECT.classify(empty, None)
            self.assertTrue(oauth_only["oauth"])
            self.assertFalse(oauth_only["integration"])

        lib_res = DETECT.classify(ROOT, ["scripts/lib.py"])
        self.assertTrue(lib_res["rust"])
        self.assertTrue(lib_res["deps"])
        self.assertTrue(lib_res["grpc"])
        self.assertTrue(lib_res["migrations"])

        arch_res = DETECT.classify(ROOT, ["quality/architecture.json"])
        self.assertTrue(arch_res["rust"])


class PublishMetadataTests(unittest.TestCase):
    def test_main_and_release_metadata(self) -> None:
        sha = "a" * 40
        main_meta = PUB.compute_metadata(
            mode="main", candidate_sha=sha, repository="Org/Svc", default_branch="main",
            ref_name="main", ref="refs/heads/main", run_id="10", run_attempt="1", crate_version="1.2.3",
        )
        self.assertEqual(main_meta["image_repo"], "ghcr.io/org/svc")
        self.assertEqual(main_meta["promote_tags"], f"sha-{sha[:12]} main")

        rel_meta = PUB.compute_metadata(
            mode="release", candidate_sha=sha, repository="Org/Svc", default_branch="main",
            ref_name="v1.2.3", ref="refs/tags/v1.2.3", run_id="11", run_attempt="1", crate_version="1.2.3",
        )
        self.assertEqual(rel_meta["promote_tags"], "v1.2.3 latest")

        with self.assertRaises(PUB.PublishError):
            PUB.compute_metadata(
                mode="release", candidate_sha=sha, repository="Org/Svc", default_branch="main",
                ref_name="v1.2.4", ref="refs/tags/v1.2.4", run_id="11", run_attempt="1", crate_version="1.2.3",
            )


class TemplateInitValidationTests(unittest.TestCase):
    def test_rejects_invalid_profiles_and_reserved_names(self) -> None:
        base = {
            "service_name": "orders-api",
            "repository": "https://github.com/org/orders",
            "description": "Orders",
            "codeowner": "@org/team",
        }
        self.assertEqual(template_init.validate_inputs(template_init.InitInputs(**base)).service_name, "orders-api")
        with self.assertRaises(template_init.InitError):
            template_init.validate_inputs(template_init.InitInputs(**{**base, "service_name": "async"}))
        with self.assertRaises(template_init.InitError):
            template_init.validate_inputs(template_init.InitInputs(**{**base, "jobs": "postgres"}))
        with self.assertRaises(template_init.InitError):
            template_init.validate_inputs(template_init.InitInputs(**{**base, "service_auth": "jwt"}))
        resolved = template_init.validate_inputs(
            template_init.InitInputs(**{**base, "outbound_auth": "oauth2-client-credentials"})
        )
        self.assertEqual(resolved.outbound_http, "bounded")

    def test_prune_mise_lock_keeps_only_retained_tools(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            staged = Path(tmp)
            (staged / "mise.toml").write_text('[tools]\njust = "1.59.0"\n', encoding="utf-8")
            (staged / "mise.lock").write_text(
                'lockfile_version = 4\n\n'
                '[[tools.buf]]\nversion = "1.75.0"\n\n'
                '[[tools."github:sbdchd/squawk"]]\nversion = "2.70.0"\n\n'
                '[[tools.just]]\nversion = "1.59.0"\n',
                encoding="utf-8",
            )
            template_init._prune_mise_lock(staged)
            parsed = tomllib.loads((staged / "mise.lock").read_text(encoding="utf-8"))
            self.assertEqual(set(parsed["tools"].keys()), {"just"})

    def test_sync_staged_removes_gitignored_unselected_harnesses(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repo = root / "repo"
            staged = root / "staged"
            repo.mkdir()
            staged.mkdir()
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            (repo / ".gitignore").write_text("/CLAUDE.md\n/QWEN.md\n/.codex/\n", encoding="utf-8")
            (repo / "README.md").write_text("hi\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(repo), "add", ".gitignore", "README.md"], check=True)
            (repo / "CLAUDE.md").write_text("claude\n", encoding="utf-8")
            (repo / "QWEN.md").write_text("qwen\n", encoding="utf-8")
            (repo / ".codex/agents").mkdir(parents=True)
            (repo / ".codex/config.toml").write_text("x = 1\n", encoding="utf-8")
            (repo / ".codex/agents/worker.toml").write_text("x = 1\n", encoding="utf-8")

            (staged / ".gitignore").write_text("", encoding="utf-8")
            (staged / "README.md").write_text("hi\n", encoding="utf-8")
            (staged / "CLAUDE.md").write_text("claude\n", encoding="utf-8")
            template_init._sync_staged_to_repo(staged, repo)

            self.assertTrue((repo / "CLAUDE.md").is_file())
            self.assertFalse((repo / "QWEN.md").exists())
            self.assertFalse((repo / ".codex").exists())


class RuntimeProgressTests(unittest.TestCase):
    def test_cleanup_failure_returns_nonzero_exit_code(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp) / "measurements"
            fake_bin = Path(tmp) / "bin"
            fake_bin.mkdir()
            for name in ("specimen", "driver"):
                (fake_bin / name).write_text("x", encoding="utf-8")
            manifest_abs = str((ROOT / "crates/service/Cargo.toml").resolve())
            build_stdout = (
                f'{{"reason":"compiler-artifact","manifest_path":"{manifest_abs}","executable":"{fake_bin / "specimen"}","target":{{"name":"runtime_progress","kind":["example"]}}}}\n'
                f'{{"reason":"compiler-artifact","manifest_path":"{manifest_abs}","executable":"{fake_bin / "driver"}","target":{{"name":"runtime_progress","kind":["test"]}}}}\n'
            )

            def fake_run(cmd: Any, **_kw: Any) -> subprocess.CompletedProcess[str]:
                joined = " ".join(str(c) for c in cmd)
                if "git diff" in joined:
                    return subprocess.CompletedProcess(cmd, 0, "", "")
                if "docker info" in joined:
                    return subprocess.CompletedProcess(cmd, 0, "2\n", "")
                if "cargo build" in joined:
                    return subprocess.CompletedProcess(cmd, 0, build_stdout, "")
                if "docker image inspect --format" in joined:
                    return subprocess.CompletedProcess(cmd, 0, "sha256:abc\n", "")
                if "--list" in joined:
                    return subprocess.CompletedProcess(
                        cmd, 0, "bounded_sources_preserve_process_progress_under_one_cpu_quota: test\n", ""
                    )
                if "docker inspect" in joined:
                    return subprocess.CompletedProcess(cmd, 1, "", "")
                if "docker image rm" in joined:
                    return subprocess.CompletedProcess(cmd, 1, "", "rm failed")
                return subprocess.CompletedProcess(cmd, 0, "ok\n", "")

            with (
                mock.patch("platform.system", return_value="Linux"),
                mock.patch("shutil.which", return_value="/usr/bin/fake"),
                mock.patch.object(runtime_progress.lib, "run", side_effect=fake_run),
                mock.patch.object(runtime_progress.lib, "git", side_effect=lambda *a, **kw: b"tar" if kw.get("binary") else ("a" * 40 if a[0] == "rev-parse" else "")),
                mock.patch("subprocess.run", return_value=subprocess.CompletedProcess([], 0, "pass\n", "")),
                mock.patch.dict(os.environ, {"RUNTIME_PROGRESS_RESULTS": str(results)}, clear=False),
            ):
                rc = runtime_progress.run_proof()
            self.assertEqual(rc, 1)
            self.assertEqual(Path(f"{results}.inputs/runner-exit-code").read_text(encoding="utf-8").strip(), "1")


class HarnessWorkspaceCratesTests(unittest.TestCase):
    def test_workspace_crates_reference_matches_cargo_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            ref = repo / ".agents/skills/rust-dependencies/references/workspace-crates.md"
            ref.parent.mkdir(parents=True)
            (repo / "Cargo.toml").write_text(
                '[workspace.dependencies]\nmediatype = { version = "0.23.0", default-features = false }\n',
                encoding="utf-8",
            )
            ref.write_text(
                "# Workspace utility crates\n\n"
                "- **MIME parsing** — `mediatype`: guard empty unquoted parameters.\n"
                "- **RAII cleanup** — `std::mem::DropGuard`: Rust 1.100 std.\n",
                encoding="utf-8",
            )
            self.assertEqual(harness.check_workspace_crates_reference(repo), [])

            ref.write_text("# Workspace utility crates\n\n- **Retired crate** — `mime`: removed.\n", encoding="utf-8")
            stale = harness.check_workspace_crates_reference(repo)
            self.assertTrue(any("crate `mime` is not in" in item for item in stale), stale)

            ref.write_text("# Workspace utility crates\n\nNo bullets here.\n", encoding="utf-8")
            self.assertTrue(any("contains no crate entries" in item for item in harness.check_workspace_crates_reference(repo)))


if __name__ == "__main__":
    unittest.main()
