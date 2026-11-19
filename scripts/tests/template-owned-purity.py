#!/usr/bin/env python3

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True

SOURCE_ONLY_PREFIXES = (
    "make/source.mk",
    "scripts/tests/template-",
    "scripts/template/check_init.sh",
    "scripts/template/matrix.py",
    "scripts/template/init.py",
    "scripts/template/profiles.json",
    "specs/",
)
SOURCE_ONLY_REQUIRED_PATHS = (
    "make/source.mk",
    "scripts/template/check_init.sh",
    "scripts/template/matrix.py",
)
REQUIRED_SYNC_HELPERS = {
    "scripts/template/sync.py",
    "scripts/template/state.py",
    "scripts/harness/sync.py",
}


def load_module(root: Path, name: str):
    template_dir = root / "scripts/template"
    module_path = template_dir / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, module_path)
    if spec is None or spec.loader is None:
        raise AssertionError(f"scripts/template/{name}.py cannot be imported")
    added = False
    if os.fspath(template_dir) not in sys.path:
        sys.path.insert(0, os.fspath(template_dir))
        added = True
    try:
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        return module
    finally:
        if added:
            sys.path.remove(os.fspath(template_dir))


def _manifest_covers(entries: tuple[str, ...], helper: str) -> bool:
    return any(
        helper == entry.rstrip("/") or (entry.endswith("/") and helper.startswith(entry))
        for entry in entries
    )


def check(root: Path) -> None:
    state = load_module(root, "state")
    initializer = load_module(root, "init")
    synchronizer = load_module(root, "sync")
    entries = state.manifest_entries(root)
    if set(synchronizer.SOURCE_HELPERS) != REQUIRED_SYNC_HELPERS:
        raise AssertionError("sync SOURCE_HELPERS drifted from REQUIRED_SYNC_HELPERS")
    profile = json.loads((root / "scripts/template/profiles.json").read_text(encoding="utf-8"))
    non_profile_keys = {"schema_version", "source_only", "identity"}
    if not isinstance(profile, dict) or not non_profile_keys.issubset(profile):
        raise AssertionError("profile inventory is missing top-level metadata keys")
    section_names = sorted(set(profile) - non_profile_keys)
    if not section_names:
        raise AssertionError("profile inventory contains no profile sections")
    for name in section_names:
        removal_key = "remove_when_none" if name == "postgres" else "remove_when_unselected"
        section = profile.get(name)
        if not isinstance(section, dict) or set(section) != {removal_key}:
            raise AssertionError(f"profile inventory has no exact {name} section")
        removals = section[removal_key]
        if not isinstance(removals, list):
            raise AssertionError(f"profile inventory has an incomplete {name} projection")
        for relative in removals:
            if not isinstance(relative, str) or not relative:
                raise AssertionError(f"profile inventory has malformed {name} removal")
            plain = relative.rstrip("/")
            if name == "profile-runner-tests" and relative == "scripts/tests/test_profile_runners.py":
                continue
            if any(plain == entry.rstrip("/") or plain.startswith(f"{entry.rstrip('/')}/") for entry in entries):
                raise AssertionError(f"manifest leaks profile-specific {name} output: {relative}")
    for broad_owner in (
        "make/",
        "docs/",
        "scripts/",
        "scripts/template/",
        "scripts/profiles/",
        "scripts/service/",
        "scripts/tests/",
        "crates/",
        "scripts/tests/template-sync-canary.py",
        "scripts/tests/test_migration_history.py",
    ):
        if not state._protected_manifest_owner(broad_owner):
            raise AssertionError(f"broad protected owner was admitted: {broad_owner}")
    missing_helpers = sorted(helper for helper in REQUIRED_SYNC_HELPERS if not _manifest_covers(entries, helper))
    if missing_helpers:
        raise AssertionError(f"manifest omits required sync helpers: {missing_helpers}")
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
            if (
                state.TEMPLATE_REPOSITORY.encode("utf-8") in contents
                and file.relative_to(root).as_posix() not in state._TEMPLATE_PROVENANCE_OWNERS
            ):
                raise AssertionError(f"manifest-owned file contains template identity: {file.relative_to(root)}")
    # Purity is a contract of materialized portable bytes. Both selections use
    # the same initializer renderer as sync; unknown or malformed markers still
    # refuse, and parse_manifest still rejects every marker in the output.
    for grpc in ("none", "enabled"):
        with tempfile.TemporaryDirectory(prefix="template-owned-purity-") as temporary:
            projected = Path(temporary)
            for entry in entries:
                owner = root / entry.rstrip("/")
                if owner.is_symlink() or entry.endswith("/") != owner.is_dir():
                    raise AssertionError(f"unsafe manifest owner: {entry}")
                for file in state._manifest_files(root, entry):
                    state._regular_file(file, file.relative_to(root).as_posix())
                    destination = projected / file.relative_to(root)
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(file, destination)
            inventory = "scripts/template/profiles.json"
            (projected / inventory).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(root / inventory, projected / inventory)
            profiles = state.validate_profiles({"database": "none", "agent_harness": "all"})
            profiles["grpc"] = grpc
            lock = {
                "identity": {
                    "service_name": "purity-api", "repository": "https://github.com/example/purity-api",
                    "description": "Purity API", "codeowner": "@example/platform",
                },
                "profiles": profiles,
            }
            initializer.project_portable(projected, lock)
            projected_entries = state.parse_manifest(projected, target_repository=lock["identity"]["repository"])
            if ("scripts/tests/test_profile_runners.py" in projected_entries) != (grpc == "enabled"):
                raise AssertionError(f"portable profile test ownership does not match grpc={grpc}")
            makefile = (projected / "make/template.mk").read_text(encoding="utf-8")
            if ("grpc-check:" in makefile) != (grpc == "enabled"):
                raise AssertionError(f"portable Make targets do not match grpc={grpc}")
            # Pure older/derived sources do not need one-shot initializer
            # inputs. Reuse this materialized tree without its inventory.
            (projected / inventory).unlink()
            baseline = {
                file.relative_to(projected): file.read_bytes()
                for entry in projected_entries for file in state._manifest_files(projected, entry)
            }
            complete_lock = initializer._lock(
                initializer.InitInputs(**lock["identity"], **profiles), "0" * 40, "complete",
            )
            synchronizer._project_portable(projected, complete_lock)
            (projected / "template.lock").write_text(json.dumps(complete_lock), encoding="utf-8")
            synchronizer._project_portable(projected, complete_lock)
            state.parse_manifest(projected, target_repository=lock["identity"]["repository"])
            if any((projected / path).read_bytes() != contents for path, contents in baseline.items()):
                raise AssertionError("marker-free source projection changed portable bytes")
            different = {**complete_lock, "profiles": {**profiles, "grpc": "enabled" if grpc == "none" else "none"}}
            try:
                synchronizer._project_portable(projected, different)
            except state.Refusal as error:
                if "different capability profiles" not in str(error):
                    raise
            else:
                raise AssertionError("initialized source accepted different capability profiles")
    for path in SOURCE_ONLY_REQUIRED_PATHS:
        candidate = root / path
        if not candidate.exists():
            raise AssertionError(f"source-only check input is missing: {path}")
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
