"""Tests for service Make gates and Cargo executable selection."""

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


def _write_executable(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\n" + body + "\n", encoding="utf-8")
    path.chmod(0o755)


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

    def test_generated_make_has_no_initializer(self) -> None:
        result = self.run_make("template-init")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("No rule to make target", result.stderr)

    def test_openapi_breaking_preserves_idempotent_replay_on_the_original_url(self) -> None:
        checks = self.root / "scripts/checks"
        checks.mkdir(parents=True)
        shutil.copy2(ROOT / "scripts/checks/openapi_idempotency.py", checks)
        _write_executable(self.root / "bin/go", "exit 0")
        _write_executable(
            self.root / "bin/oasdiff",
            'case "$1" in\n'
            '  breaking) exit 0 ;;\n'
            '  upgrade) for document do :; done; cat "$document" ;;\n'
            '  *) exit 2 ;;\n'
            'esac',
        )

        def header(name, required=False):
            return {"in": "header", "name": name, "required": required, "schema": {"type": "string"}}

        def operation(*parameters, replay=True):
            response = {"description": "Created"}
            if replay:
                response["headers"] = {"Idempotent-Replayed": {"schema": {"type": "string", "enum": ["true"]}}}
            return {"parameters": [header("Idempotency-Key", required=True), *parameters], "responses": {"201": response}}

        path = "/v1/widgets/{id}"
        first = {"post": operation()}
        inherited = {
            "parameters": [{"$ref": "#/components/parameters/Region"}],
            "post": operation(header("X-Tenant")),
        }
        normalized = {
            "parameters": [header("X-REGION", required=True)],
            "post": operation(header("x-tenant"), header("X-REGION"), header("Content-Type")),
        }
        changed = {"post": operation(header("X-Tenant"), header("X-Channel"))}
        static_path = "/v1/widgets/latest"
        ordinary_post = {"post": operation(replay=False)}
        ordinary_get = {"get": {"responses": {"200": {"description": "Found"}}}}
        deprecated = {"post": {**operation(), "deprecated": True}}
        catchall_path = "/v1/widgets/{*rest}"
        detail_path = "/v1/widgets/{id}/details"
        static_detail = "/v1/widgets/latest/details"
        overlapping_dynamic = "/v1/widgets/latest/{view}"
        partitioned = {
            "/v1/{group}/{name}": ordinary_get,
            "/v1/{group}/{name}/": ordinary_get,
            "/v1/{group}/{name}/{*tail}": ordinary_get,
        }
        old_routes = {"/{*old}": first, **partitioned}
        missing_slash = {p: item for p, item in old_routes.items() if not p.endswith("/")}
        fallback_routes = {"/{*old}": ordinary_post, **partitioned}
        prefix_catchall = "/v1/widgets/archive/late{*tail}"
        archive_routes = {catchall_path: first, "/v1/widgets/archive/{name}": ordinary_get}
        components = {"parameters": {"Region": header("X-Region")}}
        cases = (
            (
                "first optional header",
                {path: first},
                {path: {"post": operation(header("X-Channel"))}},
                "added: x-channel; removed: none",
            ),
            ("existing fingerprint", {path: inherited}, {path: changed}, "added: x-channel; removed: x-region"),
            ("renamed path parameter", {path: inherited}, {"/v1/widgets/{widget_id}": changed}, "removed: x-region"),
            ("replay removed", {path: first}, {path: {"post": operation(replay=False)}}, "removed the idempotent replay contract"),
            ("same header names", {path: inherited}, {path: normalized}, None),
            ("versioned URL", {path: inherited}, {path: inherited, "/v2/widgets/{id}": changed}, None),
            (
                "new static POST shadows a retry",
                {path: first},
                {path: first, static_path: ordinary_post},
                f"{static_path} would take precedence over POST {path}",
            ),
            (
                "new static GET shadows a POST retry",
                {path: first},
                {path: first, static_path: ordinary_get},
                f"{static_path} would take precedence over POST {path}",
            ),
            (
                "new static route shadows a catchall retry",
                {catchall_path: first},
                {catchall_path: first, static_path: ordinary_get},
                f"{static_path} would take precedence over POST {catchall_path}",
            ),
            (
                "new catchall shadows part of a named path",
                {detail_path: first},
                {detail_path: first, "/v1/widgets/latest/{*rest}": ordinary_get},
                f"/v1/widgets/latest/{{*rest}} would take precedence over POST {detail_path}",
            ),
            (
                "new lower priority catchall leaves a static route intact",
                {static_path: first},
                {static_path: first, catchall_path: ordinary_get},
                None,
            ),
            (
                "new literal suffix is disjoint under a shared namespace",
                {detail_path: first},
                {detail_path: first, "/v1/widgets/latest/details-metadata": ordinary_get},
                None,
            ),
            (
                "new path only overlaps a URL already served by a higher priority route",
                {detail_path: first, static_detail: ordinary_get},
                {detail_path: first, static_detail: ordinary_get, overlapping_dynamic: ordinary_get},
                None,
            ),
            (
                "existing shadow covers only some previously protected URLs",
                {"/v1/widgets/{id}/{action}": first, static_detail: ordinary_get},
                {
                    "/v1/widgets/{id}/{action}": first,
                    static_detail: ordinary_get,
                    overlapping_dynamic: ordinary_get,
                },
                f"{overlapping_dynamic} would take precedence over POST /v1/widgets/{{id}}/{{action}}",
            ),
            (
                "three routes cover every length of the added catchall",
                old_routes,
                {**old_routes, catchall_path: ordinary_get},
                None,
            ),
            (
                "the added catchall takes previously protected trailing slashes",
                missing_slash,
                {**missing_slash, catchall_path: ordinary_get},
                f"{catchall_path} would take precedence over POST /{{*old}}",
            ),
            (
                "a one-segment blocker leaves deeper prefixed catchall retries exposed",
                archive_routes,
                {**archive_routes, prefix_catchall: ordinary_get},
                f"{prefix_catchall} would take precedence over POST {catchall_path}",
            ),
            (
                "retiring a catchall leaves every retry covered by a more specific GET",
                {catchall_path: deprecated, **fallback_routes},
                fallback_routes,
                None,
            ),
            (
                "retiring a static route exposes an ordinary POST handler",
                {static_path: deprecated, path: ordinary_post},
                {path: ordinary_post},
                f"removing POST {static_path} would route retries to {path}",
            ),
            (
                "retiring a static route exposes a different idempotent operation",
                {static_path: deprecated, path: changed},
                {path: changed},
                f"removing POST {static_path} would route retries to {path}",
            ),
            (
                "retaining a GET at the retired POST path prevents a fallback POST",
                {static_path: deprecated, path: ordinary_post},
                {static_path: ordinary_get, path: ordinary_post},
                None,
            ),
            (
                "retained prefixed GET takes precedence over a fallback POST",
                {static_path: deprecated, path: ordinary_post, "/v1/widgets/late{suffix}": ordinary_get},
                {path: ordinary_post, "/v1/widgets/late{suffix}": ordinary_get},
                None,
            ),
            (
                "retiring a disjoint route preserves the release policy",
                {static_path: deprecated, detail_path: ordinary_post},
                {detail_path: ordinary_post},
                None,
            ),
            (
                "unrelated operation",
                {path: {"post": operation(replay=False)}},
                {path: {"post": operation(header("X-Channel"), replay=False)}},
                None,
            ),
        )
        for name, before, after, expected in cases:
            with self.subTest(name=name):
                for file, paths in (("base.json", before), ("candidate.json", after)):
                    (self.root / file).write_text(
                        json.dumps({"paths": paths, "components": components}), encoding="utf-8"
                    )
                result = self.run_make(
                    "openapi-breaking", "BASE_OPENAPI=base.json", "OPENAPI_FILE=candidate.json", "OASDIFF=oasdiff"
                )
                if expected is None:
                    self.assertEqual(result.returncode, 0, result.stderr)
                else:
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertIn("POST /", result.stderr)
                    self.assertIn(expected, result.stderr)


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
