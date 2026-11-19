#!/usr/bin/env python3
"""Enforce workspace crate dependency directions (quality/architecture.json) and in-package #[path]."""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

from scripts import lib

_PATH_ATTR_RE = re.compile(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]')


def check_path_attributes(manifests: Sequence[str], root: Path) -> list[str]:
    dirs = sorted(((root / m).parent.resolve() for m in manifests), key=lambda d: len(d.parts), reverse=True)

    def owner(path: Path) -> Path | None:
        return next((d for d in dirs if path == d or d in path.parents), None)

    issues: list[str] = []
    for directory in dirs:
        for src in sorted(directory.rglob("*.rs")):
            if "target" in src.relative_to(directory).parts or owner(src) != directory:
                continue
            for match in _PATH_ATTR_RE.finditer(src.read_text(encoding="utf-8")):
                if owner((src.parent / match.group(1)).resolve()) != directory:
                    rel = src.relative_to(root).as_posix()
                    issues.append(f"{rel}: #[path] reaches outside its package ({match.group(1)})")
    return issues


def check(metadata: Mapping[str, Any], policy: Mapping[str, Any], root: Path) -> list[str]:
    root = root.resolve()
    roles: dict[str, Any] = policy["roles"]
    members: dict[str, Any] = policy["members"]
    member_ids = set(metadata["workspace_members"])
    packages: dict[str, dict[str, Any]] = {}
    for pkg in metadata["packages"]:
        if pkg["id"] in member_ids:
            rel = Path(pkg["manifest_path"]).resolve().relative_to(root).as_posix()
            packages[rel] = pkg

    dir_to_manifest = {(root / rel).parent.resolve(): rel for rel in packages}
    issues: list[str] = []

    for path, pkg in sorted(packages.items()):
        owner = members.get(path)
        if owner is None:
            issues.append(f"{pkg['name']} ({path}): unclassified workspace member")
            continue
        rule = roles[owner["role"]]
        for dep in pkg["dependencies"]:
            kind = dep.get("kind") or "normal"
            label = f"{pkg['name']} ({path}) -> {dep['name']} [kind={kind}]"
            if rule.get("std_only") and kind != "dev":
                issues.append(f"{label}: {rule['rule']} forbids non-dev dependencies")
                continue
            local_path = dep.get("path")
            if local_path is None:
                continue
            target = dir_to_manifest.get(Path(local_path).resolve())
            if target is None or target not in members:
                issues.append(f"{label}: local dependency outside classified workspace ({local_path})")
                continue
            if kind == "dev" or rule.get("test_only"):
                continue
            target_role = members[target]["role"]
            if roles[target_role].get("test_only"):
                issues.append(f"{label}: production dependency on test-only crate ({target})")
                continue
            allowed = (
                target_role in rule.get("allow_roles", ())
                or path in members[target].get("allow_dependents", {}).get("members", ())
            )
            for allowances in (rule.get("allow_members", {}), owner.get("allow_members", {})):
                allowance = allowances.get(target)
                if allowance and (not allowance.get("optional_only") or dep.get("optional")):
                    allowed = True
            if target_role in rule.get("deny_roles", ()) or not allowed:
                issues.append(f"{label}: {rule['rule']} forbids {target} (role={target_role})")

    issues.extend(check_path_attributes(list(packages), root))
    return sorted(issues)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=lib.ROOT)
    parser.add_argument("--policy", type=Path, default=None)
    args = parser.parse_args(argv)
    root = args.repo.resolve()
    policy_path = args.policy if args.policy is not None else root / "quality/architecture.json"
    if not policy_path.is_absolute():
        policy_path = root / policy_path
    try:
        policy = json.loads(policy_path.read_text(encoding="utf-8"))
        issues = check(lib.cargo_metadata(root, no_deps=True, offline=True), policy, root)
    except (OSError, ValueError, KeyError, lib.ScriptError) as error:
        print(f"architecture-check: {error}", file=sys.stderr)
        return 1
    if issues:
        for issue in issues:
            print(f"architecture-check: {issue}", file=sys.stderr)
        return 1
    print("architecture-check: passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
