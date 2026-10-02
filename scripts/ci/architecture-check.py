#!/usr/bin/env python3
"""Enforce service-owned crate directions on Cargo dependency declarations."""

import argparse
import json
from pathlib import Path, PurePosixPath
import subprocess
import sys


def load_metadata(root):
    """Let Cargo interpret manifests, aliases, targets and workspace inheritance."""
    command = [
        "cargo", "metadata", "--locked", "--offline", "--no-deps",
        "--format-version", "1",
    ]
    try:
        result = subprocess.run(command, cwd=root, capture_output=True, text=True)
    except OSError as error:
        raise ValueError(f"cannot run Cargo metadata: {error}") from error
    if result.returncode:
        raise ValueError(f"Cargo metadata failed: {result.stderr.strip()}")
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ValueError(f"Cargo metadata returned invalid JSON: {error}") from error


def _manifest_path(value):
    if not isinstance(value, str):
        return False
    path = PurePosixPath(value)
    return (
        not path.is_absolute() and path.name == "Cargo.toml"
        and ".." not in path.parts and str(path) == value
    )


def _validate_policy(policy):
    if not isinstance(policy, dict) or policy.get("version") != 1:
        raise ValueError("policy requires version 1")
    roles, members = policy.get("roles"), policy.get("members")
    if not isinstance(roles, dict) or not roles:
        raise ValueError("policy requires nonempty roles")
    if not isinstance(members, dict) or not members:
        raise ValueError("policy requires nonempty members")
    for path, member in members.items():
        if not _manifest_path(path) or not isinstance(member, dict):
            raise ValueError(f"invalid policy member: {path!r}")
        if not isinstance(member.get("role"), str) or member["role"] not in roles:
            raise ValueError(f"unrecognized policy role for {path}")
        if set(member) - {"role", "allow_members"}:
            raise ValueError(f"unknown member policy fields for {path}")
    for role, rule in roles.items():
        if not isinstance(rule, dict) or not isinstance(rule.get("rule"), str) or not rule["rule"].strip():
            raise ValueError(f"role {role} requires a named rule")
        if set(rule) - {"rule", "allow_roles", "allow_members", "deny_roles", "test_only"}:
            raise ValueError(f"unknown role policy fields for {role}")
        if type(rule.get("test_only", False)) is not bool:
            raise ValueError(f"role {role} requires boolean test_only")
        for field in ("allow_roles", "deny_roles"):
            values = rule.get(field, [])
            if not isinstance(values, list) or any(not isinstance(v, str) or v not in roles for v in values):
                raise ValueError(f"role {role} has invalid {field}")
    for owner, rule in list(roles.items()) + list(members.items()):
        allowances = rule.get("allow_members", {})
        if not isinstance(allowances, dict):
            raise ValueError(f"{owner}: allow_members must be an object")
        for target, allowance in allowances.items():
            if target not in members or not isinstance(allowance, dict):
                raise ValueError(f"{owner}: unregistered allowed member {target}")
            if set(allowance) - {"reason", "authority", "optional_only"}:
                raise ValueError(f"{owner}: unknown allowance fields for {target}")
            if any(not isinstance(allowance.get(key), str) or not allowance[key].strip()
                   for key in ("reason", "authority")):
                raise ValueError(f"{owner}: {target} requires a reviewed reason and authority")
            if type(allowance.get("optional_only", False)) is not bool:
                raise ValueError(f"{owner}: {target} requires boolean optional_only")
    return roles, members


def _workspace_packages(metadata, root):
    if not isinstance(metadata, dict) or metadata.get("version") != 1:
        raise ValueError("Cargo metadata requires format version 1")
    packages, members = metadata.get("packages"), metadata.get("workspace_members")
    if not isinstance(packages, list) or not isinstance(members, list) or not members:
        raise ValueError("Cargo metadata requires packages and nonempty workspace_members")
    if any(not isinstance(member, str) for member in members) or len(set(members)) != len(members):
        raise ValueError("Cargo metadata has invalid workspace member IDs")
    by_id = {}
    for package in packages:
        if not isinstance(package, dict) or not isinstance(package.get("id"), str):
            raise ValueError("Cargo metadata contains an invalid package")
        if package["id"] in by_id:
            raise ValueError(f"Cargo metadata contains duplicate package {package['id']}")
        by_id[package["id"]] = package
    result = {}
    for member in members:
        package = by_id.get(member)
        if package is None:
            raise ValueError(f"Cargo metadata omits workspace package {member}")
        if not isinstance(package.get("name"), str) or not isinstance(package.get("manifest_path"), str):
            raise ValueError(f"Cargo metadata package {member} lacks name/manifest_path")
        manifest = Path(package["manifest_path"])
        if not manifest.is_absolute() or manifest.name != "Cargo.toml":
            raise ValueError(f"Cargo metadata has invalid manifest path {manifest}")
        try:
            relative = manifest.resolve().relative_to(root).as_posix()
        except ValueError as error:
            raise ValueError(f"workspace member escapes repository: {manifest}") from error
        if relative in result or not isinstance(package.get("dependencies"), list):
            raise ValueError(f"Cargo metadata has duplicate manifest or invalid dependencies: {relative}")
        result[relative] = package
    return result


def _edge_label(package, path, dependency):
    kind = dependency.get("kind")
    details = [f"kind={kind if kind is not None else 'normal'}"]
    for key in ("rename", "target"):
        if dependency.get(key) is not None:
            details.append(f"{key}={dependency[key]!r}")
    details.append(f"optional={dependency.get('optional')!r}")
    return f"{package['name']} ({path}) -> {dependency.get('name', '?')} [{', '.join(details)}]"


def check(metadata, policy, root):
    """Return deterministic diagnostics; an empty list means declarations comply."""
    root = Path(root).resolve()
    try:
        roles, members = _validate_policy(policy)
        packages = _workspace_packages(metadata, root)
    except (ValueError, OSError) as error:
        return [f"architecture input: {error}"]
    diagnostics = []
    directories = {(root / path).parent.resolve(): path for path in packages}
    for path, package in sorted(packages.items()):
        owner = members.get(path)
        if owner is None:
            diagnostics.append(f"{package['name']} ({path}): unclassified-workspace-member")
        for dependency in package["dependencies"]:
            if not isinstance(dependency, dict):
                diagnostics.append(f"{package['name']} ({path}): invalid dependency declaration")
                continue
            label = _edge_label(package, path, dependency)
            if "kind" not in dependency or dependency["kind"] not in (None, "normal", "dev", "build"):
                diagnostics.append(f"{label}: unknown-dependency-kind")
                continue
            if (not isinstance(dependency.get("name"), str)
                    or type(dependency.get("optional")) is not bool
                    or any(dependency.get(key) is not None and not isinstance(dependency[key], str)
                           for key in ("rename", "target", "path", "source"))):
                diagnostics.append(f"{label}: malformed-dependency-declaration")
                continue
            local_path = dependency.get("path")
            if local_path is None:
                if not dependency.get("source"):
                    diagnostics.append(f"{label}: local-dependency-missing-path")
                continue  # Registry and Git dependencies are outside this verdict.
            if not Path(local_path).is_absolute():
                diagnostics.append(f"{label}: invalid-local-dependency-path {local_path!r}")
                continue
            target = directories.get(Path(local_path).resolve())
            if target is None or target not in members:
                diagnostics.append(f"{label}: local-dependency-outside-classified-workspace ({local_path})")
                continue
            if owner is None:
                continue
            rule = roles[owner["role"]]
            target_role = members[target]["role"]
            if dependency["kind"] == "dev" or rule.get("test_only", False):
                continue
            if roles[target_role].get("test_only", False):
                diagnostics.append(f"{label}: production-to-test-only ({target})")
                continue
            allowed = target_role in rule.get("allow_roles", [])
            for allowances in (rule.get("allow_members", {}), owner.get("allow_members", {})):
                allowance = allowances.get(target)
                if allowance and (not allowance.get("optional_only", False) or dependency["optional"]):
                    allowed = True
            if target_role in rule.get("deny_roles", []) or not allowed:
                diagnostics.append(f"{label}: {rule['rule']} forbids {target} (role={target_role})")
    return sorted(diagnostics)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    root = args.root.resolve()
    try:
        policy = json.loads((root / "quality/architecture.json").read_text(encoding="utf-8"))
        diagnostics = check(load_metadata(root), policy, root)
    except (OSError, ValueError) as error:
        print(f"architecture-check: {error}", file=sys.stderr)
        return 1
    if diagnostics:
        for diagnostic in diagnostics:
            print(f"architecture-check: {diagnostic}", file=sys.stderr)
        return 1
    print("architecture-check: declared workspace dependency directions pass")
    return 0


if __name__ == "__main__":
    sys.exit(main())
