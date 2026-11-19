#!/usr/bin/env python3
"""Unit tests for scripts/verify/{changed_paths,surfaces,affected_crates,runner}.py."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
VERIFY_DIR = ROOT / "scripts/verify"
for _dir in (VERIFY_DIR, ROOT / "scripts/template"):
    if str(_dir) not in sys.path:
        sys.path.insert(0, str(_dir))

if shutil.which("cargo") is None and (Path.home() / ".cargo/bin/cargo").is_file():
    os.environ["PATH"] = f"{Path.home() / '.cargo/bin'}:{os.environ.get('PATH', '')}"

import affected_crates
import changed_paths
import runner
from state import TEMPLATE_REPOSITORY
import surfaces


def _git(repo: Path, *args: str) -> str:
    res = subprocess.run(
        [
            "git",
            "-C",
            os.fspath(repo),
            "-c",
            "user.name=verify-test",
            "-c",
            "user.email=verify-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            *args,
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    return res.stdout.strip()


def _clean_verify_env(**extra: str) -> dict[str, str]:
    excluded = {
        "CI",
        "ALLOW_FULL",
        "ALLOW_HEAVY",
        "REPOSITORY_CI",
        "VERIFY_RUNTIME_IMAGE",
        "VERIFY_FORCE",
        "BASE_REF",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_BUILD_BUILD_DIR",
    }
    base = {
        key: value
        for key, value in os.environ.items()
        if key not in excluded and not key.startswith("VALIDATION_LOCK_")
    }
    base["PYTHONDONTWRITEBYTECODE"] = "1"
    return {**base, **extra}


class ChangedPathsTests(unittest.TestCase):
    def test_base_union_files_and_unavailable_ref(self) -> None:
        with tempfile.TemporaryDirectory(prefix="changed-paths-test-") as tmp_str:
            repo = Path(tmp_str) / "repo"
            repo.mkdir()
            _git(repo, "init", "-q")

            old_rs = repo / "crates/old/src/lib.rs"
            old_rs.parent.mkdir(parents=True)
            old_rs.write_text("pub fn old() {}\n", encoding="utf-8")
            _git(repo, "add", "crates/old/src/lib.rs")
            _git(repo, "commit", "-qm", "start")

            (repo / "crates/new/src").mkdir(parents=True)
            _git(repo, "mv", "crates/old/src/lib.rs", "crates/new/src/lib.rs")
            _git(repo, "commit", "-qm", "move")

            (repo / "notes.md").write_text("untracked\n", encoding="utf-8")
            base_only = changed_paths.changed_paths(repo, base="HEAD~1")
            self.assertEqual(base_only, ["crates/new/src/lib.rs", "crates/old/src/lib.rs"])

            union_prev = changed_paths.changed_paths(repo, union="HEAD~1")
            self.assertIn("notes.md", union_prev)
            self.assertIn("crates/new/src/lib.rs", union_prev)

            special_name = "copy with space\\.rs"
            shutil.copy2(repo / "crates/new/src/lib.rs", repo / special_name)
            union_head = changed_paths.changed_paths(repo, union="HEAD")
            self.assertEqual(union_head, [special_name, "notes.md"])

            _git(repo, "add", ".")
            _git(repo, "commit", "-qm", "copy")
            cli_base = subprocess.run(
                [sys.executable, str(VERIFY_DIR / "changed_paths.py"), "--repo", str(repo), "--base", "HEAD~1"],
                capture_output=True,
                text=True,
            )
            self.assertEqual(cli_base.returncode, 0, cli_base.stderr)
            self.assertEqual(cli_base.stdout.splitlines(), [special_name, "notes.md"])

            (repo / "notes.md").write_text("staged\n", encoding="utf-8")
            _git(repo, "add", "notes.md")
            (repo / special_name).write_text("unstaged\n", encoding="utf-8")
            self.assertEqual(changed_paths.changed_paths(repo, union="HEAD"), [special_name, "notes.md"])

            self.assertEqual(
                changed_paths.changed_paths(repo, files=["b.rs", "a.rs", "b.rs", ""]),
                ["a.rs", "b.rs"],
            )

            bad_ref = subprocess.run(
                [sys.executable, str(VERIFY_DIR / "changed_paths.py"), "--repo", str(repo), "--base", "missing-ref"],
                capture_output=True,
                text=True,
            )
            self.assertEqual(bad_ref.returncode, 2)
            self.assertIn("verification base is unavailable: missing-ref", bad_ref.stderr)


class SurfacesTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="surfaces-test-")))
        self.source_fixture = self.temp / "source"
        self.derived_fixture = self.temp / "derived"
        self.source_fixture.mkdir()
        self.derived_fixture.mkdir()
        (self.source_fixture / "make").mkdir()
        (self.source_fixture / "make/source.mk").write_text("", encoding="utf-8")

        head_rev = _git(ROOT, "rev-parse", "HEAD") if (ROOT / ".git").exists() else "0" * 40
        if len(head_rev) not in (40, 64):
            head_rev = "0" * 40
        self.head_rev = head_rev
        self._write_lock(self.derived_fixture, {"database": "none", "agent_harness": "core"})

    def _write_lock(self, root: Path, profiles: dict[str, str]) -> None:
        payload = {
            "schema_version": 1,
            "state": "complete",
            "identity": {
                "service_name": "fixture-api",
                "repository": "https://github.com/example/fixture-api",
                "description": "Fixture API",
                "codeowner": "@example/platform",
            },
            "profiles": profiles,
            "source": {
                "repository": TEMPLATE_REPOSITORY,
                "checkout_revision": self.head_rev,
                "provenance": "local-checkout",
            },
        }
        (root / "template.lock").write_text(json.dumps(payload) + "\n", encoding="utf-8")

    def _assert_surfaces(
        self,
        repo: Path,
        file: str,
        true_names: tuple[str, ...],
        false_names: tuple[str, ...] = (),
        *,
        post_merge: bool = False,
    ) -> None:
        verdicts = surfaces.classify_files(repo, [file], post_merge=post_merge)
        for name in true_names:
            self.assertTrue(verdicts.get(name), f"{file}: expected {name}=True, got {verdicts}")
        for name in false_names:
            self.assertFalse(verdicts.get(name), f"{file}: expected {name}=False, got {verdicts}")

    def test_batched_service_hook_runs_once(self) -> None:
        hook_dir = self.source_fixture / "scripts/service"
        hook_dir.mkdir(parents=True)
        counter = self.temp / "hook-invocations.txt"
        (hook_dir / "changed-surfaces.sh").write_text(
            f'printf "x\\n" >>"{counter}"\n'
            "service_changed_surfaces() {\n"
            '  case "$1" in\n'
            "    assets/data.bin) mark rust_source runtime_image ;;\n"
            "    assets/extra.cfg) mark documentation ;;\n"
            "  esac\n"
            "}\n",
            encoding="utf-8",
        )
        verdicts = surfaces.classify_files(
            self.source_fixture,
            ["assets/data.bin", "assets/extra.cfg"],
        )
        self.assertTrue(verdicts["rust_source"])
        self.assertTrue(verdicts["runtime_image"])
        self.assertTrue(verdicts["documentation"])
        self.assertFalse(verdicts["db_integration"])
        self.assertEqual(len(counter.read_text(encoding="utf-8").splitlines()), 1)

    def test_quality_tooling_vs_rust_duplication_split(self) -> None:
        for rs_file in ("crates/config/build.rs", "crates/health/src/lib.rs", "test/tests/probes.rs"):
            self._assert_surfaces(
                self.source_fixture,
                rs_file,
                ("rust_source", "duplication", "architecture"),
                ("quality_tooling", "template_engine"),
            )
            self._assert_surfaces(
                self.source_fixture,
                rs_file,
                ("rust_source", "duplication", "architecture", "quality_tooling"),
                (),
                post_merge=True,
            )

        for q_file in (
            ".jscpd.json",
            "quality/architecture.json",
            "scripts/checks/architecture.py",
            "scripts/checks/npx.py",
            "scripts/tests/test_quality.py",
        ):
            self._assert_surfaces(
                self.source_fixture,
                q_file,
                ("duplication", "architecture", "quality_tooling"),
                ("rust_source",),
            )

        self._assert_surfaces(
            self.source_fixture,
            "docs/example.md",
            ("documentation",),
            ("duplication", "architecture", "quality_tooling"),
        )
        self._assert_surfaces(
            self.source_fixture,
            "tools/grpc-codegen/src/main.rs",
            ("rust_source",),
            ("duplication", "architecture", "quality_tooling"),
        )

    def test_new_architecture_paths_and_profiles(self) -> None:
        # Verify scripts/image/cargo_artifact.py marks openapi, runtime_image, validation_system (+ runtime_progress when retained)
        self._assert_surfaces(
            self.source_fixture,
            "scripts/image/cargo_artifact.py",
            ("openapi", "runtime_image", "validation_system", "python", "cargo_dependencies", "module_initializer", "initializer_artifacts"),
            ("rust_source", "documentation"),
        )
        self._assert_surfaces(
            self.source_fixture,
            "scripts/checks/openapi_idempotency.py",
            ("openapi", "validation_system", "python"),
            ("rust_source",),
        )
        # template:begin runtime-progress:test-surfaces-runtime-progress
        for rp_file in (
            "crates/health/src/lib.rs",
            "crates/service/examples/runtime_progress.rs",
            "crates/service/tests/runtime_progress.rs",
            "scripts/profiles/runtime_progress.sh",
            "scripts/image/cargo_artifact.py",
        ):
            self._assert_surfaces(self.source_fixture, rp_file, ("runtime_progress",))
        self._assert_surfaces(
            self.source_fixture,
            "build/docker/runtime-progress.Dockerfile",
            ("runtime_progress", "module_initializer"),
            ("runtime_image", "initializer_source", "initializer_runtime", "initializer_artifacts"),
        )
        self._assert_surfaces(
            self.source_fixture,
            "crates/health/tests/probes.rs",
            ("rust_source",),
            ("runtime_progress",),
        )
        # template:end runtime-progress:test-surfaces-runtime-progress
        for img_file in ("build/docker/Dockerfile", "build/docker/pgo-train.sh"):
            self._assert_surfaces(
                self.source_fixture,
                img_file,
                ("runtime_image", "module_initializer", "initializer_artifacts"),
            )
        for img_input in ("deny.toml", "tools/versions.env", "LICENSE"):
            self._assert_surfaces(self.source_fixture, img_input, ("runtime_image", "initializer_artifacts"))

        # template:begin grpc:test-surfaces-grpc
        for grpc_file in (
            "api/proto/example/v1/echo.proto",
            "crates/grpc-contracts/src/generated/example.v1.rs",
            "tools/grpc-codegen/Cargo.toml",
            "buf.yaml",
            "scripts/profiles/grpc_generate.sh",
            "scripts/profiles/grpc_check.sh",
        ):
            self._assert_surfaces(
                self.source_fixture,
                grpc_file,
                ("grpc_schema", "module_initializer", "initializer_runtime"),
                ("openapi", "runtime_image"),
            )
        self._assert_surfaces(
            self.source_fixture,
            "tools/grpc-codegen/deny.toml",
            ("grpc_schema", "dependency_policy"),
            ("cargo_dependencies", "rust_source"),
        )
        # template:end grpc:test-surfaces-grpc

        # template:begin service-auth:test-surfaces-service-auth
        self._assert_surfaces(
            self.source_fixture,
            "crates/infra-serviceauth/src/verify.rs",
            ("rust_source", "module_initializer", "initializer_runtime"),
            ("cargo_dependencies", "documentation", "db_integration"),
        )
        # template:end service-auth:test-surfaces-service-auth

        for verify_py in ("lock", "changed_paths", "surfaces", "affected_crates", "runner"):
            self._assert_surfaces(
                self.source_fixture,
                f"scripts/verify/{verify_py}.py",
                ("python", "validation_system", "module_initializer"),
                ("shell", "rust_source", "initializer_runtime"),
            )

        self._assert_surfaces(
            self.source_fixture,
            "scripts/checks/tools.py",
            ("python", "tool_manifest", "validation_system", "module_initializer"),
            ("shell", "runtime_image"),
        )
        for railway_manifest in (".railway/package.json", ".railway/package-lock.json", ".railway/.npmrc"):
            self._assert_surfaces(
                self.source_fixture,
                railway_manifest,
                ("tool_manifest", "documentation", "module_initializer"),
                ("cargo_dependencies", "rust_source", "initializer_runtime"),
            )
        self._assert_surfaces(
            self.source_fixture,
            ".railway/railway.ts",
            ("documentation", "module_initializer"),
            ("tool_manifest", "cargo_dependencies", "rust_source", "initializer_runtime"),
        )
        self._assert_surfaces(
            self.derived_fixture,
            "migrations/.gitkeep",
            ("no_validation_required",),
            ("migrations", "db_integration", "runtime_image"),
        )
        for img_inputs in ("scripts/checks/image_inputs.py", "scripts/tests/test_image_inputs.py"):
            self._assert_surfaces(
                self.source_fixture,
                img_inputs,
                ("python", "documentation", "validation_system", "module_initializer"),
                ("runtime_image", "initializer_runtime"),
            )
        self._assert_surfaces(
            self.source_fixture,
            "scripts/image/inventory.py",
            ("python", "runtime_image", "validation_system", "module_initializer", "initializer_artifacts"),
            ("initializer_runtime", "cargo_dependencies"),
        )
        self._assert_surfaces(
            self.source_fixture,
            "scripts/image/scan.sh",
            ("shell", "runtime_image", "validation_system", "module_initializer", "initializer_artifacts"),
            ("initializer_runtime",),
        )
        for img_sh in ("scripts/image/build.sh", "scripts/image/check.sh"):
            self._assert_surfaces(
                self.source_fixture,
                img_sh,
                ("shell", "runtime_image", "module_initializer"),
                ("tool_manifest", "validation_system"),
            )
        self._assert_surfaces(
            self.source_fixture,
            "scripts/image/publish_metadata.py",
            ("python", "publication_metadata", "validation_system", "module_initializer"),
            ("github_workflows", "runtime_image"),
        )
        self._assert_surfaces(
            self.source_fixture,
            "scripts/harness/sync.py",
            ("python", "agent_instructions", "module_initializer"),
            ("shell", "validation_system", "initializer_runtime"),
        )
        self._assert_surfaces(
            self.source_fixture,
            "scripts/template/sync.py",
            ("agent_instructions", "validation_system", "module_initializer", "initializer_source"),
            ("rust_source", "template_engine", "initializer_runtime", "initializer_artifacts"),
        )
        self._assert_surfaces(
            self.derived_fixture,
            "scripts/template/sync.py",
            ("agent_instructions", "validation_system"),
            ("module_initializer", "initializer_source", "initializer_runtime", "initializer_artifacts"),
        )
        for tmpl_file in (
            "scripts/template/state.py",
            "scripts/template/init.py",
            "scripts/template/profiles.json",
        ):
            self._assert_surfaces(
                self.source_fixture,
                tmpl_file,
                ("agent_instructions", "validation_system", "template_engine", "module_initializer", "initializer_source", "initializer_runtime", "initializer_artifacts"),
                ("rust_source",),
            )
            self._assert_surfaces(
                self.derived_fixture,
                tmpl_file,
                ("agent_instructions", "validation_system", "template_engine"),
                ("module_initializer", "initializer_source", "initializer_runtime", "initializer_artifacts"),
            )

        for action_file in (
            ".github/actions/load-versions/action.yml",
            ".github/actions/resolve-changes/action.yml",
        ):
            self._assert_surfaces(
                self.source_fixture,
                action_file,
                ("github_workflows", "duplication", "architecture", "monitoring_rules", "module_initializer", "initializer_runtime"),
                ("dependency_automation",),
            )

        for mon_file in (
            "env/monitoring/service.rules.yml",
            "env/monitoring/tests/service-rules.yml",
            "env/monitoring/tests/postgres-maintenance-rules.yml",
            "scripts/checks/monitoring_rules.sh",
        ):
            self._assert_surfaces(
                self.source_fixture,
                mon_file,
                ("monitoring_rules", "module_initializer"),
                ("db_integration", "rust_source", "initializer_runtime"),
            )

        self._assert_surfaces(
            self.source_fixture,
            "scripts/checks/dependency_drift.sh",
            ("dependency_policy", "tool_manifest", "shell", "module_initializer", "initializer_runtime"),
            ("cargo_dependencies", "rust_source"),
        )
        self._assert_surfaces(
            self.source_fixture,
            "scripts/checks/native_transport.py",
            ("python", "rust_source", "cargo_dependencies", "runtime_image", "module_initializer", "initializer_runtime"),
            ("db_integration", "migrations"),
        )
        self._assert_surfaces(
            self.source_fixture,
            "scripts/profiles/migration_validate.sh",
            ("migrations", "runtime_image", "shell"),
            ("db_integration",),
        )
        self._assert_surfaces(
            self.source_fixture,
            "scripts/profiles/migration_history.sh",
            ("migrations", "runtime_image", "shell"),
            ("db_integration", "validation_system"),
        )
        self._assert_surfaces(
            self.source_fixture,
            "scripts/tests/test_migration_history.py",
            ("migrations", "python", "module_initializer"),
            ("db_integration", "runtime_image"),
        )
        self._assert_surfaces(
            self.source_fixture,
            "scripts/profiles/sqlx_prepare.sh",
            ("db_integration", "shell", "module_initializer", "initializer_runtime"),
            ("migrations", "validation_system"),
        )

        # P9 mounted fixture check
        p9 = self.source_fixture / "test/tests/http_idempotency/mounted.rs"
        p9.parent.mkdir(parents=True)
        p9.write_text("", encoding="utf-8")
        self._assert_surfaces(self.source_fixture, "crates/infra-http/src/authn.rs", ("db_integration",))
        shutil.rmtree(self.source_fixture / "test")
        self._assert_surfaces(self.source_fixture, "crates/infra-http/src/authn.rs", ("rust_source",), ("db_integration",))

        # Derived capability profiles without PostgreSQL
        self._write_lock(
            self.derived_fixture,
            {"database": "none", "messaging": "nats-jetstream", "agent_harness": "core"},
        )
        self._assert_surfaces(
            self.derived_fixture,
            "crates/infra-messaging/src/wire.rs",
            ("rust_source", "messaging_integration"),
            ("db_integration", "migrations", "module_initializer"),
        )
        self._assert_surfaces(
            self.derived_fixture,
            "scripts/profiles/integration.sh",
            ("shell", "messaging_integration", "validation_system"),
            ("db_integration", "cache_integration"),
        )

        self._write_lock(
            self.derived_fixture,
            {"database": "none", "cache": "redis", "agent_harness": "core"},
        )
        self._assert_surfaces(
            self.derived_fixture,
            "crates/infra-cache/src/lib.rs",
            ("rust_source", "cache_integration"),
            ("db_integration", "messaging_integration"),
        )

        self._write_lock(
            self.derived_fixture,
            {"database": "none", "object_storage": "s3", "agent_harness": "core"},
        )
        self._assert_surfaces(
            self.derived_fixture,
            "crates/infra-object-storage/src/lib.rs",
            ("rust_source", "object_storage_integration"),
            ("db_integration", "cache_integration"),
        )

        self._write_lock(
            self.derived_fixture,
            {
                "database": "none",
                "outbound_http": "bounded",
                "outbound_auth": "oauth2-client-credentials",
                "agent_harness": "core",
            },
        )
        self._assert_surfaces(
            self.derived_fixture,
            "crates/infra-oauth2-client-credentials/src/lib.rs",
            ("rust_source", "oauth_integration"),
            ("db_integration", "object_storage_integration"),
        )

    def test_all_surfaces_github_output_fail_closed_and_union(self) -> None:
        for cap_dir in (
            "crates/infra-messaging",
            "crates/infra-cache",
            "crates/infra-object-storage",
            "crates/infra-oauth2-client-credentials",
        ):
            (self.source_fixture / cap_dir).mkdir(parents=True, exist_ok=True)
        all_source = surfaces.all_surfaces_report(self.source_fixture)
        for name in surfaces.SURFACE_NAMES:
            self.assertTrue(all_source.surfaces[name], name)

        all_derived = surfaces.all_surfaces_report(self.derived_fixture)
        for cleared in (
            "db_integration",
            "messaging_integration",
            "cache_integration",
            "object_storage_integration",
            "oauth_integration",
            "migrations",
            "template_engine",
            "module_initializer",
            "initializer_source",
            "initializer_runtime",
            "initializer_artifacts",
        ):
            self.assertFalse(all_derived.surfaces[cleared], cleared)
        self.assertTrue(all_derived.surfaces["monitoring_rules"])

        # Corrupt template.lock exits 2
        (self.derived_fixture / "template.lock").write_text("{corrupt\n", encoding="utf-8")
        corrupt = subprocess.run(
            [sys.executable, str(VERIFY_DIR / "surfaces.py"), "--repo", str(self.derived_fixture), "--all"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(corrupt.returncode, 2)

        # Unknown path fails closed (CLI exit 1 + UnclassifiedPathsError in Python)
        with self.assertRaises(surfaces.UnclassifiedPathsError):
            surfaces.classify_files(self.source_fixture, ["unknown/new-owner.xyz"])

        gh_out = self.temp / "github_output.txt"
        unknown_cli = subprocess.run(
            [
                sys.executable,
                str(VERIFY_DIR / "surfaces.py"),
                "--repo",
                str(self.source_fixture),
                "--files",
                "unknown/new-owner.xyz",
                "--github-output",
                str(gh_out),
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(unknown_cli.returncode, 1)
        self.assertIn("classified=false\n", unknown_cli.stdout)
        self.assertIn("unclassified_files=unknown/new-owner.xyz\n", unknown_cli.stdout)
        self.assertEqual(gh_out.read_text(encoding="utf-8"), unknown_cli.stdout)

        # Every tracked file in ROOT must classify cleanly
        if (ROOT / ".git").exists():
            ls_res = subprocess.run(
                ["git", "-C", str(ROOT), "ls-files"],
                capture_output=True,
                text=True,
                check=True,
            )
            tracked = [
                f
                for f in ls_res.stdout.splitlines()
                if f and ((ROOT / f).exists() or (ROOT / f).is_symlink())
            ]
            if tracked:
                report = surfaces.classify_report(ROOT, tracked)
                self.assertTrue(
                    report.classified,
                    f"unclassified tracked files: {report.unclassified_files}",
                )

        # Union mode: refuses base without classifier, merges base + head when present
        union_repo = self.temp / "union-repo"
        union_repo.mkdir()
        _git(union_repo, "init", "-q")
        (union_repo / "README.md").write_text("# doc\n", encoding="utf-8")
        _git(union_repo, "add", "README.md")
        _git(union_repo, "commit", "-qm", "no-classifier")

        no_base_cls = subprocess.run(
            [
                sys.executable,
                str(VERIFY_DIR / "surfaces.py"),
                "--repo",
                str(union_repo),
                "--union",
                "HEAD",
                "--files",
                "README.md",
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(no_base_cls.returncode, 2)
        self.assertIn("classifier base is unavailable: HEAD", no_base_cls.stderr)

        (union_repo / "scripts/verify").mkdir(parents=True)
        shutil.copy2(VERIFY_DIR / "surfaces.py", union_repo / "scripts/verify/surfaces.py")
        shutil.copy2(VERIFY_DIR / "changed_paths.py", union_repo / "scripts/verify/changed_paths.py")
        (union_repo / "scripts/legacy.sh").write_text("#!/bin/sh\n", encoding="utf-8")
        (union_repo / "make").mkdir()
        (union_repo / "make/source.mk").touch()
        (union_repo / "scripts/template").mkdir()
        sync_path = "scripts/template/sync.py"
        (union_repo / sync_path).touch()
        # template:begin runtime-progress:test-surfaces-quota-union-fixture
        (union_repo / "build/docker").mkdir(parents=True)
        quota_path = "build/docker/runtime-progress.Dockerfile"
        (union_repo / quota_path).touch()
        # template:end runtime-progress:test-surfaces-quota-union-fixture
        _git(union_repo, "add", "-A")
        _git(union_repo, "commit", "-qm", "with-classifier")
        base_sha = _git(union_repo, "rev-parse", "HEAD")

        (union_repo / "scripts/legacy.sh").unlink()
        (union_repo / "README.md").write_text("# updated\n", encoding="utf-8")
        union_report = surfaces.classify_union_report(union_repo, base_sha)
        self.assertTrue(union_report.classified)
        self.assertTrue(union_report.surfaces["shell"])
        self.assertTrue(union_report.surfaces["documentation"])
        sync_union = surfaces.classify_union_report(union_repo, base_sha, [sync_path])
        self.assertTrue(sync_union.surfaces["initializer_source"])
        self.assertTrue(sync_union.surfaces["module_initializer"])
        self.assertFalse(sync_union.surfaces["initializer_runtime"])
        self.assertFalse(sync_union.surfaces["initializer_artifacts"])
        # template:begin runtime-progress:test-surfaces-quota-union
        quota_union = surfaces.classify_union_report(union_repo, base_sha, [quota_path])
        self.assertTrue(quota_union.surfaces["runtime_progress"])
        self.assertTrue(quota_union.surfaces["module_initializer"])
        self.assertFalse(quota_union.surfaces["runtime_image"])
        self.assertFalse(quota_union.surfaces["initializer_artifacts"])
        # template:end runtime-progress:test-surfaces-quota-union

        legacy_classifier = union_repo / "scripts/verify/surfaces.py"
        legacy_classifier.write_text(
            'import sys\nprint("rust_source=true" if "old.rs" in sys.argv else "lint_config=true")\n',
            encoding="utf-8",
        )
        deleted = ("old.rs", "clippy.toml", "rustfmt.toml")
        for path in deleted:
            (union_repo / path).touch()
        _git(union_repo, "add", legacy_classifier.as_posix(), *deleted)
        _git(union_repo, "commit", "-qm", "base classifier predates profiling selection")
        for path in deleted:
            (union_repo / path).unlink()
            report = surfaces.classify_union_report(union_repo, "HEAD", [path])
            self.assertEqual(report.surfaces["profiling"], path != "rustfmt.toml", path)


class AffectedCratesTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls._tmp = tempfile.TemporaryDirectory(prefix="affected-crates-test-")
        cls.fixture = Path(cls._tmp.name)
        (cls.fixture / "Cargo.toml").write_text(
            '[workspace]\nresolver = "2"\nmembers = ["crates/*", "test"]\n',
            encoding="utf-8",
        )
        # 7 workspace members: leaf <- mid <- (dev) top; leaf <- (build) builder; leaf <- extra;
        # leaf <- fixture-config (in crates/cfg); alone; integration-tests (in test)
        # Note: leaf closure hits 5/7 (< 80%? 5*5=25 < 7*4=28), add dep from integration-tests -> leaf
        # so leaf closure hits 6/7 (6*5=30 >= 28 => >=80% workspace fallback!).
        for crate in ("leaf", "mid", "top", "builder", "extra", "cfg", "alone"):
            src = cls.fixture / f"crates/{crate}/src"
            src.mkdir(parents=True)
            (src / "lib.rs").write_text("", encoding="utf-8")
        (cls.fixture / "test/src").mkdir(parents=True)
        (cls.fixture / "test/src/lib.rs").write_text("", encoding="utf-8")

        (cls.fixture / "crates/leaf/Cargo.toml").write_text(
            '[package]\nname = "leaf"\nversion = "0.1.0"\nedition = "2021"\n',
            encoding="utf-8",
        )
        (cls.fixture / "crates/mid/Cargo.toml").write_text(
            '[package]\nname = "mid"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nleaf = { path = "../leaf" }\n',
            encoding="utf-8",
        )
        (cls.fixture / "crates/top/Cargo.toml").write_text(
            '[package]\nname = "top"\nversion = "0.1.0"\nedition = "2021"\n\n[dev-dependencies]\nmid = { path = "../mid" }\n',
            encoding="utf-8",
        )
        (cls.fixture / "crates/builder/Cargo.toml").write_text(
            '[package]\nname = "builder"\nversion = "0.1.0"\nedition = "2021"\n\n[build-dependencies]\nmid = { path = "../mid" }\n',
            encoding="utf-8",
        )
        (cls.fixture / "crates/extra/Cargo.toml").write_text(
            '[package]\nname = "extra"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nleaf = { path = "../leaf" }\n',
            encoding="utf-8",
        )
        (cls.fixture / "crates/cfg/Cargo.toml").write_text(
            "[package] # inline comment\nname = 'fixture-config'\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nleaf = { path = \"../leaf\" }\n",
            encoding="utf-8",
        )
        (cls.fixture / "crates/alone/Cargo.toml").write_text(
            '[package]\nname = "alone"\nversion = "0.1.0"\nedition = "2021"\n',
            encoding="utf-8",
        )
        (cls.fixture / "test/Cargo.toml").write_text(
            '[package]\nname = "integration-tests"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nleaf = { path = "../crates/leaf" }\n',
            encoding="utf-8",
        )
        subprocess.run(
            ["cargo", "generate-lockfile", "--offline", "--quiet"],
            cwd=cls.fixture,
            check=True,
        )

    @classmethod
    def tearDownClass(cls) -> None:
        cls._tmp.cleanup()

    def test_reverse_closure_and_fallbacks(self) -> None:
        self.assertEqual(
            affected_crates.select_affected_crates(
                self.fixture, ["README.md", "crates/leaf/README.md", "test/README.md"]
            ),
            ("none", []),
        )

        for ws_file in (
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            ".config/nextest.toml",
            "clippy.toml",
            "vendor/hotpath/src/lib.rs",
            "test/src/lib.rs",
            "test/tests/postgres.rs",
            "env/config/local.toml",
            "scripts/tool.rs",
            "crates/ghost/src/lib.rs",
        ):
            with self.subTest(file=ws_file):
                self.assertEqual(
                    affected_crates.select_affected_crates(self.fixture, [ws_file]),
                    ("workspace", []),
                )

        # Normal + dev + build reverse closure
        self.assertEqual(
            affected_crates.select_affected_crates(self.fixture, ["crates/mid/src/lib.rs"]),
            ("packages", ["builder", "mid", "top"]),
        )
        self.assertEqual(
            affected_crates.select_affected_crates(self.fixture, ["crates/mid/build.rs"]),
            ("packages", ["builder", "mid", "top"]),
        )
        self.assertEqual(
            affected_crates.select_affected_crates(self.fixture, ["crates/cfg/src/lib.rs"]),
            ("packages", ["fixture-config"]),
        )

        # Crate integration test stays scoped to its crate
        self.assertEqual(
            affected_crates.select_affected_crates(
                self.fixture,
                ["crates/alone/src/lib.rs", "crates/mid/tests/it.rs", "crates/mid/tests/fixtures/a.toml"],
            ),
            ("packages", ["alone", "mid"]),
        )

        # >= 80% workspace closure (7/8 crates affected) falls back to workspace
        self.assertEqual(
            affected_crates.select_affected_crates(self.fixture, ["crates/leaf/src/lib.rs"]),
            ("workspace", []),
        )

        # CLI output formats: lines, space, json
        for fmt, expected_out in (
            ("lines", "builder\nmid\ntop\n"),
            ("space", "builder mid top\n"),
        ):
            res = subprocess.run(
                [
                    sys.executable,
                    str(VERIFY_DIR / "affected_crates.py"),
                    "--repo",
                    str(self.fixture),
                    "--files",
                    "crates/mid/src/lib.rs",
                    "--format",
                    fmt,
                ],
                capture_output=True,
                text=True,
            )
            self.assertEqual(res.returncode, 0, res.stderr)
            self.assertEqual(res.stdout, expected_out)

        json_res = subprocess.run(
            [
                sys.executable,
                str(VERIFY_DIR / "affected_crates.py"),
                "--repo",
                str(self.fixture),
                "--files",
                "crates/mid/src/lib.rs",
                "--format",
                "json",
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(json_res.returncode, 0, json_res.stderr)
        payload = json.loads(json_res.stdout)
        self.assertEqual(payload["mode"], "packages")
        self.assertEqual(payload["crates"], ["builder", "mid", "top"])
        self.assertFalse(payload["fallback"])

        # Missing cargo binary falls back to workspace cleanly
        missing_cargo = subprocess.run(
            [
                sys.executable,
                str(VERIFY_DIR / "affected_crates.py"),
                "--repo",
                str(self.fixture),
                "--files",
                "crates/mid/src/lib.rs",
                "--format",
                "json",
            ],
            env={**os.environ, "CARGO": "missing-cargo-binary-for-test"},
            capture_output=True,
            text=True,
        )
        self.assertEqual(missing_cargo.returncode, 0)
        self.assertEqual(json.loads(missing_cargo.stdout)["mode"], "workspace")


class VerifyRunnerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="verify-runner-test-")))
        self.fixture = self.temp / "repo"
        self.lock_dir = self.temp / "validation.lock"
        self.fixture.mkdir()

        (self.fixture / "Cargo.toml").write_text(
            '[workspace]\nresolver = "2"\nmembers = ["crates/*"]\n',
            encoding="utf-8",
        )
        for crate in ("leaf", "mid", "alone", "other"):
            cdir = self.fixture / f"crates/{crate}/src"
            cdir.mkdir(parents=True)
            (cdir / "lib.rs").write_text("", encoding="utf-8")
            (self.fixture / f"crates/{crate}/Cargo.toml").write_text(
                f'[package]\nname = "{crate}"\nversion = "0.1.0"\nedition = "2021"\n',
                encoding="utf-8",
            )
        with (self.fixture / "crates/mid/Cargo.toml").open("a", encoding="utf-8") as handle:
            handle.write('\n[dependencies]\nleaf = { path = "../leaf" }\n')

        (self.fixture / "Cargo.lock").write_text(
            'version = 4\n\n[[package]]\nname = "alone"\nversion = "0.1.0"\n\n'
            '[[package]]\nname = "leaf"\nversion = "0.1.0"\n\n'
            '[[package]]\nname = "mid"\nversion = "0.1.0"\ndependencies = ["leaf"]\n\n'
            '[[package]]\nname = "other"\nversion = "0.1.0"\n',
            encoding="utf-8",
        )
        (self.fixture / "tools").mkdir()
        (self.fixture / "tools/versions.env").write_text("RUFF_VERSION=0.15.11\n", encoding="utf-8")
        (self.fixture / "scripts/checks").mkdir(parents=True)
        (self.fixture / "scripts/checks/monitoring_rules.sh").write_text("#!/bin/sh\n", encoding="utf-8")
        (self.fixture / "scripts/checks/native_transport.py").write_text("", encoding="utf-8")
        (self.fixture / "scripts/profiles").mkdir(parents=True)
        (self.fixture / "scripts/profiles/fixture.sh").write_text("#!/bin/sh\n", encoding="utf-8")
        (self.fixture / ".agents/roles").mkdir(parents=True)
        (self.fixture / ".agents/roles/worker-agent.toml").write_text("", encoding="utf-8")
        (self.fixture / "README.md").write_text("# Fixture\n", encoding="utf-8")
        (self.fixture / ".gitleaks.toml").write_text("", encoding="utf-8")
        (self.fixture / "Makefile").write_text(
            "check-instructions:\n\t@printf 'instructions ok\\n'\n"
            "docs-check:\n\t@printf 'docs ok\\n'\n"
            "shellcheck:\n\t@printf 'shell ok\\n'\n"
            "quality-check-self-test:\n\t@printf 'quality self-test ok\\n'\n"
            "template-quality-projections:\n\t@printf 'quality projections ok\\n'\n",
            encoding="utf-8",
        )
        _git(self.fixture, "init", "-q")
        _git(self.fixture, "add", "-A")
        _git(self.fixture, "commit", "-qm", "init")

    def _run_verify(self, *args: str, **env_overrides: str) -> subprocess.CompletedProcess[str]:
        env = _clean_verify_env(VALIDATION_LOCK_DIR=str(self.lock_dir), **env_overrides)
        return subprocess.run(
            [sys.executable, str(VERIFY_DIR / "runner.py"), "--repo", str(self.fixture), *args],
            cwd=self.fixture,
            env=env,
            capture_output=True,
            text=True,
        )

    @staticmethod
    def _plan_section(output: str, header: str) -> str:
        lines: list[str] = []
        capturing = False
        for line in output.splitlines():
            if line == f"{header}:":
                capturing = True
                continue
            if capturing:
                if line and not line.startswith(" "):
                    break
                lines.append(line)
        return "\n".join(lines)

    def test_fingerprint_sensitivity(self) -> None:
        head = _git(self.fixture, "rev-parse", "HEAD")
        plain = self.fixture / "plain"
        exe = self.fixture / "executable"
        link = self.fixture / "link"
        plain.write_text("plain\n", encoding="utf-8")
        exe.write_text("exe\n", encoding="utf-8")
        exe.chmod(0o755)
        link.symlink_to("plain")

        watched = ["plain", "executable", "link", "deleted"]
        base_fp = runner.fingerprint_candidate(
            self.fixture,
            watched,
            base_ref="HEAD",
            resolved_base_sha=head,
            merge_base_sha=head,
        )

        plain.chmod(0o755)
        self.assertNotEqual(
            runner.fingerprint_candidate(
                self.fixture, watched, base_ref="HEAD", resolved_base_sha=head, merge_base_sha=head
            ),
            base_fp,
        )
        plain.chmod(0o644)

        plain.write_text("changed\n", encoding="utf-8")
        self.assertNotEqual(
            runner.fingerprint_candidate(
                self.fixture, watched, base_ref="HEAD", resolved_base_sha=head, merge_base_sha=head
            ),
            base_fp,
        )
        plain.write_text("plain\n", encoding="utf-8")

        link.unlink()
        link.symlink_to("executable")
        self.assertNotEqual(
            runner.fingerprint_candidate(
                self.fixture, watched, base_ref="HEAD", resolved_base_sha=head, merge_base_sha=head
            ),
            base_fp,
        )
        link.unlink()
        link.symlink_to("plain")

        (self.fixture / "deleted").write_text("new\n", encoding="utf-8")
        self.assertNotEqual(
            runner.fingerprint_candidate(
                self.fixture, watched, base_ref="HEAD", resolved_base_sha=head, merge_base_sha=head
            ),
            base_fp,
        )
        (self.fixture / "deleted").unlink()
        self.assertEqual(
            runner.fingerprint_candidate(
                self.fixture, watched, base_ref="HEAD", resolved_base_sha=head, merge_base_sha=head
            ),
            base_fp,
        )

        (self.fixture / ".jscpd.json").write_text('{"threshold": 0}\n', encoding="utf-8")
        q_fp = runner.fingerprint_candidate(
            self.fixture,
            watched,
            base_ref="HEAD",
            resolved_base_sha=head,
            merge_base_sha=head,
            include_quality_configs=True,
        )
        (self.fixture / ".jscpd.json").write_text('{"threshold": 1}\n', encoding="utf-8")
        self.assertNotEqual(
            runner.fingerprint_candidate(
                self.fixture,
                watched,
                base_ref="HEAD",
                resolved_base_sha=head,
                merge_base_sha=head,
                include_quality_configs=True,
            ),
            q_fp,
        )

    def test_plan_routing_and_quality_tooling_gate(self) -> None:
        res = self._run_verify("--plan", "--files", ".agents/roles/worker-agent.toml")
        self.assertEqual(res.returncode, 0, res.stderr)
        self.assertIn("  make check-instructions\n", res.stdout)

        for path, expected in (
            ("clippy.toml", True),
            ("Cargo.lock", True),
            ("crates/leaf/src/lib.rs", True),
            ("rustfmt.toml", False),
            (".typos.toml", False),
        ):
            with self.subTest(path=path):
                res = self._run_verify("--plan", "--files", path, REPOSITORY_CI="none")
                self.assertEqual(res.returncode, 0, res.stderr)
                self.assertEqual("  make profiling-check\n" in res.stdout, expected)

        res = self._run_verify("--plan", "--files", "README.md")
        self.assertEqual(res.returncode, 0)
        self.assertIn("documentation=true", res.stdout)
        self.assertIn("  make docs-check\n", res.stdout)
        self.assertIn("requires_docker=true", res.stdout)

        res = self._run_verify("--plan", "--files", ".github/dependabot.yml")
        self.assertEqual(res.returncode, 0)
        self.assertIn("  make tools-check\n", res.stdout)
        self.assertIn("dependency_automation:", res.stdout)

        res = self._run_verify("--plan", "--files", "LICENSE")
        self.assertEqual(res.returncode, 0)
        self.assertIn("  make dockerfile-check\n", res.stdout)

        res = self._run_verify("--plan", "--files", ".editorconfig")
        self.assertEqual(res.returncode, 0)
        self.assertIn("  none\n", res.stdout)

        # Ordinary .rs edit routes duplication-check + architecture-check + affected crates, NOT quality-check-self-test or template-quality-projections
        (self.fixture / "make").mkdir(exist_ok=True)
        (self.fixture / "make/source.mk").write_text("", encoding="utf-8")
        res = self._run_verify("--plan", "--files", "crates/leaf/src/lib.rs")
        self.assertEqual(res.returncode, 0, res.stderr)
        self.assertIn("  make duplication-check\n", res.stdout)
        self.assertIn("  make architecture-check\n", res.stdout)
        self.assertIn("  make fmt-check\n", res.stdout)
        self.assertIn("  make lint-changed PKGS='leaf mid'\n", res.stdout)
        self.assertIn("  make test-changed PKGS='leaf mid'\n", res.stdout)
        self.assertIn("  make unused-deps\n", res.stdout)
        self.assertNotIn("quality-check-self-test", res.stdout)
        self.assertNotIn("template-quality-projections", res.stdout)
        self.assertNotIn("  make test\n", res.stdout)

        q_res = self._run_verify("--plan", "--files", ".jscpd.json")
        self.assertEqual(q_res.returncode, 0, q_res.stderr)
        self.assertIn("  make duplication-check\n", q_res.stdout)
        self.assertIn("  make architecture-check\n", q_res.stdout)
        self.assertIn("  make quality-check-self-test\n", q_res.stdout)
        self.assertIn("  make template-quality-projections\n", q_res.stdout)

        # Template engine files ALSO route quality-check-self-test and template-quality-projections
        t_res = self._run_verify("--plan", "--files", "scripts/template/init.py")
        self.assertEqual(t_res.returncode, 0, t_res.stderr)
        self.assertIn("  make quality-check-self-test\n", t_res.stdout)
        self.assertIn("  make template-quality-projections\n", t_res.stdout)

        for ci_mode in ("github-actions", "none"):
            with self.subTest(ci_mode=ci_mode):
                sync_res = self._run_verify("--plan", "--files", "scripts/template/sync.py", REPOSITORY_CI=ci_mode)
                self.assertEqual(sync_res.returncode, 0, sync_res.stderr)
                commands = self._plan_section(sync_res.stdout, "commands")
                self.assertIn("  make template-init-projections\n", commands)
                source_section = "ci-owned" if ci_mode == "github-actions" else "commands"
                self.assertIn(
                    "  make template-init-source-checks\n",
                    self._plan_section(sync_res.stdout, source_section),
                )
                for unrelated in ("template-init-check", "template-init-artifacts", "template-quality-projections"):
                    self.assertNotIn(unrelated, sync_res.stdout)

        # template:begin runtime-progress:test-runner-runtime-progress
        rp_res = self._run_verify("--plan", "--files", "build/docker/runtime-progress.Dockerfile")
        self.assertEqual(rp_res.returncode, 0, rp_res.stderr)
        self.assertIn("  make runtime-progress-proof", self._plan_section(rp_res.stdout, "ci-owned"))
        self.assertIn("  make template-init-projections\n", self._plan_section(rp_res.stdout, "commands"))
        for unrelated in ("dockerfile-check", "runtime-image-build", "template-init-artifacts"):
            self.assertNotIn(unrelated, rp_res.stdout)
        # template:end runtime-progress:test-runner-runtime-progress
        (self.fixture / "make/source.mk").unlink()

        # Cargo.lock routes workspace lint, build, test, deny, unused-deps, native-transport-regressions
        lock_res = self._run_verify("--plan", "--files", "Cargo.lock")
        self.assertEqual(lock_res.returncode, 0)
        for expected in ("make deny", "make dependency-drift", "make unused-deps", "make lint", "make build", "make test"):
            self.assertIn(f"  {expected}\n", lock_res.stdout)
        self.assertIn("make native-transport-regressions", self._plan_section(lock_res.stdout, "ci-owned"))

        # template:begin grpc:test-verify-grpc-generator-deny
        (self.fixture / "tools/grpc-codegen").mkdir(parents=True, exist_ok=True)
        (self.fixture / "tools/grpc-codegen/Cargo.toml").write_text("", encoding="utf-8")
        gen_deny_res = self._run_verify("--plan", "--files", "tools/grpc-codegen/deny.toml")
        self.assertEqual(gen_deny_res.returncode, 0, gen_deny_res.stderr)
        self.assertIn("  make deny\n", gen_deny_res.stdout)
        self.assertIn("  make grpc-generator-deny\n", gen_deny_res.stdout)
        shutil.rmtree(self.fixture / "tools/grpc-codegen")
        # template:end grpc:test-verify-grpc-generator-deny

        # Shell existing vs removed
        sh_res = self._run_verify("--plan", "--files", "scripts/profiles/fixture.sh")
        self.assertIn("  make shellcheck SHELL_FILES='scripts/profiles/fixture.sh'\n", sh_res.stdout)
        rm_sh_res = self._run_verify("--plan", "--files", "scripts/profiles/removed.sh")
        self.assertIn("shell: no changed shell source remains", rm_sh_res.stdout)

        # Dockerfile CI-owned vs ALLOW_HEAVY=1 vs REPOSITORY_CI=none
        df_res = self._run_verify("--plan", "--files", "build/docker/Dockerfile")
        self.assertIn("  make dockerfile-check", self._plan_section(df_res.stdout, "commands"))
        self.assertIn("  make runtime-image-build RUNTIME_IMAGE=service:verify", self._plan_section(df_res.stdout, "ci-owned"))

        heavy_res = self._run_verify("--plan", "--files", "build/docker/Dockerfile", ALLOW_HEAVY="1")
        self.assertIn("  make runtime-image-build RUNTIME_IMAGE=service:verify", self._plan_section(heavy_res.stdout, "commands"))

        head_sha = _git(self.fixture, "rev-parse", "HEAD")
        none_res = self._run_verify("--plan", "--files", "build/docker/Dockerfile", REPOSITORY_CI="none")
        self.assertIn("ci_mode: none\n", none_res.stdout)
        self.assertIn("  make runtime-image-build RUNTIME_IMAGE=service:verify", self._plan_section(none_res.stdout, "commands"))
        self.assertIn(f"  make secret-scan BASE_REF={head_sha}", self._plan_section(none_res.stdout, "commands"))

        # template:begin grpc:test-runner-grpc
        grpc_res = self._run_verify("--plan", "--files", "buf.yaml")
        self.assertIn("  make grpc-check", self._plan_section(grpc_res.stdout, "ci-owned"))
        grpc_none = self._run_verify("--plan", "--files", "buf.yaml", REPOSITORY_CI="none")
        self.assertIn("  make grpc-check", self._plan_section(grpc_none.stdout, "commands"))
        self.assertIn("  make grpc-generator-deny", self._plan_section(grpc_none.stdout, "commands"))
        # template:end grpc:test-runner-grpc

        bad_ci = self._run_verify("--plan", "--files", "README.md", REPOSITORY_CI="gitlab")
        self.assertEqual(bad_ci.returncode, 2)

    def test_host_shellcheck_and_lychee_fallback_when_docker_unavailable(self) -> None:
        # Build isolated bin directory without shellcheck or lychee
        isolated_bin = self.temp / "isolated-bin"
        isolated_bin.mkdir()
        for tool in ("git", "make", "python3", "sh", "printf", "cargo", "rustc"):
            found = shutil.which(tool)
            if found:
                (isolated_bin / tool).symlink_to(found)

        # Hard-docker target (dockerfile-check) always blocks without Docker
        df_blocked = self._run_verify(
            "--files",
            "build/docker/Dockerfile",
            PATH=str(isolated_bin),
        )
        self.assertEqual(df_blocked.returncode, 2)
        self.assertIn("Docker is required", df_blocked.stderr)

        # Shell surface blocks when NEITHER Docker NOR host shellcheck exists
        sh_blocked = self._run_verify(
            "--files",
            "scripts/profiles/fixture.sh",
            PATH=str(isolated_bin),
        )
        self.assertEqual(sh_blocked.returncode, 2)
        self.assertIn("Docker is required", sh_blocked.stderr)

        # Add host shellcheck & lychee stubs -> succeeds locally (CI != "true")!
        for host_tool in ("shellcheck", "lychee"):
            stub = isolated_bin / host_tool
            stub.write_text(f"#!/bin/sh\nprintf '{host_tool} 1.0.0\\n'\n", encoding="utf-8")
            stub.chmod(0o755)

        sh_pass = self._run_verify(
            "--files",
            "scripts/profiles/fixture.sh",
            VERIFY_FORCE="1",
            PATH=str(isolated_bin),
        )
        self.assertEqual(sh_pass.returncode, 0, sh_pass.stderr)
        self.assertIn("result: pass\n", sh_pass.stdout)

        docs_pass = self._run_verify(
            "--files",
            "README.md",
            VERIFY_FORCE="1",
            PATH=str(isolated_bin),
        )
        self.assertEqual(docs_pass.returncode, 0, docs_pass.stderr)
        self.assertIn("result: pass\n", docs_pass.stdout)

        # Under CI="true", host fallback is disallowed -> blocks when Docker is unavailable!
        ci_blocked = self._run_verify(
            "--files",
            "scripts/profiles/fixture.sh",
            CI="true",
            PATH=str(isolated_bin),
        )
        self.assertEqual(ci_blocked.returncode, 2)
        self.assertIn("Docker is required", ci_blocked.stderr)

    def test_receipt_reuse_invalidation_failure_and_pruning(self) -> None:
        first = self._run_verify("--files", ".agents/roles/worker-agent.toml", VERIFY_FORCE="1")
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertIn("instructions ok", first.stdout)
        self.assertIn("result: pass\n", first.stdout)
        self.assertIn("status: verified\n", first.stdout)

        reused = self._run_verify("--files", ".agents/roles/worker-agent.toml")
        self.assertEqual(reused.returncode, 0, reused.stderr)
        self.assertIn("reusing exact passing receipt:", reused.stdout)

        # Partially verified when CI owns heavy/full steps
        (self.fixture / "make").mkdir(exist_ok=True)
        (self.fixture / "make/source.mk").write_text("", encoding="utf-8")
        partial = self._run_verify(
            "--files",
            ".agents/roles/worker-agent.toml",
            "scripts/tests/template-fixture.json",
            VERIFY_FORCE="1",
        )
        self.assertEqual(partial.returncode, 0, partial.stderr)
        self.assertIn("status: partially_verified\n", partial.stdout)
        self.assertIn("ci_owned: make template-init-check", partial.stdout)
        (self.fixture / "make/source.mk").unlink()

        # Candidate mutation during step invalidates attempt and writes no new receipt
        verify_dir = self.fixture / ".git/codex/verify"
        receipts_before = len(list(verify_dir.glob("*.receipt")))
        (self.fixture / "Makefile").write_text(
            "check-instructions:\n\t@printf 'mutated\\n' >>.agents/roles/worker-agent.toml\n",
            encoding="utf-8",
        )
        invalidated = self._run_verify("--files", ".agents/roles/worker-agent.toml", VERIFY_FORCE="1")
        self.assertEqual(invalidated.returncode, 1)
        self.assertEqual(len(list(verify_dir.glob("*.receipt"))), receipts_before)
        attempt_line = next(
            line for line in invalidated.stdout.splitlines() if line.startswith("verification attempt: ")
        )
        attempt_file = Path(attempt_line.removeprefix("verification attempt: "))
        attempt_text = attempt_file.read_text(encoding="utf-8")
        self.assertIn("step_state: 1 invalidated", attempt_text)
        self.assertIn("attempt_state: invalidated", attempt_text)

        # Failing step stops execution before subsequent steps and writes no receipt
        (self.fixture / "Makefile").write_text(
            "check-instructions:\n\t@exit 42\n"
            "secret-scan:\n\t@touch should-not-run\n",
            encoding="utf-8",
        )
        failed = self._run_verify(
            "--files",
            ".agents/roles/worker-agent.toml",
            ".gitleaks.toml",
            VERIFY_FORCE="1",
        )
        self.assertEqual(failed.returncode, 1)
        self.assertFalse((self.fixture / "should-not-run").exists())
        self.assertIn("result: fail\n", failed.stderr)
        self.assertEqual(len(list(verify_dir.glob("*.receipt"))), receipts_before)

        # Pruning retains at most 20 newest receipts and 20 newest attempt logs
        for idx in range(25):
            r_file = verify_dir / f"old-{idx:02d}.receipt"
            a_file = verify_dir / f"attempt-old-{idx:02d}.log"
            r_file.write_text("receipt\n", encoding="utf-8")
            a_file.write_text("attempt\n", encoding="utf-8")
            ts = 1_700_000_000 + idx
            os.utime(r_file, (ts, ts))
            os.utime(a_file, (ts, ts))

        (self.fixture / "Makefile").write_text(
            "check-instructions:\n\t@printf 'ok\\n'\n",
            encoding="utf-8",
        )
        pruned_run = self._run_verify("--files", ".agents/roles/worker-agent.toml", VERIFY_FORCE="1")
        self.assertEqual(pruned_run.returncode, 0, pruned_run.stderr)

        remaining_receipts = list(verify_dir.glob("*.receipt"))
        remaining_attempts = [
            p for p in verify_dir.iterdir() if p.name.startswith("attempt-")
        ]
        self.assertEqual(len(remaining_receipts), 20)
        self.assertEqual(len(remaining_attempts), 20)
        self.assertFalse((verify_dir / "old-00.receipt").exists())
        self.assertFalse((verify_dir / "attempt-old-00.log").exists())


if __name__ == "__main__":
    unittest.main()
