#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
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


def module(name: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / f"scripts/ci/{name}.py")
    assert spec is not None and spec.loader is not None
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


dup = module("duplication-check")
arch = module("architecture-check")


def location(data: bytes, offset: int) -> dict[str, int]:
    prefix = data[:offset]
    return {
        "position": offset,
        "line": prefix.count(b"\n") + 1,
        "column": len(prefix.rsplit(b"\n", 1)[-1]),
    }


def file_interval(root: Path, path: str, start: int, end: int) -> dict[str, object]:
    data = (root / path).read_bytes()
    a, b = location(data, start), location(data, end)
    return {"name": path, "startLoc": a, "endLoc": b, "start": a["line"], "end": b["line"]}


def report(root: Path, paths: list[str], pairs: list[tuple[tuple[str, int, int], tuple[str, int, int], int]]) -> dict[str, object]:
    return {
        "statistics": {"total": {"sources": len(paths), "tokens": 1000, "clones": len(pairs)}},
        "duplicates": [
            {
                "format": "rust",
                "kind": "exact",
                "tokens": tokens,
                "firstFile": file_interval(root, *a),
                "secondFile": file_interval(root, *b),
            }
            for a, b, tokens in pairs
        ],
    }


class Admission(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.paths = ["a.rs", "b.rs", "c.rs", "d.rs"]
        self.original = "let first = 1;\nlet second = 2;\nlet third = 3;\n"
        occurrences = []
        for path in self.paths:
            before, after = f"// before {path}\n", f"// after {path}\n"
            (self.root / path).write_text(before + self.original + after)
            occurrences.append(
                {
                    "path": path,
                    "max_tokens": 131 if path == "a.rs" else 277,
                    "source": self.original,
                    "before": {"text": before, "boundary": False},
                    "after": {"text": after, "boundary": False},
                }
            )
        self.baseline = {
            "version": 1,
            "cases": [
                {
                    "id": "recorders",
                    "kind": "test",
                    "reason": "Independent recorder owners",
                    "occurrences": occurrences,
                }
            ],
        }

    def interval(self, path: str, text: str | None = None) -> tuple[str, int, int]:
        data = (self.root / path).read_bytes()
        content = (text or self.original).encode()
        start = data.index(content)
        return path, start, start + len(content)

    def verdict(self, pairs: list[tuple[tuple[str, int, int], tuple[str, int, int], int]]) -> list[str]:
        return dup.admit(report(self.root, self.paths, pairs), self.root, self.paths, self.baseline)

    def test_family_pairs_shift_shrink_and_profile_removal(self) -> None:
        for i, first in enumerate(self.paths):
            for second in self.paths[i + 1 :]:
                with self.subTest(pair=(first, second)):
                    self.assertEqual([], self.verdict([(self.interval(first), self.interval(second), 120)]))
        (self.root / "b.rs").unlink()
        (self.root / "c.rs").unlink()
        self.paths = ["a.rs", "d.rs"]
        reduced = "let first = 1;\nlet third = 3;\n"
        for path in ("a.rs", "d.rs"):
            data = (self.root / path).read_text().replace(self.original, reduced)
            (self.root / path).write_text("// harmless prefix shift\n" + data)
        self.assertEqual([], self.verdict([(self.interval("a.rs", reduced), self.interval("d.rs", reduced), 100)]))

    def test_growth_changed_text_and_extra_copy_cannot_use_old_budget(self) -> None:
        for changed in (
            self.original + "let fourth = 4;\n",
            self.original.replace("second = 2", "second = 20"),
            self.original.replace("let second = 2;\n", "// inserted comment\nlet second = 2;\n"),
        ):
            with self.subTest(changed=changed):
                (self.root / "b.rs").write_text("// before b.rs\n" + changed + "// after b.rs\n")
                self.assertTrue(self.verdict([(self.interval("a.rs"), self.interval("b.rs", changed), 120)]))
        self.baseline["cases"].append(
            {
                "id": "deleted",
                "kind": "production",
                "reason": "gone",
                "occurrences": [
                    dict(x, path="missing-" + x["path"]) for x in self.baseline["cases"][0]["occurrences"][:2]
                ],
            }
        )
        self.paths.append("new.rs")
        (self.root / "new.rs").write_text(self.original)
        self.assertTrue(self.verdict([(self.interval("a.rs"), self.interval("new.rs"), 120)]))
        with (self.root / "a.rs").open("a") as output:
            output.write(self.original)
        data = (self.root / "a.rs").read_bytes()
        start = data.rindex(self.original.encode())
        self.assertTrue(self.verdict([(("a.rs", start, start + len(self.original)), self.interval("d.rs"), 120)]))

    def test_capacity_splits_deduplication_and_byte_bounds(self) -> None:
        first = "let first = 1;\n"
        third = "let third = 3;\n"
        split = [
            (self.interval("a.rs", first), self.interval("b.rs", first), 50),
            (self.interval("a.rs", third), self.interval("b.rs", third), 50),
        ]
        self.assertEqual([], self.verdict(split + split))
        self.assertTrue(self.verdict([(a, b, 70) for a, b, _ in split]))
        full = (self.interval("a.rs"), self.interval("b.rs"), 120)
        self.assertTrue(self.verdict(split + [full]))
        self.assertTrue(self.verdict([(full[0], full[1], 132)]))
        outside = ("a.rs", full[0][1] - 1, full[0][2])
        self.assertTrue(self.verdict([(outside, full[1], 120)]))
        malformed = report(self.root, self.paths, [full])
        malformed["duplicates"][0]["firstFile"]["endLoc"]["position"] = 100000
        with self.assertRaises(dup.CheckError):
            dup.admit(malformed, self.root, self.paths, self.baseline)

    def test_crlf_indentation_and_ambiguous_anchor(self) -> None:
        changed = "    " + self.original.replace("\n", "\n    ").rstrip(" ")
        for path in ("a.rs", "b.rs"):
            text = f"// before {path}\n" + changed + f"// after {path}\n"
            (self.root / path).write_bytes(text.replace("\n", "\r\n").encode())
        interval = changed.replace("\n", "\r\n")
        self.assertEqual([], self.verdict([(self.interval("a.rs", interval), self.interval("b.rs", interval), 120)]))
        with (self.root / "a.rs").open("a") as output:
            output.write("// before a.rs\n")
        self.assertTrue(self.verdict([(self.interval("a.rs", interval), self.interval("b.rs", interval), 120)]))

    def test_fail_closed_report_schema_and_empty_scan(self) -> None:
        valid = report(self.root, self.paths, [])
        for bad in (
            {},
            dict(valid, duplicates={}),
            dict(valid, statistics=None),
            dict(valid, statistics={}),
            dict(valid, statistics={"total": {"sources": 0, "tokens": 0, "clones": 0}}),
        ):
            with self.subTest(report=bad), self.assertRaises(dup.CheckError):
                dup.validate_report(bad, self.root, self.paths)


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
        with tempfile.TemporaryDirectory(prefix="quality-native-") as temporary:
            root = Path(temporary).resolve()
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / "tools").mkdir()
            shutil.copy(ROOT / "tools/versions.env", root / "tools/versions.env")
            shutil.copy(ROOT / ".jscpd.json", root / ".jscpd.json")
            (root / "quality").mkdir()
            (root / "quality/duplication-baseline.json").write_text('{"version":1,"cases":[]}')
            (root / "Cargo.toml").write_text(
                '[workspace]\nmembers=["crates/example", "crates/grpc-contracts"]\nresolver="3"\n'
            )
            for member in ("example", "grpc-contracts"):
                directory = root / "crates" / member
                (directory / "src").mkdir(parents=True)
                (directory / "Cargo.toml").write_text(
                    f'[package]\nname="{member}"\nversion="0.1.0"\nedition="2024"\n'
                )
                (directory / "src/lib.rs").write_text(f'pub const NAME: &str = "{member}";\n')
            (root / "Cargo.lock").write_text(
                'version = 4\n\n[[package]]\nname = "example"\nversion = "0.1.0"\n\n[[package]]\nname = "grpc-contracts"\nversion = "0.1.0"\n'
            )
            snippet = (
                "pub fn cloned(value: i32) -> i32 {\n"
                + "".join(f"    let value = value + {i};\n" for i in range(24))
                + "    value\n}\n"
            )
            owners = [
                "crates/example/src/first.rs",
                "crates/example/src/latest.rs",
                "crates/example/src/generated/handwritten.rs",
                "crates/example/tests/scenario.rs",
                "crates/example/src/tests.rs",
                "crates/grpc-contracts/src/generated/contract.rs",
            ]
            for index, path in enumerate(owners):
                file = root / path
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text(snippet.replace("value", f"value{index}"))
            (root / ".gitignore").write_text("ignored.rs\n")
            (root / "crates/example/src/ignored.rs").write_text(snippet)
            (root / "crates/example/src/link.rs").symlink_to("first.rs")
            source = dup.discover(root, dup.metadata(root))
            self.assertIn(owners[1], source)  # 'latest' is not a dedicated test component.
            self.assertIn(owners[2], source)  # Only the exact generated owner is excluded.
            for dedicated in owners[3:]:
                self.assertNotIn(dedicated, source)
            self.assertNotIn("crates/example/src/ignored.rs", source)
            self.assertNotIn("crates/example/src/link.rs", source)
            command, config = dup.native_command(root), dup.detector_config(root)
            initial = dup.scan(root, source, root / "reports/initial", command, config)
            self.assertEqual([], initial["duplicates"])
            (root / owners[1]).write_bytes((root / owners[0]).read_bytes())
            cloned = dup.scan(root, source, root / "reports/clone", command, config)
            self.assertTrue(dup.admit(cloned, root, source, {"version": 1, "cases": []}))
            (root / ".jscpd.json").write_text('{"mode":"weak"}')
            with self.assertRaises(dup.CheckError):
                dup.detector_config(root)
            with self.assertRaises(OSError):
                dup.run([str(root / "missing-detector")], root)
            with self.assertRaises(dup.CheckError):
                dup.run(["git", "not-a-command"], root)
            with self.assertRaises(dup.CheckError):
                dup.scan(root, source, root / "reports/missing", ["true"], config)


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
            "scripts/ci/duplication-check.py",
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
        dup_py = self.root / "scripts/ci/duplication-check.py"
        original_dup = dup_py.read_text()
        dup_py.write_text(original_dup.replace("NPX_COOLDOWN_DAYS = 7", "NPX_COOLDOWN_DAYS = 3"))
        result = self.check_script(name, EMPTY_BANNER="0", VERSION_STATUS="0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("npx cooldown days", result.stderr)
        dup_py.write_text(original_dup)
        cargo_cfg = self.root / ".cargo/config.toml"
        cargo_cfg.write_text(cargo_cfg.read_text().replace('"7 days"', '"3 days"'))
        result = self.check_script(name, EMPTY_BANNER="0", VERSION_STATUS="0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("global-min-publish-age", result.stderr)


if __name__ == "__main__":
    unittest.main()
