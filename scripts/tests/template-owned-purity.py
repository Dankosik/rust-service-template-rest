#!/usr/bin/env python3
"""Source-only structural checks for portable ownership and fixture containment."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import sys
from pathlib import Path


SOURCE_ONLY_PREFIXES = (
    "make/source.mk",
    "scripts/tests/template-",
    "scripts/ci/template-init-check.sh",
    "specs/",
    "docs/roadmap.md",
    "evals/",
)
SOURCE_ONLY_REQUIRED_PATHS = (
    "make/source.mk",
    "scripts/ci/template-init-check.sh",
)
REQUIRED_SYNC_HELPERS = {
    "scripts/template-sync.sh",
    "scripts/lib/template_sync.py",
    "scripts/lib/template_state.py",
}


def load_state(root: Path):
    module_path = root / "scripts/lib/template_state.py"
    spec = importlib.util.spec_from_file_location("template_state", module_path)
    if spec is None or spec.loader is None:
        raise AssertionError("template_state.py cannot be imported")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def check(root: Path) -> None:
    state = load_state(root)
    entries = state.parse_manifest(root)
    owned = set(entries)
    for broad_owner in ("make/", "docs/", "scripts/", "crates/", "scripts/tests/template-sync-canary.py"):
        if not state._protected_manifest_owner(broad_owner):
            raise AssertionError(f"broad protected owner was admitted: {broad_owner}")
    if not REQUIRED_SYNC_HELPERS.issubset(owned):
        missing = sorted(REQUIRED_SYNC_HELPERS - owned)
        raise AssertionError(f"manifest omits required sync helpers: {missing}")
    for entry in entries:
        plain = entry.rstrip("/")
        if state._protected_manifest_owner(entry):
            raise AssertionError(f"manifest overrides service-owned policy: {entry}")
        if any(plain == prefix.rstrip("/") or plain.startswith(prefix) for prefix in SOURCE_ONLY_PREFIXES):
            raise AssertionError(f"manifest leaks source-only owner: {entry}")
        for file in state._manifest_files(root, entry):
            contents = file.read_bytes()
            if not contents:
                raise AssertionError(f"manifest owner is empty: {file.relative_to(root)}")
            if state._contains_profile_marker(contents):
                raise AssertionError(f"manifest-owned file contains a profile marker: {file.relative_to(root)}")
            if (
                state.TEMPLATE_REPOSITORY.encode("utf-8") in contents
                and file.relative_to(root).as_posix() not in state._TEMPLATE_PROVENANCE_OWNERS
            ):
                raise AssertionError(f"manifest-owned file contains template identity: {file.relative_to(root)}")
    for path in SOURCE_ONLY_REQUIRED_PATHS:
        candidate = root / path
        if not candidate.exists():
            raise AssertionError(f"source-only check input is missing: {path}")
    if not any(root.glob("scripts/tests/template-*")):
        raise AssertionError("source-only template fixture set is missing")
    for helper in REQUIRED_SYNC_HELPERS:
        if not (root / helper).is_file():
            raise AssertionError(f"required sync helper is missing: {helper}")
    # Keep the map as one shared helper truth rather than embedding pack paths
    # in this source-only checker.
    if not state.ADAPTERS or not state.REQUIRED_AUTHORITIES:
        raise AssertionError("adapter or required-authority inventory is empty")
    digest = hashlib.sha256("\n".join(entries).encode()).hexdigest()[:12]
    print(f"template owned purity: pass manifest_entries={len(entries)} manifest={digest}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True, type=Path)
    args = parser.parse_args()
    check(args.repo.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
