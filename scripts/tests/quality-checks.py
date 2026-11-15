#!/usr/bin/env python3
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


def module(name: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / f"scripts/ci/{name}.py")
    assert spec is not None and spec.loader is not None
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


arch = module("architecture-check")


def jscpd_version(root: Path) -> str:
    versions = (root / "tools/versions.env").read_text(encoding="utf-8")
    matches = re.findall(r"^JSCPD_VERSION=([0-9]+\.[0-9]+\.[0-9]+)$", versions, re.M)
    if len(matches) != 1:
        raise AssertionError("tools/versions.env must declare one JSCPD_VERSION")
    return matches[0]


class Architecture(unittest.TestCase):
    def setUp(self) -> None:
        self.root = ROOT
        self.policy = json.loads((ROOT / "quality/architecture.json").read_text())
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
            ("infra-http", "infra-webhooks", "build"),
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


class Native(unittest.TestCase):
    def test_detector_scope_and_new_clone(self) -> None:
        version = jscpd_version(ROOT)
        env = dict(os.environ, npm_config_prefer_offline="true")
        cmd = ["npx", "--yes", f"jscpd@{version}", "--config", ".jscpd.json", "."]
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

            clean = subprocess.run(cmd, cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(clean.returncode, 0, clean.stderr or clean.stdout)

            (root / production[1]).write_bytes((root / production[0]).read_bytes())
            cloned = subprocess.run(cmd, cwd=root, env=env, capture_output=True, text=True)
            self.assertNotEqual(cloned.returncode, 0)

            (root / production[1]).write_text(
                "/* jscpd:ignore-start */\n"
                + (root / production[0]).read_text(encoding="utf-8")
                + "/* jscpd:ignore-end */\n"
                + snippet.replace("value", "distinct"),
                encoding="utf-8",
            )
            ignored_clone = subprocess.run(cmd, cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(ignored_clone.returncode, 0, ignored_clone.stderr or ignored_clone.stdout)


class CommandAdmission(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        (self.root / "scripts/ci").mkdir(parents=True)
        (self.root / "bin").mkdir()
        self.env = dict(os.environ, PATH=str(self.root / "bin") + os.pathsep + os.environ["PATH"])

    def executable(self, path: Path, body: str) -> str:
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o755)
        return str(path)

    def check_script(self, name: str, **settings: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(self.root / "scripts/ci" / name)],
            cwd=self.root,
            env=dict(self.env, **settings),
            capture_output=True,
            text=True,
        )

    @unittest.skipUnless((ROOT / "scripts/ci/grpc-check.sh").is_file(), "gRPC profile removed")
    def test_grpc_base_absence_requires_a_readable_tree(self) -> None:
        shutil.copy(ROOT / "scripts/ci/grpc-check.sh", self.root / "scripts/ci")
        (self.root / "scripts/grpc-generate.sh").write_text('mkdir -p "$1"\n')
        (self.root / "crates/grpc-contracts/src/generated").mkdir(parents=True)
        self.executable(self.root / "bin/buf", 'printf "%s\\n" "$*"')

        def git(*args: str) -> str:
            return subprocess.check_output(
                [
                    "git",
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "gc.auto=0",
                    *args,
                ],
                cwd=self.root,
                text=True,
            ).strip()

        git("init", "-q")
        git("commit", "--allow-empty", "-qm", "before protobuf")
        result = self.check_script("grpc-check.sh", GRPC_BASE_REF="HEAD", BUF="buf")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("first addition", result.stdout)
        (self.root / "api/proto").mkdir(parents=True)
        (self.root / "api/proto/contract.proto").touch()
        git("add", "api")
        git("commit", "-qm", "protobuf base")
        result = self.check_script("grpc-check.sh", GRPC_BASE_REF="HEAD", BUF="buf")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"breaking --against .git#ref={git('rev-parse', 'HEAD')}", result.stdout)
        tree = git("rev-parse", "HEAD^{tree}")
        (self.root / ".git/objects" / tree[:2] / tree[2:]).unlink()
        result = self.check_script("grpc-check.sh", GRPC_BASE_REF="HEAD", BUF="buf")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("first addition", result.stdout)

    @unittest.skipUnless((ROOT / "scripts/ci/test-integration-db.sh").is_file(), "PostgreSQL profile removed")
    def test_database_proof_requires_profile_only_for_owned_fixtures(self) -> None:
        shutil.copy(ROOT / "scripts/ci/test-integration-db.sh", self.root / "scripts/ci")
        (self.root / "scripts/ci/validation-lock.sh").write_text("exit 0\n")
        lib = self.root / "scripts/lib"
        lib.mkdir()
        (lib / "template_state.py").write_text(
            'import os, sys\nprint(os.environ["PROFILE"])\nsys.exit(int(os.environ["PROFILE_STATUS"]))\n'
        )
        (lib / "compose-postgres.sh").write_text(
            """
require_docker() { :; }
compose_postgres_up() { :; }
compose_pgbouncer_up() { :; }
compose_postgres_cleanup() { :; }
compose_postgres() { echo "compose $*" >> events; echo '127.0.0.1:4222'; }
compose_postgres_host_dsn() { echo postgres; }
compose_pgbouncer_host_dsn() { echo pgbouncer; }
"""
        )
        self.executable(self.root / "bin/cargo", "echo cargo >> events")
        events = self.root / "events"
        for managed, profile, status, success, nats in (
            ("0", "none", "1", False, False),
            ("0", "none", "0", True, False),
            ("0", "nats-jetstream", "0", True, True),
            ("1", "none", "1", True, False),
        ):
            with self.subTest(managed=managed, profile=profile, status=status):
                events.write_text("")
                result = self.check_script(
                    "test-integration-db.sh",
                    PROFILE=profile,
                    PROFILE_STATUS=status,
                    INTEGRATION_COMPOSE_MANAGED=managed,
                    DATABASE_URL="external-postgres",
                    PGBOUNCER_DATABASE_URL="external-pgbouncer",
                )
                self.assertEqual(result.returncode == 0, success, result.stderr)
                recorded = events.read_text().splitlines()
                self.assertEqual("cargo" in recorded, success)
                self.assertEqual("compose up -d --wait nats" in recorded, nats)

    def test_tool_pins_require_successful_version_output(self) -> None:
        name = "tools-check.sh"
        shutil.copy(ROOT / "scripts/ci" / name, self.root / "scripts/ci")
        for path in (
            "tools/versions.env",
            "build/docker/Dockerfile",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "scripts/ci/dependency-drift.sh",
            "make/template.mk",
            ".github/dependabot.yml",
        ):
            target = self.root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / path, target)
        for var in ("CARGO_DENY", "CARGO_NEXTEST", "CARGO_SHEAR", "ZIZMOR"):
            banner = f'. ./tools/versions.env\nprintf "tool v%s (build 1.0.0)\\n" "${{{var}_VERSION}}"'
            if var == "CARGO_DENY":
                banner = f'[ "$EMPTY_BANNER" = 1 ] || {{ {banner}; }}\nexit "$VERSION_STATUS"'
            self.env[var] = self.executable(self.root / "bin" / var, banner)
        for empty, status, success in (("0", "0", True), ("0", "1", False), ("1", "0", False)):
            with self.subTest(empty=empty, status=status):
                result = self.check_script(name, EMPTY_BANNER=empty, VERSION_STATUS=status)
                self.assertEqual(result.returncode == 0, success, result.stderr)
        make_mk = self.root / "make/template.mk"
        original_mk = make_mk.read_text()
        make_mk.write_text(original_mk.replace("NPX_COOLDOWN_DAYS := 7", "NPX_COOLDOWN_DAYS := 3"))
        result = self.check_script(name, EMPTY_BANNER="0", VERSION_STATUS="0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("npx cooldown days", result.stderr)
        make_mk.write_text(original_mk)
        cargo_cfg = self.root / ".cargo/config.toml"
        cargo_cfg.write_text(cargo_cfg.read_text().replace('"7 days"', '"3 days"'))
        result = self.check_script(name, EMPTY_BANNER="0", VERSION_STATUS="0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("global-min-publish-age", result.stderr)


if __name__ == "__main__":
    unittest.main()
