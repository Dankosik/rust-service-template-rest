#!/usr/bin/env python3
"""Unit tests for scripts/image/inventory.py and scripts/image/scan.sh."""

from __future__ import annotations

import copy
import hashlib
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "scripts/template"))
from state import TEMPLATE_REPOSITORY

IMAGE_CONFIG = b'{"architecture":"amd64","os":"linux"}'
IMAGE_ID = "sha256:" + hashlib.sha256(IMAGE_CONFIG).hexdigest()
DESCRIPTOR_ID = "sha256:" + "a" * 64


def runtime_result(path: str, name: str, version: str) -> dict:
    # Shared packages deliberately occur in multiple application graphs. A
    # flattened encoder could preserve package names while losing these edges.
    return {
        "Target": path.lstrip("/"),
        "Class": "lang-pkgs",
        "Type": "rustbinary",
        "Packages": [
            {
                "ID": f"{name}@{version}",
                "Name": name,
                "Version": version,
                "Relationship": "root",
                "DependsOn": ["shared@1.2.3", "adapter@4.5.6"],
            },
            {"ID": "shared@1.2.3", "Name": "shared", "Version": "1.2.3"},
            {"ID": "adapter@4.5.6", "Name": "adapter", "Version": "4.5.6", "DependsOn": ["shared@1.2.3"]},
        ],
    }


class Inventory(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="inventory-test-")))
        for relative in (
            "scripts/image/inventory.py",
            "scripts/image/scan.sh",
            "scripts/verify/lock.py",
            "scripts/template/state.py",
            "tools/versions.env",
        ):
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, destination)
        self.select(database="postgres", jobs="postgres", messaging="nats-jetstream")

    def select(self, *, database: str, jobs: str, messaging: str, extra_roles: bool = False) -> None:
        self.paths = ["/service"]
        if database == "postgres":
            self.paths.append("/migrate")
        if jobs == "postgres" or messaging == "nats-jetstream":
            self.paths.append("/jobs-worker")
        extension = self.root / "scripts/service/runtime-binaries.json"
        if extra_roles:
            self.paths.extend(["/batch", "/migrate-batch"])
            extension.parent.mkdir(parents=True, exist_ok=True)
            extension.write_text(
                json.dumps(
                    {
                        "/batch": {"crate": "batch", "source": "src/main.rs"},
                        "/migrate-batch": {"crate": "migrate", "source": "src/bin/migrate-batch.rs"},
                    }
                ),
                encoding="utf-8",
            )
        elif extension.exists():
            extension.unlink()
        identity = {
            "service_name": "orders-api",
            "repository": "https://github.com/example/orders-api",
            "description": "Orders API",
            "codeowner": "@example/platform",
        }
        profiles = {
            "database": database,
            "jobs": jobs,
            "messaging": messaging,
            "agent_harness": "core",
            "authn": "none",
            "outbound_http": "none",
            "http_idempotency": "none",
            "webhooks": "none",
            "inbound_webhooks": "none",
        }
        (self.root / "template.lock").write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "state": "complete",
                    "identity": identity,
                    "profiles": profiles,
                    "source": {
                        "repository": TEMPLATE_REPOSITORY,
                        "checkout_revision": "b" * 40,
                        "provenance": "local-checkout",
                    },
                }
            ),
            encoding="utf-8",
        )
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.9.0"\n', encoding="utf-8")
        stages = ["ARG SERVICE_PACKAGE=orders-api\nFROM builder AS source\n"]
        copies = []
        self.report = {
            "SchemaVersion": 2,
            "ArtifactName": IMAGE_ID,
            "ArtifactType": "container_image",
            "Metadata": {"ImageID": IMAGE_ID, "ImageConfig": {"config": {}}, "OS": {"Family": "debian", "Name": "13"}},
            "Results": [],
        }
        for path in self.paths:
            directory = "migrate" if path == "/migrate-batch" else path[1:]
            package = "orders-api" if path == "/service" else "batch-worker" if path == "/batch" else directory
            binary = "batch" if path == "/batch" else "migrate-batch" if path == "/migrate-batch" else package
            version = "2.3.4" if directory == "migrate" else "0.9.0"
            manifest = self.root / "crates" / directory / "Cargo.toml"
            manifest.parent.mkdir(parents=True, exist_ok=True)
            version_line = 'version = "2.3.4"' if directory == "migrate" else "version.workspace = true"
            if path == "/migrate-batch":
                manifest.write_text(
                    manifest.read_text(encoding="utf-8")
                    + '[[bin]]\nname = "migrate-batch"\npath = "src/bin/migrate-batch.rs"\n',
                    encoding="utf-8",
                )
            else:
                manifest.write_text(
                    f'[package]\nname = "{package}"\n{version_line}\n[[bin]]\nname = "{binary}"\npath = "src/main.rs"\n',
                    encoding="utf-8",
                )
            selection = (
                '-p "${SERVICE_PACKAGE}" --bin "${SERVICE_BIN}"'
                if path == "/service"
                else f"-p {package} --bin {binary}"
            )
            stages.append(
                f"FROM source AS {path[1:]}-build\n"
                f"ARG SERVICE_BIN=orders-api\n"
                f"RUN cargo auditable build --release --locked {selection} && cp target/release/{binary} /out/{binary}\n"
            )
            copies.append(f"COPY --from={path[1:]}-build /out/{binary} {path}\n")
            self.report["Results"].append(runtime_result(path, package, version))
        self.report["Results"].append(
            {
                "Target": "image.tar (debian 13)",
                "Class": "os-pkgs",
                "Type": "debian",
                "Packages": [{"Name": "libc6", "Version": "2.41-12+deb13u1", "SrcName": "glibc"}],
            }
        )
        dockerfile = self.root / "build/docker/Dockerfile"
        dockerfile.parent.mkdir(parents=True, exist_ok=True)
        dockerfile.write_text("".join(stages) + "FROM distroless\n" + "".join(copies), encoding="utf-8")
        self.report_path = self.root / "scan.json"

    def run_check(self, expected: str | None = None) -> subprocess.CompletedProcess:
        self.report_path.write_text(json.dumps(self.report), encoding="utf-8")
        result = subprocess.run(
            [
                sys.executable,
                str(self.root / "scripts/image/inventory.py"),
                "--repo",
                str(self.root),
                "--report",
                str(self.report_path),
                "--image-id",
                IMAGE_ID,
            ],
            text=True,
            capture_output=True,
        )
        if expected is None:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn(expected, result.stderr)
        return result

    def test_four_binary_selections_and_current_manifest_identity(self) -> None:
        for database, jobs, messaging in (
            ("none", "none", "none"),
            ("postgres", "none", "none"),
            ("none", "none", "nats-jetstream"),
            ("postgres", "postgres", "nats-jetstream"),
        ):
            with self.subTest(database=database, jobs=jobs, messaging=messaging):
                self.select(database=database, jobs=jobs, messaging=messaging)
                self.run_check()

    def test_each_retained_binary_is_required_despite_other_valid_graphs(self) -> None:
        original = copy.deepcopy(self.report)
        for result in original["Results"][:-1]:
            with self.subTest(target=result["Target"]):
                self.report = copy.deepcopy(original)
                self.report["Results"].remove(result)
                self.run_check(f'/{result["Target"]}: missing Rust inventory')

    def test_debian_inventory_is_required_despite_valid_rust_graphs(self) -> None:
        original = copy.deepcopy(self.report)
        for name, mutate, refusal in (
            ("wrong OS", lambda report: report["Metadata"]["OS"].update(Family="ubuntu"), "Debian OS"),
            ("missing result", lambda report: report["Results"].pop(), "Debian OS inventory"),
            (
                "wrong result class",
                lambda report: report["Results"][-1].update(Class="lang-pkgs"),
                "Debian OS inventory",
            ),
            (
                "missing libc6",
                lambda report: report["Results"][-1]["Packages"][0].update(Name="libssl3t64"),
                "libc6",
            ),
            (
                "malformed package",
                lambda report: report["Results"][-1]["Packages"][0].pop("Version"),
                "Debian package version",
            ),
        ):
            with self.subTest(name=name):
                self.report = copy.deepcopy(original)
                mutate(self.report)
                self.run_check(refusal)

    def test_additional_processes_require_their_own_runtime_inventory(self) -> None:
        self.select(database="postgres", jobs="postgres", messaging="nats-jetstream", extra_roles=True)
        self.run_check()
        original = copy.deepcopy(self.report)
        for target in ("batch", "migrate-batch"):
            with self.subTest(target=target):
                self.report = copy.deepcopy(original)
                self.report["Results"] = [item for item in self.report["Results"] if item["Target"] != target]
                self.run_check(f"/{target}: missing Rust inventory")

    def test_native_graph_refusals(self) -> None:
        mutations = [
            (
                "root-only",
                lambda result: result.update(Packages=[{**result["Packages"][0], "DependsOn": []}]),
                "root-only",
            ),
            ("dangling", lambda result: result["Packages"][0]["DependsOn"].append("absent@9"), "dangling"),
            ("disconnected", lambda result: result["Packages"][0].update(DependsOn=["shared@1.2.3"]), "disconnected"),
            ("wrong-root", lambda result: result["Packages"][0].update(Name="another-service"), "root identity"),
            ("wrong-version", lambda result: result["Packages"][0].update(Version="0.8.0"), "root identity"),
            ("no-root", lambda result: result["Packages"][0].pop("Relationship"), "exactly one runtime root"),
            ("two-roots", lambda result: result["Packages"][1].update(Relationship="root"), "exactly one runtime root"),
            (
                "duplicate-id",
                lambda result: result["Packages"].append(copy.deepcopy(result["Packages"][1])),
                "ambiguous package",
            ),
            ("missing-name", lambda result: result["Packages"][1].pop("Name"), "package name"),
            ("empty-packages", lambda result: result.update(Packages=[]), "missing runtime packages"),
            (
                "malformed-edges",
                lambda result: result["Packages"][1].update(DependsOn="shared@1.2.3"),
                "malformed dependencies",
            ),
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
        original = dockerfile.read_text(encoding="utf-8")
        dockerfile.write_text(
            original.replace("ARG SERVICE_PACKAGE=orders-api", "ARG SERVICE_PACKAGE=wrong-api"),
            encoding="utf-8",
        )
        self.run_check("disagree with selected manifest")
        dockerfile.write_text(
            original + "\n# trailing backslash in comment \\\nCOPY --from=missing /out/rogue /service\n",
            encoding="utf-8",
        )
        self.run_check("ambiguous output /service")

    def test_pgo_wrapper_must_build_the_selected_auditable_binary(self) -> None:
        dockerfile = self.root / "build/docker/Dockerfile"
        original = dockerfile.read_text(encoding="utf-8")
        direct = 'RUN cargo auditable build --release --locked -p "${SERVICE_PACKAGE}" --bin "${SERVICE_BIN}"'
        self.assertEqual(original.count(direct), 1)
        wrapper = (
            'RUN build() { cargo auditable build --release --locked -p "${SERVICE_PACKAGE}" '
            '--bin "${SERVICE_BIN}" "$@"; } && case "${PGO}" in none) ;; '
            'train) build --target-dir /pgo/target ;; *) exit 1 ;; esac && build'
        )
        wrapped = original.replace(direct, wrapper)
        dockerfile.write_text(wrapped, encoding="utf-8")
        self.run_check()
        for old, new, refusal in (
            ('"$@"', '"$*"', "unsupported auditable build wrapper"),
            ('"$@"', "'$@'", "unsupported auditable build wrapper"),
            ('"$@"', "$@", "unsupported auditable build wrapper"),
            ("esac && build &&", "esac && true &&", "unsupported auditable build wrapper"),
            (
                "train) build --target-dir /pgo/target ;;",
                "train) true;case x in x) true ;; esac && build && true ;;",
                "unsupported auditable build wrapper",
            ),
            ('--bin "${SERVICE_BIN}"', "--bin wrong-binary", "does not select an admitted auditable binary output"),
        ):
            with self.subTest(mutation=new):
                self.assertEqual(wrapped.count(old), 1)
                dockerfile.write_text(wrapped.replace(old, new), encoding="utf-8")
                self.run_check(refusal)

    def install_docker_fixture(self) -> None:
        binary = self.root / "bin/docker"
        binary.parent.mkdir()
        binary.write_text(
            '''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys, uuid
args = sys.argv[1:]
root = pathlib.Path(os.environ["FIXTURE_ROOT"])
with (root / "calls.jsonl").open("a") as stream: stream.write(json.dumps(args) + "\\n")
containers = root / "containers"
containers.mkdir(exist_ok=True)
def create_container():
    identity = uuid.uuid4().hex + uuid.uuid4().hex
    name = args[args.index("--name") + 1] if "--name" in args else identity[:12]
    state = {"Id": identity, "Name": "/" + name,
             "State": {"Running": args[0] == "run", "Restarting": False}}
    (containers / identity).write_text(json.dumps(state))
    return identity
if args[:2] == ["context", "show"]: print("inventory-fixture")
elif args[0] == "info": print("inventory-fixture-daemon")
elif args[0] == "ps":
    name = args[args.index("--filter") + 1].removeprefix("name=^/").removesuffix("$")
    for path in containers.iterdir():
        state = json.loads(path.read_text())
        if state["Name"] == "/" + name: print(state["Id"])
elif args[0] == "inspect":
    print(json.dumps([json.loads((containers / identity).read_text()) for identity in args[1:]]))
elif args[:2] == ["container", "inspect"]:
    assert (containers / args[-1]).is_file()
    print(os.environ.get("FIXTURE_MOUNTS", "[]"))
elif args[0] == "export":
    assert (containers / args[1]).is_file()
    with (root / "rootfs.tar").open("rb") as archive:
        shutil.copyfileobj(archive, sys.stdout.buffer)
    sys.exit(int(os.environ.get("EXPORT_EXIT", "0")))
elif args[:2] == ["image", "inspect"]:
    print(os.environ["FIXTURE_IMAGE"], os.environ["FIXTURE_ID_KIND"], "linux/amd64")
elif args[:2] == ["image", "save"]:
    assert args[-1] == os.environ["FIXTURE_IMAGE"]
    with (root / "image.tar").open("rb") as archive:
        shutil.copyfileobj(archive, sys.stdout.buffer)
elif args[0] == "create":
    assert args[-1] == os.environ["FIXTURE_IMAGE"]
    print(create_container())
elif args[0] == "cp":
    path = args[1].split(":", 1)[1]
    if path not in json.loads(os.environ["FIXTURE_BINARIES"]): sys.exit(1)
    dest = pathlib.Path(args[2]); dest.write_bytes(b"executable"); dest.chmod(0o755)
elif args[0] == "rm": (containers / args[-1]).unlink(missing_ok=True)
elif args[0] == "run":
    identity = create_container()
    status = 0
    if "--download-db-only" in args:
        pass
    elif "convert" not in args:
        archive = pathlib.Path(next(arg.removesuffix(":/input/image.tar:ro") for arg in args
                                    if arg.endswith(":/input/image.tar:ro")))
        assert archive.read_bytes() == (root / "image.tar").read_bytes()
        with (root / "scan.json").open("rb") as report:
            shutil.copyfileobj(report, sys.stdout.buffer)
    elif "cyclonedx" in args:
        if os.environ.get("FAIL_CONVERT"): status = 1
        else: print("converted-report", end="")
    else:
        for result in json.load(sys.stdin)["Results"]:
            for vuln in result.get("Vulnerabilities", []): print(vuln["VulnerabilityID"])
    if "--rm" in args: (containers / identity).unlink()
    sys.exit(status)
else: sys.exit(2)
''',
            encoding="utf-8",
        )
        binary.chmod(0o755)

    def run_scan(
        self,
        mode: str,
        *,
        binaries: list[str] | None = None,
        archive_image_count: int = 1,
        **extra: str,
    ) -> subprocess.CompletedProcess:
        self.report_path.write_text(json.dumps(self.report), encoding="utf-8")
        binaries = self.paths if binaries is None else binaries
        with tarfile.open(self.root / "rootfs.tar", "w") as archive:
            for path in binaries:
                info = tarfile.TarInfo(path.lstrip("/"))
                info.mode = 0o755
                info.size = len(b"executable")
                archive.addfile(info, io.BytesIO(b"executable"))
        config_path = "blobs/sha256/" + IMAGE_ID.removeprefix("sha256:")
        manifest = json.dumps([{"Config": config_path, "Layers": []}] * archive_image_count).encode()
        with tarfile.open(self.root / "image.tar", "w") as archive:
            for name, data in (("manifest.json", manifest), (config_path, IMAGE_CONFIG)):
                info = tarfile.TarInfo(name)
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
        environment = {key: value for key, value in os.environ.items() if not key.startswith("VALIDATION_LOCK_")}
        return subprocess.run(
            ["bash", "scripts/image/scan.sh", mode, "mutable:tag", "result.cdx.json"],
            cwd=self.root,
            capture_output=True,
            text=True,
            env={
                **environment,
                "VALIDATION_LOCK_DIR": str(self.root / "validation.lock"),
                "PATH": f"{self.root / 'bin'}:{os.environ['PATH']}",
                "FIXTURE_ROOT": str(self.root),
                "FIXTURE_IMAGE": IMAGE_ID,
                "FIXTURE_ID_KIND": "config",
                "FIXTURE_BINARIES": json.dumps(binaries),
                "DOCKER_DEFAULT_PLATFORM": "",
                **extra,
            },
        )

    def test_missing_and_pruned_files_refuse_before_scanning(self) -> None:
        self.install_docker_fixture()
        for path in self.paths:
            with self.subTest(path=path):
                result = self.run_scan("sbom", binaries=[entry for entry in self.paths if entry != path])
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"{path}: could not copy retained binary", result.stderr)
        self.select(database="none", jobs="none", messaging="none")
        result = self.run_scan("sbom", binaries=["/service", "/jobs-worker"])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("/jobs-worker: pruned binary is present", result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text(encoding="utf-8").splitlines()]
        self.assertFalse(any(call[0] == "run" for call in calls))

    def test_pruned_proof_requires_a_successful_export_and_unmasked_paths(self) -> None:
        self.install_docker_fixture()
        self.select(database="none", jobs="none", messaging="none")
        unrelated = json.dumps([{"Destination": "/var/data"}])
        result = self.run_scan("filesystem", FIXTURE_MOUNTS=unrelated)
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.run_scan("filesystem", EXPORT_EXIT="23")
        self.assertNotEqual(result.returncode, 0)
        for mount in ("/", "/jobs-worker", "/migrate/cache"):
            with self.subTest(mount=mount):
                result = self.run_scan("filesystem", FIXTURE_MOUNTS=json.dumps([{"Destination": mount}]))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("mount prevents", result.stderr)

    def test_incomplete_archives_and_pruned_entries_refuse(self) -> None:
        mounts = self.root / "mounts.json"
        mounts.write_text("[]", encoding="utf-8")
        command = [
            sys.executable,
            str(self.root / "scripts/image/inventory.py"),
            "--rootfs-mounts",
            str(mounts),
            "--pruned",
            "/migrate",
        ]
        service = tarfile.TarInfo("service")
        service.size = tarfile.BLOCKSIZE
        header = service.tobuf()
        member = header + b"s" * service.size
        zero = bytes(tarfile.BLOCKSIZE)
        archives = {
            "truncated data": header + b"s",
            "partial header": member + b"h",
            "damaged header": member + b"h" * tarfile.BLOCKSIZE + zero * 2,
            "missing terminator": member,
            "single end block": member + zero,
            "data after terminator": member + zero * 2 + member,
            "partial padding block": member + zero * 2 + b"\x00",
        }
        for name, kind in (
            ("./migrate", tarfile.SYMTYPE),
            ("migrate/child", tarfile.REGTYPE),
            ("/migrate/", tarfile.DIRTYPE),
        ):
            entry = tarfile.TarInfo(name)
            entry.type = kind
            archives[name] = member + entry.tobuf() + zero * 2
        for case, contents in archives.items():
            with self.subTest(case=case):
                result = subprocess.run(command, input=contents, capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(b"runtime-image-inventory:", result.stderr)

    def test_scan_freezes_image_and_converts_only_admitted_reports(self) -> None:
        self.install_docker_fixture()
        result = self.run_scan("sbom")
        self.assertEqual(result.returncode, 0, result.stderr)
        output = self.root / "result.cdx.json"
        self.assertEqual(output.read_text(encoding="utf-8"), "converted-report")
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text(encoding="utf-8").splitlines()]
        self.assertEqual(sum(call[:2] == ["image", "inspect"] for call in calls), 1)
        self.assertIn(["image", "save", IMAGE_ID], calls)
        self.assertFalse(any(call[0] == "export" for call in calls))
        trivy = [call for call in calls if call[0] == "run"]
        self.assertFalse(any("docker.sock" in arg or "DOCKER_HOST" in arg for call in trivy for arg in call))
        refresh = next(call for call in trivy if "--download-db-only" in call)
        self.assertFalse(any("image.tar" in arg for arg in refresh))
        scan = next(call for call in trivy if "--input" in call)
        self.assertEqual(scan[scan.index("--network") + 1], "none")
        self.assertIn("--list-all-pkgs=true", scan)
        self.assertNotIn("--severity", scan)
        self.assertNotIn("--ignore-unfixed", scan)
        output.write_text("previous-report", encoding="utf-8")
        self.report["Results"].pop(0)
        (self.root / "calls.jsonl").unlink()
        result = self.run_scan("sbom")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing Rust inventory", result.stderr)
        self.assertEqual(output.read_text(encoding="utf-8"), "previous-report")
        self.assertNotIn('"convert"', (self.root / "calls.jsonl").read_text(encoding="utf-8"))

    def test_descriptor_scan_compares_trivy_with_the_archived_platform_config(self) -> None:
        self.install_docker_fixture()
        descriptor_image = {"FIXTURE_IMAGE": DESCRIPTOR_ID, "FIXTURE_ID_KIND": "descriptor"}
        result = self.run_scan("sbom", **descriptor_image)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text(encoding="utf-8").splitlines()]
        self.assertIn(["image", "save", "--platform", "linux/amd64", DESCRIPTOR_ID], calls)
        create = next(call for call in calls if call[0] == "create")
        self.assertEqual(create[create.index("--platform") + 1], "linux/amd64")

        self.report["Metadata"]["ImageID"] = DESCRIPTOR_ID
        result = self.run_scan("security", **descriptor_image)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Trivy report does not identify the fixed image ID", result.stderr)
        result = self.run_scan("security", archive_image_count=2, **descriptor_image)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Docker archive: expected exactly one image", result.stderr)

    def test_conversion_failure_preserves_output(self) -> None:
        self.install_docker_fixture()
        output = self.root / "result.cdx.json"
        output.write_text("previous-report", encoding="utf-8")
        result = self.run_scan("sbom", FAIL_CONVERT="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(output.read_text(encoding="utf-8"), "previous-report")

    def test_security_displays_unfixed_findings_and_blocks_only_fixed_high_critical(self) -> None:
        self.install_docker_fixture()
        self.report["Results"][-1]["Vulnerabilities"] = [
            {"VulnerabilityID": "CVE-2026-1234", "Severity": "HIGH", "Status": "affected"},
            {"VulnerabilityID": "CVE-2026-1235", "Severity": "MEDIUM", "Status": "fixed", "FixedVersion": "2.42"},
        ]
        result = self.run_scan("security")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("CVE-2026-1234", result.stdout)
        self.report["Results"][0]["Vulnerabilities"] = [
            {"VulnerabilityID": "RUSTSEC-2026-0001", "Severity": "CRITICAL", "Status": "fixed", "FixedVersion": "1.2.4"}
        ]
        result = self.run_scan("security")
        self.assertEqual(result.returncode, 1)
        self.assertIn("CVE-2026-1234", result.stdout)
        self.assertIn("RUSTSEC-2026-0001", result.stdout)
        self.assertIn("1 fixed HIGH/CRITICAL vulnerabilities", result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text(encoding="utf-8").splitlines()]
        verdict = next(call for call in reversed(calls) if call[0] == "run" and "convert" in call)
        self.assertIn("HIGH,CRITICAL", verdict)
        scan = next(call for call in reversed(calls) if call[0] == "run" and "--input" in call)
        self.assertNotIn("--ignore-unfixed", scan)


if __name__ == "__main__":
    unittest.main()
