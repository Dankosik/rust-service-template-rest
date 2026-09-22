#!/usr/bin/env python3
"""One-shot derived-service initialization from a committed template snapshot."""

from __future__ import annotations

import argparse
import html
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence

from template_state import (
    ADAPTERS,
    DATABASE_CHOICES,
    HARNESS_CHOICES,
    LOCK_NAME,
    LOCK_SCHEMA_VERSION,
    TEMPLATE_REPOSITORY,
    PlannedLink,
    PlannedWrite,
    Refusal,
    ToolFailure,
    admit_plan,
    atomic_write,
    ensure_safe_destination,
    git,
    git_head,
    git_root,
    load_lock,
    parse_manifest,
    parse_json_bytes,
    safe_relative,
    selected_adapters,
    snapshot_tree,
    validate_codeowner,
    validate_description,
    validate_repository,
    validate_service_name,
    write_plan,
)


_MARKER_RE = re.compile(
    r"^(?P<indent>[ \t]*)(?:(?P<hash>#)|(?P<slash>//)|(?P<html><!--))"
    r"[ \t]*template:(?P<kind>begin|end)[ \t]+postgres:(?P<id>[a-z0-9]+(?:-[a-z0-9]+)*)"
    r"(?:(?(html)[ \t]*-->|))[ \t]*$"
)
_PROFILE_FILE = "scripts/lib/template_profiles.json"


@dataclass(frozen=True)
class InitInputs:
    service_name: str
    repository: str
    description: str
    codeowner: str
    database: str
    agent_harness: str

    def identity(self) -> dict[str, str]:
        return {
            "service_name": self.service_name,
            "repository": self.repository,
            "description": self.description,
            "codeowner": self.codeowner,
        }

    def profiles(self) -> dict[str, str]:
        return {"database": self.database, "agent_harness": self.agent_harness}


@dataclass(frozen=True)
class ProfileData:
    source_only: tuple[str, ...]
    remove_when_none: tuple[str, ...]
    markers: tuple[tuple[str, str], ...]
    identity: tuple[dict[str, Any], ...]
    cargo_lock: dict[str, Any]


def _argument_value(arguments: argparse.Namespace, field: str, *, default: str | None = None) -> str:
    flag_value = getattr(arguments, field)
    environment_name = field.upper()
    environment_value = os.environ.get(environment_name)
    if flag_value is not None and environment_value is not None:
        raise Refusal(f"{environment_name} may be supplied once, by flag or environment")
    value = flag_value if flag_value is not None else environment_value
    if value is None:
        if default is None:
            raise Refusal(f"{environment_name} is required")
        return default
    return value


def parse_inputs(arguments: argparse.Namespace) -> InitInputs:
    inputs = InitInputs(
        service_name=validate_service_name(_argument_value(arguments, "service_name")),
        repository=validate_repository(_argument_value(arguments, "repository")),
        description=validate_description(_argument_value(arguments, "description")),
        codeowner=validate_codeowner(_argument_value(arguments, "codeowner")),
        database=_argument_value(arguments, "database", default="none"),
        agent_harness=_argument_value(arguments, "agent_harness", default="all"),
    )
    if inputs.database not in DATABASE_CHOICES:
        raise Refusal("DATABASE is unsupported")
    if inputs.agent_harness not in HARNESS_CHOICES:
        raise Refusal("AGENT_HARNESS is unsupported")
    return inputs


def _profile_data(snapshot: Path) -> ProfileData:
    profile_path = snapshot / _PROFILE_FILE
    try:
        raw = parse_json_bytes(profile_path.read_bytes(), _PROFILE_FILE)
    except FileNotFoundError:
        raise Refusal("template profile inventory is missing") from None
    expected = {"schema_version", "source_only", "postgres", "identity", "cargo_lock"}
    if not isinstance(raw, dict) or set(raw) != expected or raw.get("schema_version") != 1:
        raise Refusal("template profile inventory has an unsupported schema")
    source_only = _path_list(raw["source_only"], "source_only")
    postgres = raw["postgres"]
    if not isinstance(postgres, dict) or set(postgres) != {"remove_when_none", "markers"}:
        raise Refusal("template PostgreSQL inventory has an unsupported shape")
    removals = _path_list(postgres["remove_when_none"], "remove_when_none")
    markers = _markers(postgres["markers"])
    identity = raw["identity"]
    if not isinstance(identity, list):
        raise Refusal("template identity inventory has an unsupported shape")
    _validate_identity_inventory(identity)
    cargo_lock = raw["cargo_lock"]
    if cargo_lock != {"schema_version": 1}:
        raise Refusal("template Cargo.lock inventory has an unsupported shape")
    return ProfileData(tuple(source_only), tuple(removals), tuple(markers), tuple(identity), cargo_lock)


def _path_list(value: object, label: str) -> list[str]:
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise Refusal(f"template {label} inventory is invalid")
    paths = [safe_relative(item, allow_directory=True) for item in value]
    if len(paths) != len(set(paths)):
        raise Refusal(f"template {label} inventory has duplicate paths")
    return paths


def _markers(value: object) -> list[tuple[str, str]]:
    if not isinstance(value, list):
        raise Refusal("template marker inventory is invalid")
    markers: list[tuple[str, str]] = []
    for item in value:
        if not isinstance(item, dict) or set(item) != {"path", "id"}:
            raise Refusal("template marker inventory has an unsupported entry")
        path = safe_relative(item["path"])
        marker_id = item["id"]
        if not isinstance(marker_id, str) or not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", marker_id):
            raise Refusal("template marker inventory has an invalid id")
        markers.append((path, marker_id))
    if len(markers) != len(set(markers)) or len({marker_id for _, marker_id in markers}) != len(markers):
        raise Refusal("template marker inventory has duplicate paths or ids")
    return markers


def _validate_identity_inventory(value: list[Any]) -> None:
    seen: set[tuple[str, str]] = set()
    encodings = {"text", "rust_string", "toml_string", "json_string", "url", "markdown"}
    for anchor in value:
        if not isinstance(anchor, dict) or set(anchor) not in (
            {"path", "old", "template", "count", "encoding"},
            {"path", "old", "template", "count", "encoding", "context"},
        ):
            raise Refusal("template identity inventory has an unsupported anchor")
        path = safe_relative(anchor["path"])
        if not isinstance(anchor["old"], str) or not anchor["old"]:
            raise Refusal("template identity anchor has no old literal")
        if not isinstance(anchor["template"], str) or not anchor["template"]:
            raise Refusal("template identity anchor has no template")
        if type(anchor["count"]) is not int or anchor["count"] < 1:
            raise Refusal("template identity anchor has an invalid count")
        if anchor["encoding"] not in encodings:
            raise Refusal("template identity anchor has an unsupported encoding")
        key = (path, anchor["old"])
        if key in seen:
            raise Refusal("template identity inventory repeats an anchor")
        seen.add(key)


def _render_value(value: str, encoding: str) -> str:
    if encoding in {"text", "url"}:
        return value
    if encoding in {"rust_string", "toml_string", "json_string"}:
        return json.dumps(value, ensure_ascii=False)[1:-1]
    if encoding == "markdown":
        escaped = html.escape(value, quote=False)
        return re.sub(r"([\\`*_{}\[\]()<>#+.!|])", r"\\\1", escaped)
    raise AssertionError(f"unhandled encoding {encoding}")


def _identity_values(inputs: InitInputs, encoding: str) -> dict[str, str]:
    repository_path = inputs.repository.removeprefix("https://github.com/")
    return {
        "service_name": _render_value(inputs.service_name, encoding),
        "service_name_underscore": _render_value(inputs.service_name.replace("-", "_"), encoding),
        "repository": _render_value(inputs.repository, encoding),
        "repository_path": _render_value(repository_path, encoding),
        "description": _render_value(inputs.description, encoding),
        "codeowner": _render_value(inputs.codeowner, encoding),
    }


def _apply_identity(snapshot: Path, profiles: ProfileData, inputs: InitInputs) -> None:
    for anchor in profiles.identity:
        path = snapshot / anchor["path"]
        try:
            contents = path.read_text(encoding="utf-8")
        except UnicodeDecodeError as error:
            raise Refusal(f"identity anchor is not UTF-8: {anchor['path']}") from error
        if (
            anchor["path"] == "crates/service/Cargo.toml"
            and anchor["old"] == 'name = "service"'
            and anchor["count"] == 2
        ):
            _replace_service_manifest_names(path, contents, inputs.service_name)
            continue
        count = contents.count(anchor["old"])
        if count != anchor["count"]:
            raise Refusal(f"identity anchor count changed: {anchor['path']}")
        try:
            replacement = anchor["template"].format_map(_identity_values(inputs, anchor["encoding"]))
        except (KeyError, ValueError) as error:
            raise Refusal(f"identity anchor template is invalid: {anchor['path']}") from error
        path.write_text(contents.replace(anchor["old"], replacement), encoding="utf-8")


def _replace_service_manifest_names(path: Path, contents: str, service_name: str) -> None:
    """Rename package/bin while proving the library target stays `service`."""

    section = ""
    replacements = 0
    output: list[str] = []
    library_name = None
    for line in contents.splitlines(keepends=True):
        if line.strip() in {"[package]", "[lib]", "[[bin]]"}:
            section = line.strip()
        if line == 'name = "service"\n' and section in {"[package]", "[[bin]]"}:
            output.append(f'name = "{service_name}"\n')
            replacements += 1
            continue
        if line.startswith("name = ") and section == "[lib]":
            library_name = line.rstrip("\n")
        output.append(line)
    if replacements != 2 or library_name != 'name = "service"':
        raise Refusal("service Cargo identity anchors changed")
    path.write_text("".join(output), encoding="utf-8")


def _marker(line: str) -> tuple[str, str] | None:
    matched = _MARKER_RE.fullmatch(line)
    if matched is None:
        return None
    return (matched.group("kind"), matched.group("id"))


def _apply_markers(snapshot: Path, profiles: ProfileData, database: str) -> None:
    expected = set(profiles.markers)
    seen: set[tuple[str, str]] = set()
    for relative in sorted({path for path, _marker_id in profiles.markers}):
        path = snapshot / relative
        if not path.exists() or path.is_symlink() or not path.is_file():
            raise Refusal(f"registered profile marker surface is missing: {relative}")
        if not path.is_file() or path.is_symlink():
            continue
        try:
            lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
        except UnicodeDecodeError:
            continue
        stack: list[str] = []
        transformed: list[str] = []
        found = False
        for line in lines:
            parsed = _marker(line.rstrip("\r\n"))
            if parsed is None:
                if stack and database == "none":
                    continue
                transformed.append(line)
                continue
            found = True
            kind, marker_id = parsed
            key = (relative, marker_id)
            if key not in expected:
                raise Refusal(f"unknown profile marker in {relative}")
            if kind == "begin":
                if stack:
                    raise Refusal(f"nested profile marker in {relative}")
                if key in seen:
                    raise Refusal(f"duplicate profile marker in {relative}")
                stack.append(marker_id)
                seen.add(key)
            else:
                if stack != [marker_id]:
                    raise Refusal(f"mismatched profile marker in {relative}")
                stack.pop()
        if stack:
            raise Refusal(f"unclosed profile marker in {relative}")
        if found:
            path.write_text("".join(transformed), encoding="utf-8")
    if seen != expected:
        raise Refusal("template profile marker inventory does not match source")


def _remove_paths(snapshot: Path, paths: Sequence[str]) -> None:
    for relative in paths:
        path = snapshot / relative.rstrip("/")
        if not path.exists() and not path.is_symlink():
            raise Refusal(f"planned source removal is missing: {relative}")
        mode = path.lstat().st_mode
        if stat.S_ISLNK(mode):
            raise Refusal(f"planned source removal is a symlink: {relative}")
        if stat.S_ISDIR(mode):
            shutil.rmtree(path)
        elif stat.S_ISREG(mode):
            path.unlink()
        else:
            raise Refusal(f"planned source removal is unsupported: {relative}")


def _remove_unselected_adapters(snapshot: Path, harness: str) -> None:
    selected = set(selected_adapters(harness))
    for adapter, pack in ADAPTERS.items():
        if adapter in selected:
            continue
        _remove_paths(snapshot, (*pack.canonical, *pack.generated, *pack.settings))


def _tracked_checkout_is_clean(root: Path) -> None:
    raw = git(root, ["status", "--porcelain=v1", "-z"])
    records = [record for record in raw.split(b"\0") if record]
    for record in records:
        if len(record) < 3:
            raise Refusal("git status contains an invalid record")
        status = record[:2]
        if status not in {b"??", b"!!"}:
            raise Refusal("initializer requires a clean tracked checkout")


def _target_overlap_is_clean(root: Path, planned: Sequence[str]) -> None:
    # Untracked and ignored data matter only when initialization will replace or
    # remove that exact path.  A later full plan expands this set before writes.
    roots = tuple(item.rstrip("/") for item in planned)
    for arguments in (
        ["ls-files", "--others", "--exclude-standard", "-z"],
        ["ls-files", "--others", "--ignored", "--exclude-standard", "-z"],
    ):
        for raw_path in git(root, arguments).split(b"\0"):
            if not raw_path:
                continue
            path = raw_path.decode("utf-8", "surrogateescape")
            if any(path == candidate or path.startswith(f"{candidate}/") for candidate in roots):
                raise Refusal("untracked or ignored content overlaps initializer output")


def _lock(inputs: InitInputs, revision: str, state: str) -> dict[str, Any]:
    return {
        "schema_version": LOCK_SCHEMA_VERSION,
        "state": state,
        "identity": inputs.identity(),
        "profiles": inputs.profiles(),
        "source": {
            "repository": TEMPLATE_REPOSITORY,
            "checkout_revision": revision,
            "provenance": "local-checkout",
        },
    }


def _lock_bytes(inputs: InitInputs, revision: str, state: str) -> bytes:
    return (json.dumps(_lock(inputs, revision, state), indent=2, ensure_ascii=False) + "\n").encode("utf-8")


def _postconditions(root: Path, inputs: InitInputs, profiles: ProfileData, *, initial: bool = False) -> None:
    try:
        workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
        service_manifest = tomllib.loads((root / "crates/service/Cargo.toml").read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise Refusal("initialized Cargo manifests are invalid") from error
    if workspace.get("workspace", {}).get("package", {}).get("repository") != inputs.repository:
        raise Refusal("initialized workspace repository identity is inconsistent")
    package = service_manifest.get("package", {})
    bins = service_manifest.get("bin", [])
    if (
        package.get("name") != inputs.service_name
        or package.get("description") != inputs.description
        or service_manifest.get("lib", {}).get("name") != "service"
        or not isinstance(bins, list)
        or {bin_data.get("name") for bin_data in bins if isinstance(bin_data, dict)} != {inputs.service_name, "openapi"}
    ):
        raise Refusal("initialized service package or binary identity is inconsistent")
    if initial:
        for relative in profiles.source_only:
            if (root / relative.rstrip("/")).exists():
                raise Refusal(f"source-only output remains: {relative}")
    _validate_database_pack(root, profiles, inputs.database)
    selected = set(selected_adapters(inputs.agent_harness))
    for adapter, pack in ADAPTERS.items():
        paths = (*pack.canonical, *pack.generated, *pack.settings)
        if adapter in selected:
            if any(not (root / item.rstrip("/")).exists() and not (root / item.rstrip("/")).is_symlink() for item in paths):
                raise Refusal(f"selected adapter output is incomplete: {adapter}")
        elif any((root / item.rstrip("/")).exists() or (root / item.rstrip("/")).is_symlink() for item in paths):
            raise Refusal(f"unselected adapter output remains: {adapter}")
    parse_manifest(
        root, harness=inputs.agent_harness, target_repository=inputs.repository,
        initializing=initial,
    )
    _validate_generated_ownership(root, inputs.agent_harness)
    for relative in {path for path, _marker_id in profiles.markers}:
        path = root / relative
        if not path.exists() or path.is_symlink() or not path.is_file():
            continue
        try:
            for line in path.read_text(encoding="utf-8").splitlines():
                if _marker(line) is not None:
                    raise Refusal("initialized output retains an executable profile marker")
        except UnicodeDecodeError as error:
                raise Refusal(f"profile marker surface is not UTF-8: {relative}") from error


def _validate_database_pack(root: Path, profiles: ProfileData, database: str) -> None:
    """Replay proves selected pack shape without reading ordinary service bytes."""

    for relative in profiles.remove_when_none:
        candidate = root / relative.rstrip("/")
        exists = candidate.exists() or candidate.is_symlink()
        if database == "none":
            if exists:
                raise Refusal(f"unselected PostgreSQL output remains: {relative}")
            continue
        if not exists or candidate.is_symlink():
            raise Refusal(f"selected PostgreSQL output is missing: {relative}")
        mode = candidate.lstat().st_mode
        if relative.endswith("/"):
            if not stat.S_ISDIR(mode):
                raise Refusal(f"selected PostgreSQL directory has an invalid type: {relative}")
        elif not stat.S_ISREG(mode):
            raise Refusal(f"selected PostgreSQL file has an invalid type: {relative}")


def _validate_generated_ownership(root: Path, harness: str) -> None:
    """Read generated carrier shape only; target helpers never execute here."""

    selected = set(selected_adapters(harness))
    for adapter in ("claude", "qwen"):
        if adapter not in selected:
            continue
        canonical = root / ".agents/skills"
        links = root / f".{adapter}/skills"
        if canonical.is_symlink() or links.is_symlink() or not canonical.is_dir() or not links.is_dir():
            raise Refusal(f"selected {adapter} skill ownership shape is invalid")
        expected_names = {entry.name for entry in canonical.iterdir() if entry.is_dir() and not entry.is_symlink()}
        actual_names = {entry.name for entry in links.iterdir()}
        if actual_names != expected_names:
            raise Refusal(f"selected {adapter} generated skill coverage is incomplete")
        for link in links.iterdir():
            expected = canonical / link.name
            if not link.is_symlink() or os.readlink(link) != f"../../.agents/skills/{link.name}" or not expected.is_dir():
                raise Refusal(f"selected {adapter} generated skill ownership shape is invalid")
    if "codex" in selected:
        config = root / ".codex/config.toml"
        if config.is_symlink() or not config.is_file():
            raise Refusal("selected Codex generated ownership shape is invalid")
        lines = config.read_text(encoding="utf-8").splitlines()
        for begin, end in (
            ("# template-owned-codex-runtime:start", "# template-owned-codex-runtime:end"),
            ("# template-owned-codex-agents:start", "# template-owned-codex-agents:end"),
        ):
            if lines.count(begin) != 1 or lines.count(end) != 1 or lines.index(begin) >= lines.index(end):
                raise Refusal("selected Codex generated ownership shape is invalid")


def _replay(root: Path, inputs: InitInputs) -> int:
    lock = load_lock(root, required=True)
    assert lock is not None
    if lock["state"] != "complete":
        raise Refusal("template.lock is incomplete; inspect the init-produced diff and use a fresh template checkout")
    if lock["identity"] != inputs.identity() or lock["profiles"] != inputs.profiles():
        raise Refusal("template initialization choices differ from the complete template.lock")
    profiles = _profile_data(root)
    _postconditions(root, inputs, profiles)
    print("template init: matching complete lock; no changes")
    return 0


def _run_staged_command(snapshot: Path, command: Sequence[str], operation: str) -> bytes:
    try:
        result = subprocess.run(
            command,
            cwd=snapshot,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env=_staged_environment(snapshot),
        )
    except OSError as error:
        raise ToolFailure(f"staged {operation} tool is unavailable") from error
    if result.returncode:
        raise Refusal(f"staged {operation} failed (exit {result.returncode})")
    return result.stdout


def _preflight_staged(snapshot: Path, inputs: InitInputs, profiles: ProfileData) -> None:
    _check_harness_projection(snapshot, "all")
    _apply_markers(snapshot, profiles, inputs.database)
    _apply_identity(snapshot, profiles, inputs)
    _remove_paths(snapshot, profiles.source_only)
    if inputs.database == "none":
        _remove_paths(snapshot, profiles.remove_when_none)
    _remove_unselected_adapters(snapshot, inputs.agent_harness)
    _generate_harness_projection(snapshot, inputs.agent_harness)
    _check_harness_projection(snapshot, inputs.agent_harness)
    # Cargo.lock has its own strict source-shape projection.  The accepted map
    # is intentionally required rather than asking Cargo to resolve/update it.
    _project_cargo_lock(snapshot, profiles.cargo_lock, inputs)
    metadata_bytes = _run_staged_command(
        snapshot,
        ["cargo", "metadata", "--locked", "--offline", "--format-version", "1"],
        "locked offline Cargo metadata",
    )
    _format_staged_rust(snapshot, metadata_bytes)
    try:
        generated = subprocess.run(
            ["cargo", "run", "-q", "-p", inputs.service_name, "--bin", "openapi", "--locked", "--offline"],
            cwd=snapshot,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env=_staged_environment(snapshot),
        )
    except OSError as error:
        raise ToolFailure("staged OpenAPI generator is unavailable") from error
    if generated.returncode:
        raise Refusal(f"staged OpenAPI generation failed (exit {generated.returncode})")
    (snapshot / "api/openapi/service.yaml").write_bytes(generated.stdout)


def _format_staged_rust(snapshot: Path, metadata_bytes: bytes) -> None:
    """Use pinned rustfmt on Cargo's workspace targets, without another resolver."""

    try:
        metadata = json.loads(metadata_bytes)
        members = set(metadata["workspace_members"])
        channel = tomllib.loads((snapshot / "rust-toolchain.toml").read_text(encoding="utf-8"))["toolchain"]["channel"]
        targets_by_edition: dict[str, set[str]] = {}
        for package in metadata["packages"]:
            if package["id"] not in members:
                continue
            edition = package["edition"]
            for target in package["targets"]:
                path = Path(target["src_path"]).resolve(strict=True)
                if not path.is_relative_to(snapshot.resolve()):
                    raise Refusal("workspace formatting target escapes the staged tree")
                if path.suffix == ".rs":
                    targets_by_edition.setdefault(edition, set()).add(os.fspath(path))
    except (KeyError, TypeError, ValueError, OSError) as error:
        raise Refusal("staged formatting inputs are invalid") from error
    if not targets_by_edition or not isinstance(channel, str):
        raise Refusal("staged formatting inputs are incomplete")
    lock_before = (snapshot / "Cargo.lock").read_bytes()
    for edition, targets in sorted(targets_by_edition.items()):
        _run_staged_command(
            snapshot,
            ["rustup", "run", channel, "rustfmt", "--edition", edition, *sorted(targets)],
            "pinned Rust formatting",
        )
    if (snapshot / "Cargo.lock").read_bytes() != lock_before:
        raise Refusal("staged formatting changed the projected Cargo.lock")


def _staged_environment(snapshot: Path) -> dict[str, str]:
    environment = os.environ.copy()
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    environment["CARGO_TARGET_DIR"] = os.fspath(snapshot.parent / "cargo-target")
    return environment


def _harness_commands(snapshot: Path, harness: str, mode: str) -> list[list[str]]:
    commands = [
        ["bash", "scripts/agent-roles-sync.sh", f"--{mode}", "--repo", os.fspath(snapshot), "--harness", harness]
    ]
    selected = set(selected_adapters(harness))
    if "codex" in selected:
        commands.append(["bash", "scripts/codex-agents-sync.sh", f"--{mode}", "--repo", os.fspath(snapshot), "--harness", harness])
    for adapter in ("claude", "qwen"):
        if adapter in selected:
            commands.append(["bash", "scripts/harness-skills-sync.sh", adapter, f"--{mode}", "--repo", os.fspath(snapshot), "--harness", harness])
    return commands


def _run_harness(snapshot: Path, harness: str, mode: str) -> None:
    for command in _harness_commands(snapshot, harness, mode):
        _run_staged_command(snapshot, command, f"{mode} harness projection")


def _check_harness_projection(snapshot: Path, harness: str) -> None:
    _run_harness(snapshot, harness, "check")


def _generate_harness_projection(snapshot: Path, harness: str) -> None:
    _run_harness(snapshot, harness, "apply")


@dataclass
class _LockRecord:
    body: str
    data: dict[str, Any]

    @property
    def key(self) -> tuple[str, str, str]:
        return (self.data["name"], self.data["version"], self.data.get("source", ""))


def _lock_records(contents: str) -> tuple[str, list[_LockRecord]]:
    header, separator, remainder = contents.partition("[[package]]\n")
    if not separator or "version = 4" not in header:
        raise Refusal("Cargo.lock has an unsupported header")
    records: list[_LockRecord] = []
    for body in remainder.split("[[package]]\n"):
        if not body.strip():
            continue
        try:
            parsed = tomllib.loads("[[package]]\n" + body)
            package = parsed["package"]
        except (tomllib.TOMLDecodeError, KeyError, TypeError) as error:
            raise Refusal("Cargo.lock has an unsupported package block") from error
        if not isinstance(package, list) or len(package) != 1 or not isinstance(package[0], dict):
            raise Refusal("Cargo.lock has an unsupported package block")
        data = package[0]
        if not isinstance(data.get("name"), str) or not isinstance(data.get("version"), str):
            raise Refusal("Cargo.lock package identity is invalid")
        dependencies = data.get("dependencies", [])
        if not isinstance(dependencies, list) or not all(isinstance(item, str) for item in dependencies):
            raise Refusal("Cargo.lock package dependencies are invalid")
        records.append(_LockRecord(body, data))
    if len({record.key for record in records}) != len(records):
        raise Refusal("Cargo.lock has duplicate package identities")
    return header, records


def _lock_record(records: list[_LockRecord], name: str, version: str, source: str = "") -> _LockRecord:
    matches = [record for record in records if record.key == (name, version, source)]
    if len(matches) != 1:
        raise Refusal("Cargo.lock required package record is missing or ambiguous")
    return matches[0]


def _replace_lock_dependencies(record: _LockRecord, expected: list[str], replacement: list[str]) -> None:
    if record.data.get("dependencies", []) != expected:
        raise Refusal("Cargo.lock dependency source shape changed")
    block = re.compile(r"^dependencies = \[\n(?: [^\n]*\n)*?\]\n", re.MULTILINE)
    matched = block.search(record.body)
    if expected and matched is None:
        raise Refusal("Cargo.lock dependency block is malformed")
    if not expected and matched is not None:
        raise Refusal("Cargo.lock has an unexpected dependency block")
    rendered = "" if not replacement else "dependencies = [\n" + "".join(f' "{item}",\n' for item in replacement) + "]\n"
    if matched is not None:
        record.body = record.body[: matched.start()] + rendered + record.body[matched.end() :]
    elif rendered:
        record.body += rendered
    record.data["dependencies"] = replacement


def _replace_lock_name(record: _LockRecord, new_name: str) -> None:
    old_name = record.data["name"]
    pattern = re.compile(rf'^name = "{re.escape(old_name)}"$', re.MULTILINE)
    if len(pattern.findall(record.body)) != 1:
        raise Refusal("Cargo.lock local package name anchor changed")
    record.body = pattern.sub(f'name = "{new_name}"', record.body, count=1)
    record.data["name"] = new_name


def _dependency_key(dependency: str, records: list[_LockRecord]) -> tuple[str, str, str]:
    name, *maybe_version = dependency.split(" ", 1)
    candidates = [record.key for record in records if record.data["name"] == name]
    if maybe_version:
        candidates = [key for key in candidates if key[1] == maybe_version[0]]
    if len(candidates) != 1:
        raise Refusal("Cargo.lock dependency does not resolve uniquely")
    return candidates[0]


def _project_cargo_lock(snapshot: Path, inventory: dict[str, Any], inputs: InitInputs) -> None:
    """Project the current lock with guarded local edges, then exact reachability."""

    if inventory != {"schema_version": 1}:
        raise Refusal("template Cargo.lock projection inventory is unsupported")
    lock = snapshot / "Cargo.lock"
    header, records = _lock_records(lock.read_text(encoding="utf-8"))
    registry = "registry+https://github.com/rust-lang/crates.io-index"
    local_names = {
        "service",
        "service-config",
        "health",
        "infra-http",
        "infra-postgres",
        "infra-telemetry",
        "migrate",
        "integration-tests",
    }
    for name in local_names:
        _lock_record(records, name, "0.1.0")
    service = _lock_record(records, "service", "0.1.0")
    if inputs.database == "none":
        service_dependencies = list(service.data.get("dependencies", []))
        if not {"infra-postgres", "secrecy"}.issubset(service_dependencies):
            raise Refusal("Cargo.lock service PostgreSQL edges are missing")
        _replace_lock_dependencies(
            service,
            service_dependencies,
            [item for item in service_dependencies if item not in {"infra-postgres", "secrecy"}],
        )
        if "infra-postgres" in service.data["dependencies"] or "secrecy" in service.data["dependencies"]:
            raise Refusal("Cargo.lock PostgreSQL service edges were not removed")
        integration = _lock_record(records, "integration-tests", "0.1.0")
        integration_dependencies = list(integration.data.get("dependencies", []))
        if not {"infra-postgres", "migrate"}.issubset(integration_dependencies):
            raise Refusal("Cargo.lock integration PostgreSQL edges are missing")
        _replace_lock_dependencies(
            integration,
            integration_dependencies,
            [item for item in integration_dependencies if item not in {"health", "infra-postgres", "migrate", "sqlx"}],
        )
        for name, version, expected, replacement in (
            ("bitflags", "2.13.2", ["serde_core"], []),
            ("either", "1.18.0", ["serde"], []),
            ("hashbrown", "0.16.1", ["allocator-api2", "equivalent", "foldhash"], ["foldhash"]),
            ("smallvec", "1.16.1", ["serde"], []),
        ):
            record = _lock_record(records, name, version, registry)
            _replace_lock_dependencies(record, expected, replacement)
    _replace_lock_name(service, inputs.service_name)
    retained_locals = local_names - ({"infra-postgres", "migrate"} if inputs.database == "none" else set())
    retained_locals.remove("service")
    roots = {(inputs.service_name, "0.1.0", ""), *{(name, "0.1.0", "") for name in retained_locals}}
    by_key = {record.key: record for record in records}
    reachable: set[tuple[str, str, str]] = set()
    pending = list(roots)
    while pending:
        key = pending.pop()
        if key in reachable:
            continue
        record = by_key.get(key)
        if record is None:
            raise Refusal("Cargo.lock retained package is missing")
        reachable.add(key)
        for dependency in record.data.get("dependencies", []):
            pending.append(_dependency_key(dependency, records))
    retained = [record for record in records if record.key in reachable]
    lock.write_text(header + "".join("[[package]]\n" + record.body for record in retained), encoding="utf-8")


def _collect_plan(
    staged: Path, root: Path, owned_removals: Sequence[str]
) -> tuple[list[PlannedWrite | PlannedLink], list[str]]:
    writes: list[PlannedWrite | PlannedLink] = []
    staged_paths: set[str] = set()
    tracked = {
        raw_path.decode("utf-8", "surrogateescape")
        for raw_path in git(root, ["ls-files", "-z"]).split(b"\0")
        if raw_path
    }
    for path in staged.rglob("*"):
        relative = path.relative_to(staged).as_posix()
        if path.is_symlink():
            if relative not in tracked:
                raise Refusal(f"staged generation produced an untracked output: {relative}")
            staged_paths.add(relative)
            target = root / relative
            destination = os.readlink(path)
            if target.is_symlink() and os.readlink(target) == destination:
                continue
            writes.append(PlannedLink(relative, destination))
            continue
        if path.is_dir():
            continue
        if relative not in tracked:
            raise Refusal(f"staged generation produced an untracked output: {relative}")
        staged_paths.add(relative)
        target = root / relative
        mode = path.stat().st_mode
        contents = path.read_bytes()
        destination_mode = target.stat().st_mode if target.exists() and not target.is_symlink() else None
        expected_mode = 0o755 if mode & stat.S_IXUSR else 0o644
        if (
            destination_mode is not None
            and stat.S_ISREG(destination_mode)
            and target.read_bytes() == contents
            and (destination_mode & 0o777) == expected_mode
        ):
            continue
        writes.append(PlannedWrite(relative, contents, expected_mode))
    removals: list[str] = []
    for relative in sorted(tracked):
        if relative not in staged_paths:
            removals.append(relative)
    # Owned directories refuse unadmitted contents. Ancestors outside those
    # owners are pruned only when the admitted child removals leave them empty;
    # unrelated consumer files in a product root remain outside the plan.
    owned_directories = tuple(item.rstrip("/") for item in owned_removals if item.endswith("/"))
    candidate_directories: set[str] = set()
    for relative in removals:
        parent = Path(relative).parent
        while parent != Path("."):
            candidate = parent.as_posix()
            if not any(path.startswith(f"{candidate}/") for path in staged_paths):
                candidate_directories.add(candidate)
            parent = parent.parent
    removal_set = set(removals)
    for relative in sorted(candidate_directories, key=lambda item: (len(Path(item).parts), item), reverse=True):
        directory = root / relative
        owned = any(relative == owner or relative.startswith(f"{owner}/") for owner in owned_directories)
        if owned or all(child.relative_to(root).as_posix() in removal_set for child in directory.iterdir()):
            removals.append(relative)
            removal_set.add(relative)
    return sorted(writes, key=lambda item: item.relative), removals


def initialize(arguments: argparse.Namespace) -> int:
    inputs = parse_inputs(arguments)
    root = git_root(arguments.repo)
    existing = load_lock(root)
    if existing is not None:
        return _replay(root, inputs)
    _tracked_checkout_is_clean(root)
    revision = git_head(root)
    with tempfile.TemporaryDirectory(prefix="template-init-") as temporary:
        staged = Path(temporary) / "snapshot"
        snapshot_tree(root, staged, revision)
        profiles = _profile_data(staged)
        _preflight_staged(staged, inputs, profiles)
        _postconditions(staged, inputs, profiles, initial=True)
        owned_removals = list(profiles.source_only)
        if inputs.database == "none":
            owned_removals.extend(profiles.remove_when_none)
        for adapter, pack in ADAPTERS.items():
            if adapter not in selected_adapters(inputs.agent_harness):
                owned_removals.extend((*pack.canonical, *pack.generated, *pack.settings))
        writes, removals = _collect_plan(staged, root, owned_removals)
        planned_paths = [write.relative for write in writes] + removals + [LOCK_NAME]
        _target_overlap_is_clean(root, planned_paths)
        admit_plan(root, writes, removals)
        admit_plan(root, [PlannedWrite(LOCK_NAME, _lock_bytes(inputs, revision, "incomplete"), 0o644)])
        lock_path = root / LOCK_NAME
        atomic_write(lock_path, _lock_bytes(inputs, revision, "incomplete"), 0o644)
        try:
            write_plan(root, writes, removals)
            _postconditions(root, inputs, profiles, initial=True)
            atomic_write(lock_path, _lock_bytes(inputs, revision, "complete"), 0o644)
            if load_lock(root, required=True) != _lock(inputs, revision, "complete"):
                raise Refusal("completed template.lock readback failed")
        except Exception:
            print("template init: partial initialization; inspect the diff and use a fresh template checkout", file=sys.stderr)
            raise
    print("template init: initialized; template.lock records initialization only")
    return 0


def build_parser() -> argparse.ArgumentParser:
    class SingleValue(argparse.Action):
        def __call__(self, parser: argparse.ArgumentParser, namespace: argparse.Namespace, values: object, option_string: str | None = None) -> None:
            if getattr(namespace, self.dest, None) is not None:
                raise argparse.ArgumentError(self, f"{option_string} may be supplied once")
            setattr(namespace, self.dest, values)

    parser = argparse.ArgumentParser(description="initialize one derived service from this template")
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--service-name", action=SingleValue)
    parser.add_argument("--repository", action=SingleValue)
    parser.add_argument("--description", action=SingleValue)
    parser.add_argument("--codeowner", action=SingleValue)
    parser.add_argument("--database", action=SingleValue)
    parser.add_argument("--agent-harness", action=SingleValue)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = build_parser().parse_args(argv)
    try:
        return initialize(arguments)
    except (Refusal, ToolFailure) as error:
        print(f"template init: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
