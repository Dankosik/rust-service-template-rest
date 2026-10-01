#!/usr/bin/env python3
"""Source-only structural checks for portable ownership and fixture containment."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import shutil
import sys
import tempfile
from pathlib import Path


SOURCE_ONLY_PREFIXES = (
    "make/source.mk",
    "scripts/tests/template-",
    "scripts/ci/template-init-check.sh",
    "scripts/ci/initializer-matrix.py",
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


def load_module(root: Path, name: str):
    module_path = root / f"scripts/lib/{name}.py"
    spec = importlib.util.spec_from_file_location(name, module_path)
    if spec is None or spec.loader is None:
        raise AssertionError(f"{name}.py cannot be imported")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def check(root: Path) -> None:
    state = load_module(root, "template_state")
    initializer = load_module(root, "template_init")
    synchronizer = load_module(root, "template_sync")
    entries = state.manifest_entries(root)
    owned = set(entries)
    profile = json.loads((root / "scripts/lib/template_profiles.json").read_text(encoding="utf-8"))
    expected_profiles = {
        "postgres": "remove_when_none",
        "authn": "remove_when_unselected",
        "oidc-jwt": "remove_when_unselected",
        "oidc-introspection": "remove_when_unselected",
        "outbound-auth": "remove_when_unselected",
        "outbound-http": "remove_when_unselected",
        "tls-fixtures": "remove_when_unselected",
        "request-budget": "remove_when_unselected",
        "http-idempotency": "remove_when_unselected",
        "http-idempotency-mounted": "remove_when_unselected",
        "jobs": "remove_when_unselected",
        "jobs-http-idempotency": "remove_when_unselected",
        "messaging": "remove_when_unselected",
        "worker": "remove_when_unselected",
        "service-secrets": "remove_when_unselected",
        "outbox": "remove_when_unselected",
        "config-url": "remove_when_unselected",
        "integration": "remove_when_unselected",
        "cache": "remove_when_unselected",
        "object-storage": "remove_when_unselected",
        "rustls": "remove_when_unselected",
        "jsonwebtoken": "remove_when_unselected",
        "webhooks-common": "remove_when_unselected",
        "webhooks": "remove_when_unselected",
        "inbound-webhooks": "remove_when_unselected",
    }
    for name, removal_key in expected_profiles.items():
        section = profile.get(name)
        if not isinstance(section, dict) or set(section) != {removal_key, "markers"}:
            raise AssertionError(f"profile inventory has no exact {name} section")
        removals = section[removal_key]
        markers = section["markers"]
        if not isinstance(removals, list) or not isinstance(markers, list) or not markers:
            raise AssertionError(f"profile inventory has an incomplete {name} projection")
        if name not in {"request-budget", "tls-fixtures", "service-secrets", "config-url", "rustls", "jsonwebtoken"} and not removals:
            raise AssertionError(f"profile inventory has no removable {name} output")
        for relative in removals:
            plain = relative.rstrip("/")
            if any(plain == entry.rstrip("/") or plain.startswith(f"{entry.rstrip('/')}/") for entry in entries):
                raise AssertionError(f"manifest leaks profile-specific {name} output: {relative}")
        for marker in markers:
            if not isinstance(marker, dict) or set(marker) not in ({"path", "id"}, {"path", "ids"}):
                raise AssertionError(f"profile inventory has malformed {name} marker")
            marker_ids = [marker["id"]] if "id" in marker else marker["ids"]
            if not isinstance(marker_ids, list) or not marker_ids or len(marker_ids) != len(set(marker_ids)):
                raise AssertionError(f"profile inventory has duplicate {name} marker ids")
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
            inventory = "scripts/lib/template_profiles.json"
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
            state.parse_manifest(projected, target_repository=lock["identity"]["repository"])
            makefile = (projected / "make/template.mk").read_text(encoding="utf-8")
            if ("grpc-check:" in makefile) != (grpc == "enabled"):
                raise AssertionError(f"portable Make targets do not match grpc={grpc}")
            # Pure older/derived sources do not need one-shot initializer
            # inputs. Reuse this materialized tree without its inventory.
            (projected / inventory).unlink()
            baseline = {
                file.relative_to(projected): file.read_bytes()
                for entry in entries for file in state._manifest_files(projected, entry)
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
