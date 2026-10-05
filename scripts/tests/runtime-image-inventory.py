#!/usr/bin/env python3
"""Exercise inventory refusal and report publication at their command boundaries.

The compact native report follows Trivy v0.74.0's Rust parser and language
application conversion (pkg/dependency/parser/rust/binary/parse.go and
pkg/fanal/analyzer/language/analyze.go). It represents normal runtime packages,
not proc-macro/build dependencies. --native-conversion additionally exercises
pinned Trivy itself, offline, without an image build or vulnerability DB.
"""
from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "scripts/lib"))
from template_state import TEMPLATE_REPOSITORY

IMAGE_ID = "sha256:" + "a" * 64
NATIVE = "--native-conversion" in sys.argv
if NATIVE:
    sys.argv.remove("--native-conversion")


def runtime_result(path: str, name: str, version: str) -> dict:
    # Shared packages deliberately occur in multiple application graphs. A
    # flattened encoder could preserve package names while losing these edges.
    return {
        "Target": path.lstrip("/"), "Class": "lang-pkgs", "Type": "rustbinary",
        "Packages": [
            {"ID": f"{name}@{version}", "Name": name, "Version": version,
             "Relationship": "root", "DependsOn": ["shared@1.2.3", "adapter@4.5.6"]},
            {"ID": "shared@1.2.3", "Name": "shared", "Version": "1.2.3"},
            {"ID": "adapter@4.5.6", "Name": "adapter", "Version": "4.5.6", "DependsOn": ["shared@1.2.3"]},
        ],
    }


class Inventory(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="inventory-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for relative in ("scripts/ci/runtime-image-inventory.py", "scripts/ci/runtime-image-scan.sh",
                         "scripts/lib/template_state.py", "tools/versions.env"):
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, destination)
        self.select(database="postgres", jobs="postgres", messaging="nats-jetstream")

    def select(self, *, database: str, jobs: str, messaging: str) -> None:
        self.paths = ["/service"]
        if database == "postgres":
            self.paths.append("/migrate")
        if jobs == "postgres" or messaging == "nats-jetstream":
            self.paths.append("/jobs-worker")
        identity = {"service_name": "orders-api", "repository": "https://github.com/example/orders-api",
                    "description": "Orders API", "codeowner": "@example/platform"}
        profiles = {"database": database, "jobs": jobs, "messaging": messaging, "agent_harness": "core",
                    "authn": "none", "outbound_http": "none", "http_idempotency": "none",
                    "webhooks": "none", "inbound_webhooks": "none"}
        (self.root / "template.lock").write_text(json.dumps({"schema_version": 1, "state": "complete",
            "identity": identity, "profiles": profiles, "source": {
                "repository": TEMPLATE_REPOSITORY,
                "checkout_revision": "b" * 40, "provenance": "local-checkout"}}))
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.9.0"\n')
        stages = ['ARG SERVICE_PACKAGE=orders-api\nFROM builder AS source\n']
        copies = []
        self.report = {"SchemaVersion": 2, "ArtifactName": IMAGE_ID, "ArtifactType": "container_image",
                       "Metadata": {"ImageID": IMAGE_ID, "ImageConfig": {"config": {}}}, "Results": []}
        for path in self.paths:
            directory = path[1:]
            package = "orders-api" if path == "/service" else directory
            version = "2.3.4" if path == "/migrate" else "0.9.0"
            manifest = self.root / "crates" / directory / "Cargo.toml"
            manifest.parent.mkdir(parents=True, exist_ok=True)
            version_line = 'version = "2.3.4"' if path == "/migrate" else 'version.workspace = true'
            manifest.write_text(f'[package]\nname = "{package}"\n{version_line}\n[[bin]]\nname = "{package}"\npath = "src/main.rs"\n')
            selection = '-p "${SERVICE_PACKAGE}" --bin "${SERVICE_BIN}"' if path == "/service" else f'-p {package} --bin {package}'
            stages.append(f'FROM source AS {directory}-build\nARG SERVICE_BIN=orders-api\nRUN cargo auditable build --release --locked {selection} && cp target/release/{package} /out/{package}\n')
            copies.append(f'COPY --from={directory}-build /out/{package} {path}\n')
            self.report["Results"].append(runtime_result(path, package, version))
        dockerfile = self.root / "build/docker/Dockerfile"
        dockerfile.parent.mkdir(parents=True, exist_ok=True)
        dockerfile.write_text("".join(stages) + 'FROM distroless\n' + "".join(copies))
        self.report_path = self.root / "scan.json"

    def run_check(self, expected: str | None = None) -> subprocess.CompletedProcess:
        self.report_path.write_text(json.dumps(self.report))
        result = subprocess.run([sys.executable, str(self.root / "scripts/ci/runtime-image-inventory.py"),
                                 "--repo", str(self.root), "--report", str(self.report_path), "--image-id", IMAGE_ID],
                                text=True, capture_output=True)
        if expected is None:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn(expected, result.stderr)
        return result

    def test_four_binary_selections_and_current_manifest_identity(self) -> None:
        for database, jobs, messaging in (("none", "none", "none"), ("postgres", "none", "none"),
                                          ("none", "none", "nats-jetstream"), ("postgres", "postgres", "nats-jetstream")):
            with self.subTest(database=database, jobs=jobs, messaging=messaging):
                self.select(database=database, jobs=jobs, messaging=messaging)
                self.run_check()

    def test_each_retained_binary_is_required_despite_other_valid_graphs(self) -> None:
        original = copy.deepcopy(self.report)
        for result in original["Results"]:
            with self.subTest(target=result["Target"]):
                self.report = copy.deepcopy(original)
                self.report["Results"].remove(result)
                self.run_check(f'/{result["Target"]}: missing Rust inventory')

    def test_native_graph_refusals(self) -> None:
        mutations = [
            ("root-only", lambda result: result.update(Packages=[{**result["Packages"][0], "DependsOn": []}]), "root-only"),
            ("dangling", lambda result: result["Packages"][0]["DependsOn"].append("absent@9"), "dangling"),
            ("disconnected", lambda result: result["Packages"][0].update(DependsOn=["shared@1.2.3"]), "disconnected"),
            ("wrong-root", lambda result: result["Packages"][0].update(Name="another-service"), "root identity"),
            ("wrong-version", lambda result: result["Packages"][0].update(Version="0.8.0"), "root identity"),
            ("no-root", lambda result: result["Packages"][0].pop("Relationship"), "exactly one runtime root"),
            ("two-roots", lambda result: result["Packages"][1].update(Relationship="root"), "exactly one runtime root"),
            ("duplicate-id", lambda result: result["Packages"].append(copy.deepcopy(result["Packages"][1])), "ambiguous package"),
            ("missing-name", lambda result: result["Packages"][1].pop("Name"), "package name"),
            ("empty-packages", lambda result: result.update(Packages=[]), "missing runtime packages"),
            ("malformed-edges", lambda result: result["Packages"][1].update(DependsOn="shared@1.2.3"), "malformed dependencies"),
            ("wrong-class", lambda result: result.update(Class="os-pkgs"), "incorrectly classified"),
        ]
        original = copy.deepcopy(self.report)
        for name, mutate, expected in mutations:
            with self.subTest(name=name):
                self.report = copy.deepcopy(original)
                mutate(self.report["Results"][0])
                self.run_check(expected)

    def test_alias_targets_and_duplicate_application(self) -> None:
        self.report["Results"][0]["Target"] = "./service"
        self.run_check()
        self.report["Results"].append({**self.report["Results"][0], "Target": "/service"})
        self.run_check("ambiguous")

    def test_cycle_traversal_terminates(self) -> None:
        self.report["Results"][0]["Packages"][1]["DependsOn"] = ["adapter@4.5.6"]
        self.run_check()

    def test_pruned_native_inventory_and_wrong_image_are_refused(self) -> None:
        self.select(database="none", jobs="none", messaging="none")
        self.report["Results"].append(runtime_result("/migrate", "migrate", "2.3.4"))
        self.run_check("pruned binary")
        self.report["Results"].pop()
        self.report["Metadata"]["ImageID"] = "sha256:another-image"
        self.run_check("fixed image ID")

    def test_docker_selection_must_agree_with_manifests(self) -> None:
        dockerfile = self.root / "build/docker/Dockerfile"
        dockerfile.write_text(dockerfile.read_text().replace("ARG SERVICE_PACKAGE=orders-api", "ARG SERVICE_PACKAGE=wrong-api"))
        self.run_check("disagree with selected manifest")

    def install_docker_fixture(self) -> None:
        binary = self.root / "bin/docker"
        binary.parent.mkdir()
        binary.write_text('''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
args = sys.argv[1:]
root = pathlib.Path(os.environ["FIXTURE_ROOT"])
with (root / "calls.jsonl").open("a") as stream: stream.write(json.dumps(args) + "\\n")
if args[:2] == ["image", "inspect"]: print(os.environ["FIXTURE_IMAGE"])
elif args[0] == "create":
    assert args[-1] == os.environ["FIXTURE_IMAGE"]
    print("fixture-container")
elif args[0] == "cp":
    path = args[1].split(":", 1)[1]
    if path not in json.loads(os.environ["FIXTURE_BINARIES"]): sys.exit(1)
    dest = pathlib.Path(args[2]); dest.write_bytes(b"executable"); dest.chmod(0o755)
elif args[0] == "rm": pass
elif args[0] == "run":
    mount = next(arg[:-6] for arg in args if arg.endswith(":/work"))
    work = pathlib.Path(mount)
    if "convert" not in args:
        assert args[-1] == os.environ["FIXTURE_IMAGE"]
        shutil.copyfile(root / "scan.json", work / "native.json")
    elif "cyclonedx" in args:
        # A converter stub checks orchestration only; NativeConversion below
        # owns the actual converter's format/graph contract.
        if os.environ.get("FAIL_CONVERT"): sys.exit(1)
        (work / "sbom.cdx.json").write_text("converted-report")
    else: sys.exit(int(os.environ.get("SECURITY_EXIT", "0")))
else: sys.exit(2)
''')
        binary.chmod(0o755)

    def run_scan(self, mode: str, *, binaries: list[str] | None = None, **extra: str) -> subprocess.CompletedProcess:
        self.report_path.write_text(json.dumps(self.report))
        return subprocess.run(["bash", "scripts/ci/runtime-image-scan.sh", mode, "mutable:tag", "result.cdx.json"],
                              cwd=self.root, capture_output=True, text=True,
                              env={**os.environ, "PATH": f"{self.root / 'bin'}:{os.environ['PATH']}",
                                   "FIXTURE_ROOT": str(self.root), "FIXTURE_IMAGE": IMAGE_ID,
                                   "FIXTURE_BINARIES": json.dumps(self.paths if binaries is None else binaries), **extra})

    def test_missing_and_pruned_files_refuse_before_scanning(self) -> None:
        self.install_docker_fixture()
        for path in self.paths:
            with self.subTest(path=path):
                result = self.run_scan("sbom", binaries=[entry for entry in self.paths if entry != path])
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"{path}: retained binary is absent", result.stderr)
        self.select(database="none", jobs="none", messaging="none")
        result = self.run_scan("sbom", binaries=["/service", "/jobs-worker"])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("/jobs-worker: pruned binary is present", result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertFalse(any(call[0] == "run" for call in calls))

    def test_scan_freezes_image_and_converts_only_admitted_reports(self) -> None:
        self.install_docker_fixture()
        result = self.run_scan("sbom")
        self.assertEqual(result.returncode, 0, result.stderr)
        output = self.root / "result.cdx.json"
        self.assertEqual(output.read_text(), "converted-report")
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertEqual(sum(call[:2] == ["image", "inspect"] for call in calls), 1)
        scan = next(call for call in calls if call[0] == "run" and "convert" not in call)
        self.assertIn("--list-all-pkgs=true", scan)
        self.assertEqual(scan[-1], IMAGE_ID)
        self.assertNotIn("--severity", scan)
        self.assertNotIn("--ignore-unfixed", scan)
        output.write_text("previous-report")
        self.report["Results"].pop()
        (self.root / "calls.jsonl").unlink()
        result = self.run_scan("sbom")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing Rust inventory", result.stderr)
        self.assertEqual(output.read_text(), "previous-report")
        self.assertNotIn('"convert"', (self.root / "calls.jsonl").read_text())

    def test_conversion_failure_preserves_output_and_security_verdict_propagates(self) -> None:
        self.install_docker_fixture()
        output = self.root / "result.cdx.json"
        output.write_text("previous-report")
        result = self.run_scan("sbom", FAIL_CONVERT="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(output.read_text(), "previous-report")
        result = self.run_scan("security", SECURITY_EXIT="1")
        self.assertEqual(result.returncode, 1)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        verdict = calls[-1]
        self.assertIn("HIGH,CRITICAL", verdict)
        self.assertNotIn("--ignore-unfixed", verdict)
        scan = next(call for call in reversed(calls) if call[0] == "run" and "convert" not in call)
        self.assertIn("--ignore-unfixed", scan)
        self.assertIn("--list-all-pkgs=true", scan)
        self.assertNotIn("--severity", scan)
        self.assertEqual(verdict[-1], "/work/native.json")

    @unittest.skipUnless(NATIVE, "pinned native conversion is selected explicitly at final validation")
    def test_pinned_security_command_accepts_native_policy_report(self) -> None:
        # Record the real helper's invocation through the existing Docker fixture,
        # then replay its converter command with pinned native Trivy. This fails
        # if the helper adds a flag that the converter does not support.
        self.install_docker_fixture()
        result = self.run_scan("security")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        scan = next(call for call in calls if call[0] == "run" and "convert" not in call)
        command = next(call for call in calls if "convert" in call)
        # The helper has cleaned its private directory; replace only the bind
        # source. All executable/flag arguments come from the production command.
        command = [f"{self.root}:/work" if argument.endswith(":/work") else argument for argument in command]
        packages = copy.deepcopy(self.report["Results"][0]["Packages"])
        for severity, status, fixed_version, expected_exit in (
            ("HIGH", "fixed", "1.2.4", 1),
            ("CRITICAL", "fixed", "1.2.4", 1),
            ("MEDIUM", "fixed", "1.2.4", 0),
            # Negative control: convert cannot implement ignore-unfixed. The
            # image scanner must apply it before admission; native FilterResult
            # only changes vulnerabilities, leaving this package graph intact.
            ("HIGH", "affected", "", 1),
        ):
            with self.subTest(severity=severity, status=status):
                self.report["Results"][0]["Vulnerabilities"] = [{
                    "VulnerabilityID": "CVE-2026-10001", "PkgID": "shared@1.2.3",
                    "PkgName": "shared", "InstalledVersion": "1.2.3",
                    "FixedVersion": fixed_version, "Status": status, "Severity": severity,
                }]
                self.run_check()
                native = self.root / "native.json"
                native.write_bytes(self.report_path.read_bytes())
                result = subprocess.run(["docker", *command], capture_output=True, text=True)
                self.assertNotIn("unknown flag", result.stderr)
                self.assertEqual(result.returncode, expected_exit, result.stderr)
                # Conversion reads, never narrows, the admitted inventory input.
                self.assertEqual(json.loads(native.read_text())["Results"][0]["Packages"], packages)
                if expected_exit:
                    self.assertIn("CVE-2026-10001", result.stdout)
                else:
                    self.assertNotIn("CVE-2026-10001", result.stdout)
        self.assertIn("--ignore-unfixed", scan)
        self.assertIn("--list-all-pkgs=true", scan)
        self.assertNotIn("--severity", scan)

    @unittest.skipUnless(NATIVE, "pinned native conversion is selected explicitly at final validation")
    def test_pinned_native_conversion_preserves_each_application_graph(self) -> None:
        self.run_check()
        image_line = next(line for line in (ROOT / "tools/versions.env").read_text().splitlines() if line.startswith("TRIVY_IMAGE="))
        result = subprocess.run(["docker", "run", "--rm", "--network", "none", "-v", f"{self.root}:/work",
                                 image_line.split("=", 1)[1], "convert", "--format", "cyclonedx",
                                 "--output", "/work/native.cdx.json", "/work/scan.json"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        bom = json.loads((self.root / "native.cdx.json").read_text())
        components = {item["bom-ref"]: item for item in bom["components"]}
        edges = {item["ref"]: item.get("dependsOn", []) for item in bom["dependencies"]}
        for path in self.paths:
            applications = [ref for ref, item in components.items() if item["type"] == "application" and item["name"].lstrip("/") == path.lstrip("/")]
            self.assertEqual(len(applications), 1, path)
            pending, visited = applications.copy(), set()
            while pending:
                ref = pending.pop()
                if ref not in visited:
                    visited.add(ref)
                    pending.extend(edges.get(ref, []))
            libraries = {(components[ref]["name"], components[ref].get("version")) for ref in visited if components[ref]["type"] == "library"}
            self.assertIn(("shared", "1.2.3"), libraries, path)
            self.assertIn(("adapter", "4.5.6"), libraries, path)
            self.assertIn(("orders-api" if path == "/service" else path[1:], "2.3.4" if path == "/migrate" else "0.9.0"), libraries, path)


if __name__ == "__main__":
    unittest.main()
