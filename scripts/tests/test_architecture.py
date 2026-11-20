"""Unit tests for scripts.checks.architecture."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from scripts.checks import architecture


class ArchitectureTests(unittest.TestCase):
    def test_rejects_forbidden_edge_and_cross_package_path(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "crates/a/src").mkdir(parents=True)
            (root / "crates/b/src").mkdir(parents=True)
            (root / "crates/a/src/lib.rs").write_text('#[path = "../../b/src/lib.rs"] mod leak;\n', encoding="utf-8")
            (root / "crates/b/src/lib.rs").write_text("", encoding="utf-8")
            policy = {
                "roles": {"leaf": {"rule": "leaf-only"}, "root": {"rule": "root", "allow_roles": ["leaf"]}},
                "members": {"crates/a/Cargo.toml": {"role": "leaf"}, "crates/b/Cargo.toml": {"role": "root"}},
            }
            meta = {
                "workspace_members": ["a", "b"],
                "packages": [
                    {
                        "id": "a",
                        "name": "a",
                        "manifest_path": str(root / "crates/a/Cargo.toml"),
                        "dependencies": [{"name": "b", "kind": None, "optional": False, "path": str(root / "crates/b")}],
                    },
                    {"id": "b", "name": "b", "manifest_path": str(root / "crates/b/Cargo.toml"), "dependencies": []},
                ],
            }
            issues = architecture.check(meta, policy, root)
            self.assertTrue(any("forbids crates/b/Cargo.toml" in i for i in issues))
            self.assertTrue(any("#[path] reaches outside its package" in i for i in issues))

    def test_allow_all_dependents_and_stale_allowances(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for crate in ("shared", "used", "unused", "consumer"):
                (root / f"crates/{crate}/src").mkdir(parents=True)
                (root / f"crates/{crate}/src/lib.rs").write_text("", encoding="utf-8")
            policy = {
                "roles": {
                    "shared-rules": {"rule": "std-only-shared-rules", "std_only": True, "allow_all_dependents": True},
                    "leaf": {"rule": "leaf-only"},
                    "consumer": {
                        "rule": "consumer",
                        "allow_members": {
                            "crates/used/Cargo.toml": {"reason": "active", "authority": "test"},
                            "crates/unused/Cargo.toml": {"reason": "stale", "authority": "test"},
                        },
                    },
                    "feature": {
                        "rule": "scaffold",
                        "allow_members": {
                            "crates/used/Cargo.toml": {"reason": "scaffold", "authority": "test"},
                            "crates/missing/Cargo.toml": {"reason": "removed", "authority": "test"},
                        },
                    },
                },
                "members": {
                    "crates/shared/Cargo.toml": {"role": "shared-rules"},
                    "crates/used/Cargo.toml": {
                        "role": "leaf",
                        "allow_dependents": {"members": ["crates/consumer/Cargo.toml", "crates/unused/Cargo.toml"]},
                    },
                    "crates/unused/Cargo.toml": {"role": "leaf"},
                    "crates/consumer/Cargo.toml": {"role": "consumer"},
                },
            }
            meta = {
                "workspace_members": ["shared", "used", "unused", "consumer"],
                "packages": [
                    {"id": "shared", "name": "shared", "manifest_path": str(root / "crates/shared/Cargo.toml"), "dependencies": []},
                    {"id": "used", "name": "used", "manifest_path": str(root / "crates/used/Cargo.toml"), "dependencies": []},
                    {"id": "unused", "name": "unused", "manifest_path": str(root / "crates/unused/Cargo.toml"), "dependencies": []},
                    {
                        "id": "consumer",
                        "name": "consumer",
                        "manifest_path": str(root / "crates/consumer/Cargo.toml"),
                        "dependencies": [
                            {"name": "shared", "kind": None, "optional": False, "path": str(root / "crates/shared")},
                            {"name": "used", "kind": None, "optional": False, "path": str(root / "crates/used")},
                        ],
                    },
                ],
            }
            issues = architecture.check(meta, policy, root)
            self.assertEqual(
                issues,
                [
                    "crates/used/Cargo.toml: stale allow_dependents entry crates/unused/Cargo.toml",
                    "role consumer: stale allow_members entry crates/unused/Cargo.toml",
                    "role feature: stale allow_members entry crates/missing/Cargo.toml",
                ],
            )


if __name__ == "__main__":
    unittest.main()
