"""Unit tests for template initializer, harness sync, CI change detection, and image publication."""

from __future__ import annotations

import importlib.util
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

from template import check as template_check, harness, init as template_init, runtime_progress

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
        self.assertFalse(res["repo_lint"])
        self.assertFalse(res["profiling"])
        self.assertFalse(res["vendor_transport"])

        lock_only = DETECT.classify(ROOT, ["Cargo.lock"])
        self.assertTrue(lock_only["deps"])
        self.assertFalse(lock_only["profiling"])
        self.assertFalse(lock_only["vendor_transport"])

        hotpath_res = DETECT.classify(ROOT, ["vendor/hotpath/src/lib.rs"])
        self.assertTrue(hotpath_res["profiling"])
        self.assertTrue(hotpath_res["vendor_transport"])

        md_in_crate = DETECT.classify(ROOT, ["crates/service/README.md"])
        self.assertTrue(md_in_crate["repo_lint"])

        cache_only = DETECT.classify(ROOT, ["crates/infra-cache/src/namespace.rs"])
        self.assertTrue(cache_only["integration"])
        self.assertTrue(cache_only["integration_cache"])
        self.assertFalse(cache_only["integration_db"])
        self.assertFalse(cache_only["sqlx_check"])
        self.assertFalse(cache_only["integration_object_storage"])
        self.assertFalse(cache_only["integration_messaging"])
        self.assertFalse(cache_only["integration_temporal"])

        s3_only = DETECT.classify(ROOT, ["crates/infra-object-storage/src/download.rs"])
        self.assertTrue(s3_only["integration"])
        self.assertTrue(s3_only["integration_object_storage"])
        self.assertFalse(s3_only["integration_db"])
        self.assertFalse(s3_only["sqlx_check"])
        self.assertFalse(s3_only["integration_cache"])
        self.assertFalse(s3_only["integration_messaging"])
        self.assertFalse(s3_only["integration_temporal"])

        aws_runtime_only = DETECT.classify(
            ROOT, ["vendor/aws-runtime/src/content_encoding/body/http_body_1_x.rs"]
        )
        self.assertTrue(aws_runtime_only["rust"])
        self.assertTrue(aws_runtime_only["deps"])
        self.assertTrue(aws_runtime_only["vendor_transport"])
        self.assertTrue(aws_runtime_only["integration"])
        self.assertTrue(aws_runtime_only["integration_object_storage"])
        self.assertFalse(aws_runtime_only["integration_db"])
        self.assertFalse(aws_runtime_only["integration_cache"])
        self.assertFalse(aws_runtime_only["integration_messaging"])
        self.assertFalse(aws_runtime_only["integration_temporal"])

        pg_only = DETECT.classify(ROOT, ["crates/infra-postgres/src/pool.rs"])
        self.assertTrue(pg_only["integration"])
        self.assertTrue(pg_only["integration_db"])
        self.assertTrue(pg_only["sqlx_check"])
        self.assertFalse(pg_only["integration_cache"])
        self.assertFalse(pg_only["integration_object_storage"])
        self.assertFalse(pg_only["integration_messaging"])
        self.assertFalse(pg_only["integration_temporal"])

        for temporal_path in (
            "crates/infra-temporal/src/client.rs",
            "crates/temporal-worker/src/main.rs",
            "test/tests/temporal.rs",
        ):
            temporal_only = DETECT.classify(ROOT, [temporal_path])
            self.assertTrue(temporal_only["rust"], temporal_path)
            self.assertTrue(temporal_only["integration"], temporal_path)
            self.assertTrue(temporal_only["integration_temporal"], temporal_path)
            self.assertFalse(temporal_only["integration_db"], temporal_path)
            self.assertFalse(temporal_only["sqlx_check"], temporal_path)
            self.assertFalse(temporal_only["integration_cache"], temporal_path)
            self.assertFalse(temporal_only["integration_object_storage"], temporal_path)
            self.assertFalse(temporal_only["integration_messaging"], temporal_path)

        temporal_fixture = DETECT.classify(ROOT, ["test/src/bin/temporal_worker_fixture.rs"])
        self.assertTrue(temporal_fixture["rust"])
        self.assertTrue(temporal_fixture["integration"])
        self.assertTrue(temporal_fixture["integration_temporal"])
        self.assertFalse(temporal_fixture["sqlx_check"])
        self.assertFalse(temporal_fixture["integration_cache"])
        self.assertFalse(temporal_fixture["integration_object_storage"])
        self.assertFalse(temporal_fixture["integration_messaging"])

        util_test = DETECT.classify(ROOT, ["crates/utility-recipes/tests/utility_values.rs"])
        self.assertTrue(util_test["rust"])
        self.assertFalse(util_test["integration"])
        self.assertFalse(util_test["integration_db"])
        self.assertFalse(util_test["integration_temporal"])

        with tempfile.TemporaryDirectory() as tmp:
            empty = Path(tmp)
            all_empty = DETECT.classify(empty, None)
            self.assertFalse(all_empty["grpc"])
            self.assertFalse(all_empty["migrations"])
            self.assertFalse(all_empty["integration_temporal"])
            self.assertFalse(all_empty["template"])
            self.assertTrue(all_empty["rust"])

            (empty / "env").mkdir()
            (empty / "env/docker-compose.yml").write_text("services: {}\n", encoding="utf-8")
            (empty / "crates/infra-oauth2-client-credentials").mkdir(parents=True)
            oauth_only = DETECT.classify(empty, None)
            self.assertTrue(oauth_only["oauth"])
            self.assertFalse(oauth_only["integration"])
            self.assertFalse(oauth_only["integration_temporal"])

            (empty / "crates/infra-temporal").mkdir(parents=True)
            with_temporal = DETECT.classify(empty, None)
            self.assertTrue(with_temporal["integration"])
            self.assertTrue(with_temporal["integration_temporal"])

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
        with self.assertRaises(template_init.InitError):
            template_init.validate_inputs(template_init.InitInputs(**{**base, "temporal": "invalid"}))
        self.assertEqual(
            template_init.validate_inputs(template_init.InitInputs(**{**base, "temporal": "sdk"})).temporal,
            "sdk",
        )
        resolved = template_init.validate_inputs(
            template_init.InitInputs(**{**base, "outbound_auth": "oauth2-client-credentials"})
        )
        self.assertEqual(resolved.outbound_http, "bounded")

    def test_temporal_profile_projection_isolation_and_monitoring_rules(self) -> None:
        promtool = shutil.which("promtool")
        temporal_removed_paths = (
            "crates/infra-temporal",
            "crates/temporal-worker",
            "crates/config/src/temporal.rs",
            "crates/config/src/temporal",
            "crates/config/src/temporal_worker.rs",
            "crates/config/src/temporal_worker",
            "crates/infra-http/src/temporal.rs",
            "crates/service/src/bootstrap/temporal.rs",
            "test/src/bin/temporal_worker_fixture.rs",
            "test/tests/temporal.rs",
            "docs/decisions/temporal-decisions.md",
        )

        def check_promtool(staged: Path, profile: str) -> None:
            if promtool is None:
                return
            rules = sorted(staged.glob("env/monitoring/*.rules.yml"))
            tests = sorted(staged.glob("env/monitoring/tests/*-rules.yml"))
            for cmd in (
                [promtool, "check", "rules", *(str(p) for p in rules)],
                [promtool, "test", "rules", *(str(p) for p in tests)],
            ):
                res = subprocess.run(cmd, cwd=staged, capture_output=True, text=True, check=False)
                self.assertEqual(res.returncode, 0, f"{profile}: {cmd}\n{res.stdout}\n{res.stderr}")

        with tempfile.TemporaryDirectory(prefix="proj-minimal-temporal-") as tmp:
            staged = Path(tmp) / "repo"
            template_init.project_tree(
                ROOT, staged, template_check._inputs_for("minimal"), run_cargo=False, from_git_head=False
            )
            for rel in (*temporal_removed_paths, "crates/json-payload", "vendor/tonic"):
                self.assertFalse((staged / rel).exists(), f"minimal retained {rel}")
            for rel in (
                "Cargo.toml",
                "deny.toml",
                "build/docker/Dockerfile",
                "build/docker/rust-notices.toml",
                ".dockerignore",
                "env/config/local.toml",
                "env/monitoring/service.rules.yml",
                "env/monitoring/tests/service-rules.yml",
                "README.md",
                "docs/alerting.md",
                "docs/production-contract.md",
            ):
                self.assertNotIn("temporal", (staged / rel).read_text(encoding="utf-8").lower(), f"minimal leaked temporal into {rel}")
            check_promtool(staged, "minimal")

        with tempfile.TemporaryDirectory(prefix="proj-temporal-only-") as tmp:
            staged = Path(tmp) / "repo"
            template_init.project_tree(
                ROOT, staged, template_check._inputs_for("temporal"), run_cargo=False, from_git_head=False
            )
            for rel in (*temporal_removed_paths, "crates/json-payload", "vendor/tonic"):
                self.assertTrue((staged / rel).exists(), f"temporal missing {rel}")
            for rel in ("crates/jobs-worker", "crates/infra-jobs", "crates/infra-grpc", "test/src/bin/jobs_worker_fixture.rs"):
                self.assertFalse((staged / rel).exists(), f"temporal unexpectedly retained {rel}")
            http_rs = (staged / "crates/config/src/http.rs").read_text(encoding="utf-8")
            self.assertIn("pub struct WorkerHttpConfig", http_rs)
            self.assertIn("pub(crate) type TemporalWorkerHttpInput", http_rs)
            self.assertNotIn("pub(crate) type WorkerHttpInput", http_rs)
            self.assertIn("pub use http::WorkerHttpConfig;", (staged / "crates/config/src/lib.rs").read_text(encoding="utf-8"))
            self.assertIn("!vendor/tonic/", (staged / ".dockerignore").read_text(encoding="utf-8"))
            dockerfile = (staged / "build/docker/Dockerfile").read_text(encoding="utf-8")
            self.assertIn("COPY --from=planner /src/vendor/tonic /src/vendor/tonic", dockerfile)
            self.assertIn("temporal-worker-build", dockerfile)
            self.assertNotIn("jobs-worker-build", dockerfile)
            deny_text = (staged / "deny.toml").read_text(encoding="utf-8")
            self.assertIn('"0BSD"', deny_text)
            self.assertIn('"temporalio-common-wasm"', deny_text)
            self.assertNotIn("lru@0.18", deny_text)
            check_promtool(staged, "temporal")

        for profile in ("postgres-full", "grpc-service-auth", "full"):
            with tempfile.TemporaryDirectory(prefix=f"proj-{profile}-mon-") as tmp:
                staged = Path(tmp) / "repo"
                template_init.project_tree(
                    ROOT, staged, template_check._inputs_for(profile), run_cargo=False, from_git_head=False
                )
                rules_text = (staged / "env/monitoring/service.rules.yml").read_text(encoding="utf-8")
                fixtures_text = (staged / "env/monitoring/tests/service-rules.yml").read_text(encoding="utf-8")
                if profile == "full":
                    self.assertIn("service-temporal", rules_text)
                    self.assertIn("lru@0.18", (staged / "deny.toml").read_text(encoding="utf-8"))
                else:
                    self.assertNotIn("temporal", rules_text.lower(), f"{profile} leaked temporal into service.rules.yml")
                    self.assertNotIn("temporal", fixtures_text.lower(), f"{profile} leaked temporal into service-rules.yml")
                check_promtool(staged, profile)

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

    def test_ci_template_matrix_shards_cover_every_canonical_profile_once(self) -> None:
        ci_text = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        section = ci_text.split("template-matrix:", 1)[1].split("  # template:end source-template:ci-template-job", 1)[0]
        profiles: list[str] = []
        for match in re.finditer(r"^\s+profiles:\s+(.+)$", section, re.M):
            profiles.extend(match.group(1).split())
        self.assertEqual(len(profiles), len(set(profiles)), f"duplicate profiles in template-matrix shards: {profiles}")
        self.assertEqual(set(profiles), set(template_check.CANONICAL_PROFILES))

    def test_live_workspace_rust_selection_backtest(self) -> None:
        from scripts.checks import rust_selection

        cache_plan = rust_selection.plan_selection(ROOT, ["crates/infra-cache/src/namespace.rs"])
        self.assertEqual(cache_plan.mode, "selected")
        self.assertEqual(cache_plan.packages, ("infra-cache", "service"))
        self.assertEqual(cache_plan.lint_features, ("infra-cache/integration",))

        openapi_plan = rust_selection.plan_selection(ROOT, ["api/openapi/service.yaml"])
        self.assertEqual(openapi_plan.mode, "selected")
        self.assertEqual(openapi_plan.packages, ("service",))

        obs_plan = rust_selection.plan_selection(ROOT, ["crates/infra-telemetry/src/logging.rs"])
        self.assertEqual(obs_plan.mode, "selected")
        self.assertIn("infra-telemetry", obs_plan.packages)
        self.assertIn("service", obs_plan.packages)
        self.assertNotIn("infra-cache", obs_plan.packages)


if __name__ == "__main__":
    unittest.main()
