#!/usr/bin/env python3
"""Policy boundary fixtures plus tiny pinned detector and Clippy smoke cases."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[2]


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / f"scripts/ci/{name}.py")
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


dup = module("duplication-check")
arch = module("architecture-check")


def location(data, offset):
    prefix = data[:offset]
    return {"position": offset, "line": prefix.count(b"\n") + 1,
            "column": len(prefix.rsplit(b"\n", 1)[-1])}


def file_interval(root, path, start, end):
    data = (root / path).read_bytes()
    a, b = location(data, start), location(data, end)
    return {"name": path, "startLoc": a, "endLoc": b, "start": a["line"], "end": b["line"]}


def report(root, paths, pairs):
    return {"statistics": {"total": {"sources": len(paths), "tokens": 1000, "clones": len(pairs)}},
            "duplicates": [{"format": "rust", "kind": "exact", "tokens": tokens,
                            "firstFile": file_interval(root, *a), "secondFile": file_interval(root, *b)}
                           for a, b, tokens in pairs]}


class Admission(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.paths = ["a.rs", "b.rs", "c.rs", "d.rs"]
        self.original = "let first = 1;\nlet second = 2;\nlet third = 3;\n"
        occurrences = []
        for path in self.paths:
            before, after = f"// before {path}\n", f"// after {path}\n"
            (self.root / path).write_text(before + self.original + after)
            occurrences.append({"path": path, "max_tokens": 131 if path == "a.rs" else 277,
                                "source": self.original,
                                "before": {"text": before, "boundary": False},
                                "after": {"text": after, "boundary": False}})
        self.baseline = {"version": 1, "cases": [{"id": "recorders", "kind": "test",
                         "reason": "Independent recorder owners", "occurrences": occurrences}]}

    def interval(self, path, text=None):
        data = (self.root / path).read_bytes()
        content = (text or self.original).encode()
        start = data.index(content)
        return path, start, start + len(content)

    def verdict(self, pairs):
        return dup.admit(report(self.root, self.paths, pairs), self.root, self.paths, self.baseline)

    def test_family_pairs_shift_shrink_and_profile_removal(self):
        # Each of the six pairs is permitted, even when it was not originally
        # chosen as the detector's first-copy pairing.
        for i, first in enumerate(self.paths):
            for second in self.paths[i + 1:]:
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

    def test_growth_changed_text_and_extra_copy_cannot_use_old_budget(self):
        for changed in (self.original + "let fourth = 4;\n",
                        self.original.replace("second = 2", "second = 20"),
                        self.original.replace("let second = 2;\n", "// inserted comment\nlet second = 2;\n")):
            with self.subTest(changed=changed):
                (self.root / "b.rs").write_text("// before b.rs\n" + changed + "// after b.rs\n")
                self.assertTrue(self.verdict([(self.interval("a.rs"), self.interval("b.rs", changed), 120)]))
        # Remove an independent allowance: it cannot pay for a newly copied file.
        self.baseline["cases"].append({"id": "deleted", "kind": "production", "reason": "gone",
                                      "occurrences": [dict(x, path="missing-" + x["path"]) for x in self.baseline["cases"][0]["occurrences"][:2]]})
        self.paths.append("new.rs")
        (self.root / "new.rs").write_text(self.original)
        self.assertTrue(self.verdict([(self.interval("a.rs"), self.interval("new.rs"), 120)]))
        # Relocation inside the same file must also fail the envelope bound.
        with (self.root / "a.rs").open("a") as output:
            output.write(self.original)
        data = (self.root / "a.rs").read_bytes()
        start = data.rindex(self.original.encode())
        self.assertTrue(self.verdict([(("a.rs", start, start + len(self.original)), self.interval("d.rs"), 120)]))

    def test_capacity_splits_deduplication_and_byte_bounds(self):
        first = "let first = 1;\n"
        third = "let third = 3;\n"
        split = [(self.interval("a.rs", first), self.interval("b.rs", first), 50),
                 (self.interval("a.rs", third), self.interval("b.rs", third), 50)]
        self.assertEqual([], self.verdict(split + split))
        self.assertTrue(self.verdict([(a, b, 70) for a, b, _ in split]))
        full = (self.interval("a.rs"), self.interval("b.rs"), 120)
        self.assertTrue(self.verdict(split + [full]))
        self.assertTrue(self.verdict([(full[0], full[1], 132)]))
        # Whole-line fragment context cannot extend the first endpoint.
        outside = ("a.rs", full[0][1] - 1, full[0][2])
        self.assertTrue(self.verdict([(outside, full[1], 120)]))
        malformed = report(self.root, self.paths, [full])
        malformed["duplicates"][0]["firstFile"]["endLoc"]["position"] = 100000
        with self.assertRaises(dup.CheckError):
            dup.admit(malformed, self.root, self.paths, self.baseline)

    def test_crlf_indentation_and_ambiguous_anchor(self):
        changed = "    " + self.original.replace("\n", "\n    ").rstrip(" ")
        for path in ("a.rs", "b.rs"):
            text = f"// before {path}\n" + changed + f"// after {path}\n"
            (self.root / path).write_bytes(text.replace("\n", "\r\n").encode())
        interval = changed.replace("\n", "\r\n")
        self.assertEqual([], self.verdict([(self.interval("a.rs", interval), self.interval("b.rs", interval), 120)]))
        with (self.root / "a.rs").open("a") as output:
            output.write("// before a.rs\n")
        self.assertTrue(self.verdict([(self.interval("a.rs", interval), self.interval("b.rs", interval), 120)]))

    def test_fail_closed_report_schema_and_empty_scan(self):
        valid = report(self.root, self.paths, [])
        for bad in ({}, dict(valid, duplicates={}), dict(valid, statistics=None), dict(valid, statistics={}),
                    dict(valid, statistics={"total": {"sources": 0, "tokens": 0, "clones": 0}})):
            with self.subTest(report=bad), self.assertRaises(dup.CheckError):
                dup.validate_report(bad, self.root, self.paths)


class Architecture(unittest.TestCase):
    def setUp(self):
        self.root = ROOT
        self.policy = json.loads((ROOT / "quality/architecture.json").read_text())
        self.meta = {"version": 1, "workspace_members": list(self.policy["members"]),
                     "packages": [{"id": path, "name": Path(path).parent.name,
                                   "manifest_path": str(ROOT / path), "dependencies": []}
                                  for path in self.policy["members"]]}

    def edge(self, owner, dependency_path, kind=None, **extra):
        package = next(p for p in self.meta["packages"] if p["id"] == owner)
        package["dependencies"] = [dict(name=Path(dependency_path).parent.name, path=str((ROOT / dependency_path).parent),
                                       kind=kind, optional=False, **extra)]

    def verdict(self):
        return arch.check(self.meta, self.policy, self.root)

    def test_forbidden_declarations_include_inactive_build_alias_and_test_target(self):
        for kind, extra in ((None, {}), ("build", {}), (None, {"rename": "innocent", "target": "cfg(windows)"})):
            with self.subTest(kind=kind, extra=extra):
                self.edge("crates/infra-postgres/Cargo.toml", "crates/config/Cargo.toml", kind, **extra)
                self.meta["packages"][list(self.policy["members"]).index("crates/infra-postgres/Cargo.toml")]["dependencies"][0]["optional"] = True
                errors = self.verdict()
                self.assertTrue(errors)
                self.assertIn("infra-postgres", errors[0])
                self.assertIn("config", errors[0])
                self.assertIn("optional=True", errors[0])
        self.edge("crates/service/Cargo.toml", "test/Cargo.toml")
        self.assertTrue(any("production-to-test-only" in error for error in self.verdict()))

    def test_documented_exceptions_and_dev_composition(self):
        for owner, target, kind in (("infra-http", "infra-postgres", None),
                                    ("infra-http", "infra-webhooks", "build"),
                                    ("infra-messaging", "infra-jobs", None),
                                    ("infra-grpc", "grpc-contracts", "dev")):
            with self.subTest(owner=owner, target=target):
                self.edge(f"crates/{owner}/Cargo.toml", f"crates/{target}/Cargo.toml", kind)
                self.assertEqual([], self.verdict())
        self.edge("test/Cargo.toml", "crates/service/Cargo.toml", "build")
        self.assertEqual([], self.verdict())
        self.edge("crates/infra-oauth2-client-credentials/Cargo.toml", "crates/infra-grpc/Cargo.toml")
        edge = next(p for p in self.meta["packages"] if p["id"] == "crates/infra-oauth2-client-credentials/Cargo.toml")["dependencies"][0]
        edge["optional"] = True
        self.assertEqual([], self.verdict())

    def test_unknown_member_kind_and_escaping_local_dependency(self):
        self.meta["packages"].append({"id": "new", "name": "new", "manifest_path": str(ROOT / "crates/new/Cargo.toml"), "dependencies": []})
        self.meta["workspace_members"].append("new")
        self.assertTrue(any("unclassified" in error for error in self.verdict()))
        self.meta["workspace_members"].pop()
        self.edge("crates/service/Cargo.toml", "crates/config/Cargo.toml", "future")
        self.assertTrue(any("unknown-dependency-kind" in error for error in self.verdict()))
        self.edge("crates/service/Cargo.toml", "../outside/Cargo.toml")
        self.assertTrue(any("outside-classified" in error for error in self.verdict()))

    def test_profile_removal_and_package_rename(self):
        self.meta["packages"] = [p for p in self.meta["packages"] if p["id"] in {"crates/service/Cargo.toml", "crates/config/Cargo.toml"}]
        self.meta["workspace_members"] = [p["id"] for p in self.meta["packages"]]
        self.meta["packages"][0]["name"] = "renamed-service-or-config"
        self.edge("crates/service/Cargo.toml", "crates/config/Cargo.toml")
        self.assertEqual([], self.verdict())

    def test_registered_feature_and_provider_capabilities(self):
        feature = "crates/orders/Cargo.toml"
        provider = "crates/infra-orders/Cargo.toml"
        for path, role in ((feature, "feature"), (provider, "feature-provider")):
            self.policy["members"][path] = {"role": role}
            self.meta["workspace_members"].append(path)
            self.meta["packages"].append({"id": path, "name": Path(path).parent.name,
                "manifest_path": str(ROOT / path), "dependencies": []})
        self.edge(feature, "crates/infra-cache/Cargo.toml")
        self.assertEqual([], self.verdict())
        self.edge(feature, "crates/infra-jobs/Cargo.toml")
        self.assertTrue(self.verdict())
        self.edge(feature, "crates/infra-http/Cargo.toml")
        self.edge(provider, feature)
        self.assertTrue(self.verdict())
        self.policy["members"][provider]["allow_members"] = {
            feature: {"reason": "Orders adapter calls the orders use case",
                      "authority": "docs/architecture/boundaries.md"}}
        self.assertEqual([], self.verdict())


class Native(unittest.TestCase):
    def test_detector_scope_and_new_clone(self):
        with tempfile.TemporaryDirectory(prefix="quality-native-") as temporary:
            root = Path(temporary).resolve()
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / "tools").mkdir()
            shutil.copy(ROOT / "tools/versions.env", root / "tools/versions.env")
            shutil.copy(ROOT / ".jscpd.json", root / ".jscpd.json")
            (root / "quality").mkdir()
            (root / "quality/duplication-baseline.json").write_text('{"version":1,"cases":[]}')
            (root / "Cargo.toml").write_text('[workspace]\nmembers=["crates/example", "crates/grpc-contracts"]\nresolver="3"\n')
            for member in ("example", "grpc-contracts"):
                directory = root / "crates" / member
                (directory / "src").mkdir(parents=True)
                (directory / "Cargo.toml").write_text(f'[package]\nname="{member}"\nversion="0.1.0"\nedition="2024"\n')
                (directory / "src/lib.rs").write_text(f'pub const NAME: &str = "{member}";\n')
            (root / "Cargo.lock").write_text('version = 4\n\n[[package]]\nname = "example"\nversion = "0.1.0"\n\n[[package]]\nname = "grpc-contracts"\nversion = "0.1.0"\n')
            snippet = "pub fn cloned(value: i32) -> i32 {\n" + "".join(f"    let value = value + {i};\n" for i in range(24)) + "    value\n}\n"
            owners = ["crates/example/src/first.rs", "crates/example/src/latest.rs", "crates/example/src/generated/handwritten.rs", "crates/example/tests/scenario.rs", "crates/example/src/tests.rs", "crates/grpc-contracts/src/generated/contract.rs"]
            for index, path in enumerate(owners):
                file = root / path
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text(snippet.replace("value", f"value{index}"))
            (root / ".gitignore").write_text("ignored.rs\n")
            (root / "crates/example/src/ignored.rs").write_text(snippet)
            (root / "crates/example/src/link.rs").symlink_to("first.rs")
            source, tests = dup.discover(root, dup.metadata(root))
            self.assertIn(owners[1], source)  # 'latest' is not a dedicated test component.
            self.assertIn(owners[2], source)  # Only the exact generated owner is excluded.
            self.assertEqual(set(owners[3:5]), set(tests))
            self.assertNotIn(owners[5], source)
            self.assertNotIn("crates/example/src/ignored.rs", source)
            self.assertNotIn("crates/example/src/link.rs", source)
            command, config = dup.native_command(root), dup.detector_config(root)
            initial = dup.scan(root, source, root / "reports/initial", command, config)
            self.assertEqual([], initial["duplicates"])
            (root / owners[1]).write_bytes((root / owners[0]).read_bytes())
            cloned = dup.scan(root, source, root / "reports/clone", command, config)
            self.assertTrue(dup.admit(cloned, root, source, {"version": 1, "cases": []}))
            for path in tests:
                (root / path).unlink()
            destination = root / "reports/persistent"
            (destination / "tests").mkdir(parents=True)
            (destination / "tests/jscpd-report.json").write_text('{"stale":true}')
            baseline = (root / "quality/duplication-baseline.json").read_bytes()
            reported = subprocess.run(["python3", str(ROOT / "scripts/ci/duplication-check.py"),
                "report", "--root", str(root), "--output", str(destination)], capture_output=True, text=True)
            self.assertEqual(0, reported.returncode, reported.stderr)
            self.assertIn("Dedicated-test scope absent", reported.stdout)
            self.assertTrue((destination / "source/jscpd-report.json").is_file())
            self.assertFalse((destination / "tests/jscpd-report.json").exists())
            self.assertEqual(baseline, (root / "quality/duplication-baseline.json").read_bytes())
            # Bad configuration and missing executables fail instead of counting
            # as empty successful scans. Test at their production boundary.
            (root / ".jscpd.json").write_text('{"mode":"weak"}')
            with self.assertRaises(dup.CheckError):
                dup.detector_config(root)
            with self.assertRaises(OSError):
                dup.run([str(root / "missing-detector")], root)
            with self.assertRaises(dup.CheckError):
                dup.run(["git", "not-a-command"], root)
            with self.assertRaises(dup.CheckError):
                dup.scan(root, source, root / "reports/missing", ["true"], config)

    def test_pinned_clippy_nesting_boundary(self):
        with tempfile.TemporaryDirectory(prefix="quality-nesting-") as temporary:
            root = Path(temporary)
            shutil.copy(ROOT / "clippy.toml", root / "clippy.toml")
            driver = subprocess.check_output(["rustup", "which", "clippy-driver"], cwd=ROOT, text=True).strip()
            for depth, expected in ((6, 0), (7, 1)):
                # Function body counts as one block. Independently constructed
                # nested conditions pin Clippy's configured > threshold rule.
                code = "pub fn nesting(value: bool) {\n" + "if value {\n" * (depth - 1) + "let _answer = value;\n" + "}\n" * depth
                source = root / f"depth{depth}.rs"
                source.write_text(code)
                result = subprocess.run([driver, str(source), "--edition=2024", "--crate-type=lib", "--emit=metadata", "-o", str(root / f"depth{depth}.rmeta"), "-Aclippy::all", "-Dclippy::excessive_nesting"], cwd=ROOT, env=dict(os.environ, CLIPPY_CONF_DIR=str(root)), capture_output=True, text=True)
                if expected:
                    self.assertNotEqual(0, result.returncode, result.stderr)
                    self.assertIn("excessive_nesting", result.stderr)
                else:
                    self.assertEqual(0, result.returncode, result.stderr)


if __name__ == "__main__":
    unittest.main()
