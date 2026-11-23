"""Hermetic unit tests for deterministic Cargo crate reverse-dependency selection."""

from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

from scripts.checks import rust_selection


def _synthetic_workspace(root: Path) -> dict[str, Any]:
    return {
        "workspace_members": [
            "health",
            "infra-cache",
            "infra-postgres",
            "migrate",
            "service-app",
            "integration-tests",
        ],
        "packages": [
            {
                "id": "health",
                "name": "health",
                "manifest_path": str(root / "crates/health/Cargo.toml"),
                "features": {},
                "dependencies": [],
            },
            {
                "id": "infra-cache",
                "name": "infra-cache",
                "manifest_path": str(root / "crates/infra-cache/Cargo.toml"),
                "features": {"integration": []},
                "dependencies": [{"name": "health"}],
            },
            {
                "id": "infra-postgres",
                "name": "infra-postgres",
                "manifest_path": str(root / "crates/infra-postgres/Cargo.toml"),
                "features": {},
                "dependencies": [{"name": "health"}],
            },
            {
                "id": "migrate",
                "name": "migrate",
                "manifest_path": str(root / "crates/migrate/Cargo.toml"),
                "features": {},
                "dependencies": [{"name": "infra-postgres"}],
            },
            {
                "id": "service-app",
                "name": "service-app",
                "manifest_path": str(root / "crates/service/Cargo.toml"),
                "features": {},
                "dependencies": [
                    {"name": "health"},
                    {"name": "infra-cache"},
                    {"name": "infra-postgres"},
                    {"name": "migrate"},
                ],
            },
            {
                "id": "integration-tests",
                "name": "integration-tests",
                "manifest_path": str(root / "test/Cargo.toml"),
                "features": {"integration": []},
                "dependencies": [{"name": "infra-postgres"}, {"name": "migrate"}],
            },
        ],
    }


class RustSelectionTests(unittest.TestCase):
    def test_adapter_change_selects_owner_and_reverse_dependents_only(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            meta = _synthetic_workspace(root)
            plan = rust_selection.plan_selection(root, ["crates/infra-cache/src/namespace.rs"], metadata=meta)
            self.assertEqual(plan.mode, "selected")
            self.assertEqual(plan.packages, ("infra-cache", "service-app"))
            self.assertEqual(plan.lint_features, ("infra-cache/integration",))
            self.assertEqual(plan.provenance["infra-cache"], ("owner:crates/infra-cache/src/namespace.rs",))
            self.assertEqual(plan.provenance["service-app"], ("reverse-dep:infra-cache",))

    def test_explicit_non_import_owners_select_affected_crates_including_renamed_service(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            meta = _synthetic_workspace(root)
            openapi_plan = rust_selection.plan_selection(root, ["api/openapi/service.yaml"], metadata=meta)
            self.assertEqual(openapi_plan.mode, "selected")
            self.assertEqual(openapi_plan.packages, ("service-app",))
            self.assertEqual(openapi_plan.provenance["service-app"], ("explicit-owner:api/openapi/service.yaml",))

            mig_plan = rust_selection.plan_selection(root, ["migrations/0001_init.sql"], metadata=meta)
            self.assertEqual(mig_plan.mode, "selected")
            self.assertEqual(mig_plan.packages, ("integration-tests", "migrate", "service-app"))
            self.assertEqual(mig_plan.lint_features, ("integration-tests/integration",))

            sqlx_plan = rust_selection.plan_selection(root, [".sqlx/query-abc.json"], metadata=meta)
            self.assertEqual(sqlx_plan.mode, "selected")
            self.assertEqual(sqlx_plan.packages, ("infra-postgres", "integration-tests", "migrate", "service-app"))
            self.assertNotIn("infra-cache", sqlx_plan.packages)

    def test_fail_closed_fallbacks_widen_to_full_workspace(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            meta = _synthetic_workspace(root)
            for path, prefix in (
                ("Cargo.toml", "global-input:"),
                ("Cargo.lock", "global-input:"),
                ("crates/service/Cargo.toml", "manifest-change:"),
                ("env/config/local.toml", "global-input:"),
                ("vendor/hotpath/src/lib.rs", "global-input:"),
                ("unknown/orphan.rs", "unmapped-input:"),
            ):
                plan = rust_selection.plan_selection(root, [path], metadata=meta)
                self.assertEqual(plan.mode, "full", f"expected full fallback for {path}, got {plan}")
                self.assertTrue(plan.reason.startswith(prefix), plan.reason)

            fallback_empty = rust_selection.plan_selection(root, ["docs/build-speed.md"], metadata=meta, allow_empty=False)
            self.assertEqual(fallback_empty.mode, "full")
            self.assertEqual(fallback_empty.reason, "no-rust-owners-fallback")

            local_empty = rust_selection.plan_selection(root, ["docs/build-speed.md"], metadata=meta, allow_empty=True)
            self.assertEqual(local_empty.mode, "empty")
            self.assertEqual(local_empty.packages, ())

            with mock.patch.dict(os.environ, {"CI_RUST_SELECTION": "full"}, clear=False):
                forced = rust_selection.plan_selection(root, ["crates/service/src/main.rs"], metadata=meta)
                self.assertEqual(forced.mode, "full")
                self.assertEqual(forced.reason, "env:CI_RUST_SELECTION=full")

    def test_closure_covering_every_member_normalizes_to_full(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            meta = _synthetic_workspace(root)
            plan = rust_selection.plan_selection(root, ["crates/health/src/lib.rs"], metadata=meta)
            self.assertEqual(plan.mode, "full")
            self.assertEqual(plan.reason, "closure-covers-workspace")

    def test_cli_parses_flags_before_double_dash_extra_args(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            meta = _synthetic_workspace(root)
            ran: list[list[str]] = []

            def fake_run(cmd: list[str], **_kwargs: object) -> mock.MagicMock:
                ran.append(list(cmd))
                return mock.MagicMock(returncode=0)

            with (
                mock.patch.object(rust_selection.lib, "cargo_metadata", return_value=meta),
                mock.patch.object(rust_selection.lib, "run", side_effect=fake_run),
            ):
                rc = rust_selection.main([
                    "test", "--repo", str(root), "--files", "crates/infra-cache/src/lib.rs", "--", "--no-fail-fast",
                ])
            self.assertEqual(rc, 0)
            self.assertIn("-p", ran[0])
            self.assertIn("infra-cache", ran[0])
            self.assertIn("--no-fail-fast", ran[0])

    def test_clickhouse_adapter_selects_clickhouse_integration_feature(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            meta = _synthetic_workspace(root)
            meta["workspace_members"].append("infra-clickhouse")
            meta["packages"].append(
                {
                    "id": "infra-clickhouse",
                    "name": "infra-clickhouse",
                    "manifest_path": str(root / "crates/infra-clickhouse/Cargo.toml"),
                    "features": {"integration": []},
                    "dependencies": [{"name": "health"}],
                }
            )
            svc = next(pkg for pkg in meta["packages"] if pkg["name"] == "service-app")
            svc["dependencies"].append({"name": "infra-clickhouse"})

            plan = rust_selection.plan_selection(root, ["crates/infra-clickhouse/src/query.rs"], metadata=meta)
            self.assertEqual(plan.mode, "selected")
            self.assertEqual(plan.packages, ("infra-clickhouse", "service-app"))
            self.assertEqual(plan.lint_features, ("infra-clickhouse/integration",))
            self.assertEqual(plan.provenance["infra-clickhouse"], ("owner:crates/infra-clickhouse/src/query.rs",))
            self.assertEqual(plan.provenance["service-app"], ("reverse-dep:infra-clickhouse",))

            vendor_plan = rust_selection.plan_selection(root, ["vendor/clickhouse/src/response.rs"], metadata=meta)
            self.assertEqual(vendor_plan.mode, "full")
            self.assertIn("infra-clickhouse/integration", vendor_plan.lint_features)


if __name__ == "__main__":
    unittest.main()
