#!/usr/bin/env python3
"""Unit and smoke tests for architecture.py and duplication-check via npx.py."""

from __future__ import annotations

import importlib.util
import json
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


def _load_architecture():
    spec = importlib.util.spec_from_file_location("architecture", ROOT / "scripts/checks/architecture.py")
    assert spec is not None and spec.loader is not None
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


arch = _load_architecture()


def jscpd_version(root: Path) -> str:
    versions = (root / "tools/versions.env").read_text(encoding="utf-8")
    matches = re.findall(r"^JSCPD_VERSION=([0-9]+\.[0-9]+\.[0-9]+)$", versions, re.M)
    if len(matches) != 1:
        raise AssertionError("tools/versions.env must declare one JSCPD_VERSION")
    return matches[0]


class Architecture(unittest.TestCase):
    def setUp(self) -> None:
        self.root = ROOT
        self.policy = json.loads((ROOT / "quality/architecture.json").read_text(encoding="utf-8"))
        self.meta = {
            "version": 1,
            "workspace_members": list(self.policy["members"]),
            "packages": [
                {
                    "id": path,
                    "name": Path(path).parent.name,
                    "manifest_path": str(ROOT / path),
                    "dependencies": [],
                }
                for path in self.policy["members"]
            ],
        }

    def edge(self, owner: str, dependency_path: str, kind: str | None = None, **extra: object) -> None:
        package = next(p for p in self.meta["packages"] if p["id"] == owner)
        package["dependencies"] = [
            dict(
                name=Path(dependency_path).parent.name,
                path=str((ROOT / dependency_path).parent),
                kind=kind,
                optional=False,
                **extra,
            )
        ]

    def verdict(self) -> list[str]:
        return arch.check(self.meta, self.policy, self.root)

    def test_forbidden_declarations_include_inactive_build_alias_and_test_target(self) -> None:
        for kind, extra in ((None, {}), ("build", {}), (None, {"rename": "innocent", "target": "cfg(windows)"})):
            with self.subTest(kind=kind, extra=extra):
                self.edge("crates/infra-postgres/Cargo.toml", "crates/infra-http/Cargo.toml", kind, **extra)
                self.meta["packages"][list(self.policy["members"]).index("crates/infra-postgres/Cargo.toml")][
                    "dependencies"
                ][0]["optional"] = True
                errors = self.verdict()
                self.assertTrue(errors)
                self.assertIn("infra-postgres", errors[0])
                self.assertIn("infra-http", errors[0])
                self.assertIn("optional=True", errors[0])
        self.edge("crates/service/Cargo.toml", "test/Cargo.toml")
        self.assertTrue(any("production-to-test-only" in error for error in self.verdict()))

    def test_documented_exceptions_and_dev_composition(self) -> None:
        for owner, target, kind in (
            ("infra-http", "infra-postgres", None),
            ("infra-http", "infra-idempotency-store", "build"),
            ("infra-messaging", "infra-jobs", None),
            ("infra-grpc", "grpc-contracts", "dev"),
        ):
            with self.subTest(owner=owner, target=target):
                self.edge(f"crates/{owner}/Cargo.toml", f"crates/{target}/Cargo.toml", kind)
                self.assertEqual([], self.verdict())
        self.edge("test/Cargo.toml", "crates/service/Cargo.toml", "build")
        self.assertEqual([], self.verdict())
        self.edge("crates/infra-oauth2-client-credentials/Cargo.toml", "crates/infra-grpc/Cargo.toml")
        edge = next(
            p for p in self.meta["packages"] if p["id"] == "crates/infra-oauth2-client-credentials/Cargo.toml"
        )["dependencies"][0]
        edge["optional"] = True
        self.assertEqual([], self.verdict())

    def test_path_inclusion_may_not_leave_its_package(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            files = {
                "crates/owner/Cargo.toml": "",
                "crates/owner/src/lib.rs": '#[path = "inner/kept.rs"]\nmod kept;\n'
                '#[path = "../../other/src/shared.rs"]\nmod shared;\n',
                "crates/owner/src/inner/kept.rs": "",
                "crates/owner/tests/it.rs": '#[ path = "../../../test/fixtures/x.rs" ]\nmod x;\n',
                "crates/other/Cargo.toml": "",
                "crates/other/src/shared.rs": "",
                "test/Cargo.toml": "",
                "test/tests/own.rs": '#[path = "../fixtures/x.rs"]\nmod x;\n',
                "test/fixtures/x.rs": "",
            }
            for relative, text in files.items():
                (root / relative).parent.mkdir(parents=True, exist_ok=True)
                (root / relative).write_text(text)
            manifests = ["crates/owner/Cargo.toml", "crates/other/Cargo.toml", "test/Cargo.toml"]
            self.assertEqual(
                [
                    "crates/owner/src/lib.rs: #[path] reaches outside its package (../../other/src/shared.rs)",
                    "crates/owner/tests/it.rs: #[path] reaches outside its package (../../../test/fixtures/x.rs)",
                ],
                sorted(arch.cross_package_path_inclusions(manifests, root)),
            )

    def test_unknown_member_kind_and_escaping_local_dependency(self) -> None:
        self.meta["packages"].append(
            {"id": "new", "name": "new", "manifest_path": str(ROOT / "crates/new/Cargo.toml"), "dependencies": []}
        )
        self.meta["workspace_members"].append("new")
        self.assertTrue(any("unclassified" in error for error in self.verdict()))
        self.meta["workspace_members"].pop()
        self.edge("crates/service/Cargo.toml", "crates/config/Cargo.toml", "future")
        self.assertTrue(any("unknown-dependency-kind" in error for error in self.verdict()))
        self.edge("crates/service/Cargo.toml", "../outside/Cargo.toml")
        self.assertTrue(any("outside-classified" in error for error in self.verdict()))

    def test_profile_removal_and_package_rename(self) -> None:
        self.meta["packages"] = [
            p for p in self.meta["packages"] if p["id"] in {"crates/service/Cargo.toml", "crates/config/Cargo.toml"}
        ]
        self.meta["workspace_members"] = [p["id"] for p in self.meta["packages"]]
        self.meta["packages"][0]["name"] = "renamed-service-or-config"
        self.edge("crates/service/Cargo.toml", "crates/config/Cargo.toml")
        self.assertEqual([], self.verdict())

    def test_registered_feature_and_provider_capabilities(self) -> None:
        feature = "crates/orders/Cargo.toml"
        provider = "crates/infra-orders/Cargo.toml"
        for path, role in ((feature, "feature"), (provider, "feature-provider")):
            self.policy["members"][path] = {"role": role}
            self.meta["workspace_members"].append(path)
            self.meta["packages"].append(
                {
                    "id": path,
                    "name": Path(path).parent.name,
                    "manifest_path": str(ROOT / path),
                    "dependencies": [],
                }
            )
        self.edge(feature, "crates/infra-cache/Cargo.toml")
        self.assertEqual([], self.verdict())
        self.edge(feature, "crates/infra-jobs/Cargo.toml")
        self.assertTrue(self.verdict())
        self.edge(feature, "crates/infra-http/Cargo.toml")
        self.edge(provider, feature)
        self.assertTrue(self.verdict())
        self.policy["members"][provider]["allow_members"] = {
            feature: {
                "reason": "Orders adapter calls the orders use case",
                "authority": "docs/repo-architecture.md",
            }
        }
        self.assertEqual([], self.verdict())

    def test_dependents_allowance_admits_only_listed_members(self) -> None:
        target = "crates/operation-context/Cargo.toml"
        listed = self.policy["members"][target]["allow_dependents"]["members"]
        for owner in listed:
            with self.subTest(owner=owner):
                self.edge(owner, target)
                self.assertEqual([], self.verdict())
                self.edge(
                    owner,
                    "crates/domain-events/Cargo.toml"
                    if "messaging" not in owner
                    else "crates/grpc-contracts/Cargo.toml",
                )
                self.assertTrue(self.verdict())
                next(p for p in self.meta["packages"] if p["id"] == owner)["dependencies"] = []
        for owner in ("crates/infra-postgres/Cargo.toml", "crates/infra-idempotency-store/Cargo.toml"):
            with self.subTest(owner=owner):
                self.assertNotIn(owner, listed)
                self.edge(owner, target)
                self.assertTrue(any("operation-context" in error for error in self.verdict()))
                next(p for p in self.meta["packages"] if p["id"] == owner)["dependencies"] = []
        for broken in (
            {"members": listed},
            {"reason": "r", "authority": "a", "members": []},
            {"reason": "r", "authority": "a", "members": ["crates/absent/Cargo.toml"]},
            {"reason": " ", "authority": "a", "members": listed},
        ):
            with self.subTest(broken=broken):
                self.policy["members"][target]["allow_dependents"] = broken
                self.assertTrue(any("architecture input" in error for error in self.verdict()))

    def test_std_only_role_refuses_every_non_dev_dependency(self) -> None:
        owner = "crates/shared-rules/Cargo.toml"
        self.assertTrue(self.policy["roles"][self.policy["members"][owner]["role"]]["std_only"])
        self.assertEqual([], self.verdict())
        package = next(p for p in self.meta["packages"] if p["id"] == owner)
        registry = {
            "name": "itoa",
            "kind": None,
            "optional": False,
            "source": "registry+https://github.com/rust-lang/crates.io-index",
        }
        for dependency in (registry, dict(registry, kind="build"), dict(registry, optional=True)):
            with self.subTest(dependency=dependency):
                package["dependencies"] = [dependency]
                self.assertTrue(any("forbids every non-dev dependency" in error for error in self.verdict()))
        package["dependencies"] = [dict(registry, kind="dev")]
        self.assertEqual([], self.verdict())
        self.edge(owner, "crates/operation-context/Cargo.toml")
        self.assertTrue(any("forbids every non-dev dependency" in error for error in self.verdict()))
        package["dependencies"] = []
        self.edge("crates/infra-idempotency-store/Cargo.toml", owner)
        self.assertTrue(any("shared-rules" in error for error in self.verdict()))
        self.policy["roles"]["shared-rules"]["std_only"] = "yes"
        self.assertTrue(any("architecture input" in error for error in self.verdict()))

    def test_cli_custom_policy_argument(self) -> None:
        with tempfile.TemporaryDirectory(prefix="arch-cli-") as temporary:
            temp_dir = Path(temporary).resolve()
            bin_dir = temp_dir / "bin"
            bin_dir.mkdir()
            meta_file = temp_dir / "metadata.json"
            policy_file = temp_dir / "custom-policy.json"
            (temp_dir / "crates/service").mkdir(parents=True)
            manifest = str((temp_dir / "crates/service/Cargo.toml").resolve())
            meta_file.write_text(
                json.dumps(
                    {
                        "version": 1,
                        "workspace_members": ["service"],
                        "packages": [
                            {
                                "id": "service",
                                "name": "service",
                                "manifest_path": manifest,
                                "dependencies": [],
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            policy_file.write_text(
                json.dumps(
                    {
                        "version": 1,
                        "roles": {"root": {"rule": "root-rule"}},
                        "members": {"crates/service/Cargo.toml": {"role": "root"}},
                    }
                ),
                encoding="utf-8",
            )
            cargo_stub = bin_dir / "cargo"
            cargo_stub.write_text(f'#!/bin/sh\ncat "{meta_file}"\n', encoding="utf-8")
            cargo_stub.chmod(0o755)
            env = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}"}
            ok = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "scripts/checks/architecture.py"),
                    "--repo",
                    str(temp_dir),
                    "--policy",
                    "custom-policy.json",
                ],
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(ok.returncode, 0, ok.stderr)


class Native(unittest.TestCase):
    @unittest.skipUnless(shutil.which("npx") is not None, "npx is required for jscpd smoke test")
    def test_detector_scope_and_new_clone(self) -> None:
        version = jscpd_version(ROOT)
        npx_wrapper = str(ROOT / "scripts/checks/npx.py")
        cmd = [sys.executable, npx_wrapper, f"jscpd@{version}", "--config", ".jscpd.json", "."]
        with tempfile.TemporaryDirectory(prefix="quality-native-") as temporary:
            root = Path(temporary).resolve()
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            shutil.copy(ROOT / ".jscpd.json", root / ".jscpd.json")
            snippet = (
                "pub fn cloned(value: i32) -> i32 {\n"
                + "".join(f"    let value = value + {i};\n" for i in range(24))
                + "    value\n}\n"
            )
            production = [
                "crates/example/src/first.rs",
                "crates/example/src/latest.rs",
                "crates/example/src/generated/handwritten.rs",
            ]
            excluded = [
                "crates/example/tests/scenario.rs",
                "crates/example/src/tests.rs",
                "crates/grpc-contracts/src/generated/contract.rs",
                "crates/example/src/ignored.rs",
            ]
            for index, rel in enumerate(production):
                file = root / rel
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text(snippet.replace("value", f"value{index}"), encoding="utf-8")
            (root / ".gitignore").write_text("ignored.rs\n", encoding="utf-8")
            for rel in excluded:
                file = root / rel
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text((root / production[0]).read_text(encoding="utf-8"), encoding="utf-8")

            clean = subprocess.run(cmd, cwd=root, capture_output=True, text=True)
            self.assertEqual(clean.returncode, 0, clean.stderr or clean.stdout)

            (root / production[1]).write_bytes((root / production[0]).read_bytes())
            cloned = subprocess.run(cmd, cwd=root, capture_output=True, text=True)
            self.assertNotEqual(cloned.returncode, 0)

            (root / production[1]).write_text(
                "/* jscpd:ignore-start */\n"
                + (root / production[0]).read_text(encoding="utf-8")
                + "/* jscpd:ignore-end */\n"
                + snippet.replace("value", "distinct"),
                encoding="utf-8",
            )
            ignored_clone = subprocess.run(cmd, cwd=root, capture_output=True, text=True)
            self.assertEqual(ignored_clone.returncode, 0, ignored_clone.stderr or ignored_clone.stdout)


if __name__ == "__main__":
    unittest.main()
