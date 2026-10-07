#!/usr/bin/env python3
"""Admit each selected binary's native Trivy runtime graph before reporting."""

from __future__ import annotations

import argparse
from pathlib import Path
import posixpath
import re
import shlex
import sys
import tomllib

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from template_state import Refusal, parse_json_bytes, selected_jobs, selected_messaging, selected_profiles

ENTRYPOINTS = {"/service": "service", "/migrate": "migrate", "/jobs-worker": "jobs-worker"}


def required_string(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise Refusal(f"{label}: expected a nonempty string")
    return value


def expected_binaries(root: Path) -> dict[str, tuple[str, str, str]]:
    """Current manifests and selection own identities, independently of the scan."""
    workspace = tomllib.loads((root / "Cargo.toml").read_text())
    selected = {"/service"}
    if selected_profiles(root)[0] == "postgres":
        selected.add("/migrate")
    if selected_jobs(root) == "postgres" or selected_messaging(root) == "nats-jetstream":
        selected.add("/jobs-worker")
    expected = {}
    for target, directory in ENTRYPOINTS.items():
        if target not in selected:
            continue
        manifest_path = root / "crates" / directory / "Cargo.toml"
        manifest = tomllib.loads(manifest_path.read_text())
        package = manifest.get("package", {})
        name = required_string(package.get("name"), f"{manifest_path}: package name")
        version = package.get("version")
        if version == {"workspace": True}:
            version = workspace.get("workspace", {}).get("package", {}).get("version")
        version = required_string(version, f"{manifest_path}: package version")
        binaries = [binary for binary in manifest.get("bin", []) if binary.get("path") == "src/main.rs"]
        if len(binaries) != 1:
            raise Refusal(f"{manifest_path}: expected one explicit src/main.rs binary")
        binary = required_string(binaries[0].get("name"), f"{manifest_path}: binary name")
        expected[target] = (name, version, binary)
    check_outputs(root, expected)
    return expected


def check_outputs(root: Path, expected: dict[str, tuple[str, str, str]]) -> None:
    """Cross-check the current Docker stages, without inventing a Docker parser."""
    path = root / "build/docker/Dockerfile"
    arguments: dict[str, str] = {}
    builds: dict[str, tuple[str, str]] = {}
    copies: list[list[str]] = []
    stage = ""
    for line in re.sub(r"\\\n", " ", path.read_text()).splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        parts = shlex.split(line)
        instruction = parts[0].upper()
        if instruction == "ARG" and len(parts) == 2 and "=" in parts[1]:
            key, value = parts[1].split("=", 1)
            arguments[key] = value
        elif instruction == "FROM":
            stage = parts[-1] if len(parts) >= 4 and parts[-2].upper() == "AS" else ""
            copies = []  # Only copies into the final runtime stage are outputs.
        elif instruction == "RUN" and parts[1:4] == ["cargo", "auditable", "build"]:
            command = parts[4:parts.index("&&")] if "&&" in parts else parts[4:]
            if len(command) != 6 or command[:3] != ["--release", "--locked", "-p"] or command[4] != "--bin":
                raise Refusal(f"{path}: unsupported auditable build selection in {stage}")
            def expand(value: str) -> str:
                return re.sub(r"\$\{([A-Z_]+)\}", lambda match: arguments.get(match[1], match[0]), value)
            if stage in builds:
                raise Refusal(f"{path}: ambiguous auditable build in {stage}")
            builds[stage] = (expand(command[3]), expand(command[5]))
        elif instruction == "COPY":
            copies.append(parts[1:])
    actual: dict[str, tuple[str, str]] = {}
    for parts in copies:
        if parts[-1] not in ENTRYPOINTS:
            continue
        target = parts[-1]
        if target in actual or len(parts) != 3 or not parts[0].startswith("--from="):
            raise Refusal(f"{path}: ambiguous output {target}")
        producer = builds.get(parts[0].split("=", 1)[1])
        source = re.sub(r"\$\{([A-Z_]+)\}", lambda match: arguments.get(match[1], match[0]), parts[1])
        if producer is None or source != "/out/" + producer[1]:
            raise Refusal(f"{path}: {target} does not select an admitted auditable binary output")
        actual[target] = producer
    identities = {target: (name, binary) for target, (name, _version, binary) in expected.items()}
    if actual != identities:
        raise Refusal(f"{path}: Docker outputs {actual} disagree with selected manifest binaries {identities}")


def normalized_target(value: object) -> str:
    value = required_string(value, "Trivy Target")
    return posixpath.normpath("/" + value.lstrip("/"))


def admit(report: object, expected: dict[str, tuple[str, str, str]], image_id: str) -> None:
    if not isinstance(report, dict) or report.get("ArtifactType") != "container_image":
        raise Refusal("expected a native Trivy container image report")
    if report.get("Metadata", {}).get("ImageID") != image_id:
        raise Refusal("Trivy report does not identify the fixed image ID")
    results = report.get("Results")
    if not isinstance(results, list) or not all(isinstance(result, dict) for result in results):
        raise Refusal("Trivy Results is missing or malformed")
    found = {}
    for result in results:
        if result.get("Type") != "rustbinary":
            continue
        target = normalized_target(result.get("Target"))
        if target in ENTRYPOINTS and target not in expected:
            raise Refusal(f"{target}: pruned binary has a Rust inventory")
        if target not in expected:
            continue
        if target in found or result.get("Class") != "lang-pkgs":
            raise Refusal(f"{target}: ambiguous or incorrectly classified Rust inventory")
        found[target] = result
    for target, (name, version, _binary) in expected.items():
        if target not in found:
            raise Refusal(f"{target}: missing Rust inventory")
        packages = found[target].get("Packages")
        if not isinstance(packages, list) or not packages:
            raise Refusal(f"{target}: missing runtime packages")
        by_id = {}
        identities = set()
        roots = []
        for package in packages:
            if not isinstance(package, dict):
                raise Refusal(f"{target}: malformed runtime package")
            package_id = required_string(package.get("ID"), f"{target}: package ID")
            identity = (required_string(package.get("Name"), f"{target}: package name"),
                        required_string(package.get("Version"), f"{target}: package version"))
            if package_id in by_id or identity in identities:
                raise Refusal(f"{target}: ambiguous package ID or identity {package_id}")
            by_id[package_id] = package
            identities.add(identity)
            if package.get("Relationship") == "root":
                roots.append(package_id)
        if len(roots) != 1:
            raise Refusal(f"{target}: expected exactly one runtime root")
        root = by_id[roots[0]]
        if (root["Name"], root["Version"]) != (name, version):
            raise Refusal(f"{target}: root identity must be {name}@{version}")
        if len(by_id) == 1:
            raise Refusal(f"{target}: root-only inventory has no runtime dependency content")
        for package_id, package in by_id.items():
            dependencies = package.get("DependsOn", [])
            if not isinstance(dependencies, list) or not all(isinstance(dep, str) and dep in by_id for dep in dependencies):
                raise Refusal(f"{target}: dangling or malformed dependencies for {package_id}")
        visited = set()
        pending = roots.copy()
        while pending:
            package_id = pending.pop()
            if package_id not in visited:
                visited.add(package_id)
                pending.extend(by_id[package_id].get("DependsOn", []))
        if visited != set(by_id):
            raise Refusal(f"{target}: disconnected runtime packages {sorted(set(by_id) - visited)}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--expectations", action="store_true", help="print selected and pruned paths for filesystem checks")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--image-id")
    args = parser.parse_args()
    try:
        expected = expected_binaries(args.repo)
        if args.expectations:
            for path in ENTRYPOINTS:
                print(f"{'retained' if path in expected else 'pruned'}\t{path}")
        else:
            if args.report is None or not args.image_id:
                parser.error("--report and --image-id are required for admission")
            admit(parse_json_bytes(args.report.read_bytes(), "Trivy report"), expected, args.image_id)
            print("runtime-image-inventory: admitted runtime graphs for " + ", ".join(expected))
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        print(f"runtime-image-inventory: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
