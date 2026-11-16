#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Sequence

sys.dont_write_bytecode = True

_SCRIPT_DIR = Path(__file__).resolve().parent
if os.fspath(_SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, os.fspath(_SCRIPT_DIR))

from state import (  # noqa: E402
    ADAPTERS,
    REQUIRED_AUTHORITIES,
    PlannedLink,
    PlannedWrite,
    Refusal,
    ToolFailure,
    _contains_profile_marker,
    _manifest_files,
    _protected_manifest_owner,
    _regular_file,
    admit_plan,
    git_head,
    git_root,
    is_bytecode_path,
    load_lock,
    manifest_entries,
    parse_manifest,
    safe_relative,
    selected_adapters,
    snapshot_tree,
    verify_plan,
    write_plan,
)


SOURCE_HELPERS = (
    "scripts/template/sync.py",
    "scripts/template/state.py",
    "scripts/harness/sync.py",
)
PROFILE_PROJECTION_HELPERS = ("scripts/template/init.py", "scripts/template/profiles.json")
_INSTRUCTION_PREFIXES = ("docs/",)
_RULE_HEADER = re.compile(r"^\s*([^#\s][^:=]*?)\s*:(?![=])")
_ASSIGNMENT = re.compile(r"^\s*[A-Za-z_][A-Za-z0-9_.-]*\s*(?::=|\?=|\+=|!=|=)")


@dataclass(frozen=True)
class Scope:
    relative: str
    directory: bool


@dataclass(frozen=True)
class Node:
    kind: str
    mode: int
    data: bytes | None = None
    link_target: str | None = None


def _relative(path: Path, root: Path) -> str:
    return path.relative_to(root).as_posix()


def _path_exists(path: Path) -> bool:
    return path.exists() or path.is_symlink()


def _link_target(relative: str, path: Path) -> str | None:
    if not path.is_symlink():
        return None
    try:
        target = os.readlink(path)
    except OSError as error:
        raise Refusal(f"cannot read generated link: {relative}") from error
    for prefix in (".claude/skills/", ".qwen/skills/"):
        if relative.startswith(prefix):
            name = relative.removeprefix(prefix)
            if "/" not in name and target == f"../../.agents/skills/{name}":
                return target
    return None


def _node(root: Path, relative: str, *, links: bool) -> Node | None:
    path = root / relative
    if not _path_exists(path):
        return None
    mode = path.lstat().st_mode
    if stat.S_ISREG(mode):
        return Node("file", stat.S_IMODE(mode), path.read_bytes())
    if stat.S_ISDIR(mode):
        return Node("directory", stat.S_IMODE(mode))
    if stat.S_ISLNK(mode) and links:
        target = _link_target(relative, path)
        if target is not None:
            return Node("link", 0, link_target=target)
    raise Refusal(f"unsafe selected path: {relative}")


def _tree(root: Path, scope: Scope, *, links: bool) -> dict[str, Node]:
    safe_relative(scope.relative, allow_directory=scope.directory)
    stem = scope.relative.rstrip("/")
    first = _node(root, stem, links=links)
    if first is None:
        return {}
    if scope.directory != (first.kind == "directory"):
        raise Refusal(f"selected path has an unexpected type: {stem}")
    result = {stem: first}
    if not scope.directory:
        return result

    def visit(path: Path) -> None:
        for child in sorted(path.iterdir(), key=lambda item: item.name):
            relative = _relative(child, root)
            if not child.is_symlink() and is_bytecode_path(relative):
                continue
            node = _node(root, relative, links=links)
            assert node is not None
            result[relative] = node
            if node.kind == "directory":
                visit(child)

    visit(root / stem)
    return result


def _under(relative: str, root: str) -> bool:
    stem = root.rstrip("/")
    return relative == stem or relative.startswith(f"{stem}/")


def _scope_matches(relative: str, scopes: Iterable[Scope]) -> bool:
    return any(_under(relative, scope.relative) for scope in scopes)


def _read_git(root: Path, args: Sequence[str]) -> bytes:
    try:
        result = subprocess.run(
            ["git", "-C", os.fspath(root), *args],
            capture_output=True,
            check=False,
            env={**os.environ, "GIT_OPTIONAL_LOCKS": "0"},
        )
    except OSError as error:
        raise ToolFailure("git is unavailable") from error
    if result.returncode:
        raise Refusal("git admission failed")
    return result.stdout


def _status_paths(root: Path) -> tuple[str, ...]:
    raw = _read_git(root, ["status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignored=matching"])
    records = (record for record in raw.split(b"\0") if record)
    paths: list[str] = []
    for record in records:
        if len(record) < 4 or record[2:3] != b" ":
            raise Refusal("git status returned an invalid record")
        status = record[:2]
        path = record[3:].decode("utf-8", "surrogateescape")
        if status == b"!!" and is_bytecode_path(path):
            continue
        paths.append(path)
        if status[:1] in {b"R", b"C"} or status[1:] in {b"R", b"C"}:
            original = next(records, None)
            if original is None:
                raise Refusal("git status returned an incomplete rename")
            paths.append(original.decode("utf-8", "surrogateescape"))
    return tuple(paths)


def _check_ignored_destinations(root: Path, paths: Iterable[str]) -> None:
    ordered = sorted(set(paths))
    if not ordered:
        return
    for relative in ordered:
        safe_relative(relative)
    payload = b"\0".join(relative.encode("utf-8") for relative in ordered) + b"\0"
    try:
        result = subprocess.run(
            ["git", "-C", os.fspath(root), "check-ignore", "--no-index", "--stdin", "-z"],
            input=payload,
            capture_output=True,
            check=False,
            env={**os.environ, "GIT_OPTIONAL_LOCKS": "0"},
        )
    except OSError as error:
        raise ToolFailure("git is unavailable") from error
    if result.returncode == 0:
        matched = next(
            (entry.decode("utf-8", "surrogateescape") for entry in result.stdout.split(b"\0") if entry),
            ordered[0],
        )
        raise Refusal(f"ignored destination overlaps selected sync path: {matched}")
    if result.returncode != 1:
        raise Refusal("git ignore admission failed")


def _adapter_scopes(adapters: Iterable[str], field: str) -> tuple[Scope, ...]:
    result: list[Scope] = []
    for adapter in adapters:
        values = getattr(ADAPTERS[adapter], field)
        result.extend(Scope(value, value.endswith("/")) for value in values)
    return tuple(result)


def _instruction_entry(entry: str, selected: set[str]) -> bool:
    plain = entry.rstrip("/")
    if plain == "AGENTS.md" or plain.startswith(_INSTRUCTION_PREFIXES):
        return True
    if plain.startswith(".agents/adapters/"):
        return any(plain in ADAPTERS[adapter].canonical for adapter in selected)
    if plain.startswith(".agents/"):
        return plain != ".agents/codex-project.toml" or "codex" in selected
    return any(plain in ADAPTERS[adapter].canonical for adapter in selected)


def _selected_manifest(snapshot: Path, manifest: Sequence[str], selected: set[str], instructions_only: bool) -> tuple[Scope, ...]:
    canonical = {path for adapter in selected for path in ADAPTERS[adapter].canonical}
    result: list[Scope] = []
    for entry in manifest:
        plain = entry.rstrip("/")
        adapter_owned = any(plain in pack.canonical for pack in ADAPTERS.values())
        if adapter_owned and plain not in canonical:
            continue
        if instructions_only and not _instruction_entry(entry, selected):
            continue
        result.append(Scope(entry, entry.endswith("/")))
    if not result:
        raise Refusal("selected sync mode has no manifest owners")
    for scope in result:
        if not _tree(snapshot, scope, links=False):
            raise Refusal(f"selected source owner is missing: {scope.relative}")
    return tuple(result)


def _validate_source_inputs(snapshot: Path, selected: set[str], manifest: Sequence[str]) -> None:
    listed = {entry.rstrip("/") for entry in manifest}
    for helper in SOURCE_HELPERS:
        _regular_file(snapshot / helper, helper)
    for adapter in selected:
        for path in ADAPTERS[adapter].canonical:
            _regular_file(snapshot / path, path)
            if path not in listed:
                raise Refusal(f"adapter canonical source is absent from manifest: {path}")
    skills = snapshot / ".agents/skills"
    if not skills.is_dir() or skills.is_symlink():
        raise Refusal("source canonical skills directory is unsafe")
    for entry in skills.iterdir():
        if entry.is_dir() and not entry.is_symlink() and _path_exists(entry / ".service-owned"):
            raise Refusal("source canonical skills must not reserve service-owned content")


def _project_portable(snapshot: Path, target_lock: dict) -> bool:
    source_lock = load_lock(snapshot)
    if source_lock is not None:
        # Initialized sources cannot recover sections removed by their profile.
        if any(
            source_lock["profiles"][name] != value
            for name, value in target_lock["profiles"].items()
            if name != "agent_harness"
        ):
            raise Refusal("initialized sync source has different capability profiles")
        return False
    if not any(
        _contains_profile_marker(path.read_bytes())
        for entry in manifest_entries(snapshot)
        for path in _manifest_files(snapshot, entry)
    ):
        # Older portable sources were already pure and have no renderer API.
        return False
    for helper in PROFILE_PROJECTION_HELPERS:
        _regular_file(snapshot / helper, helper)
    # Load the admitted source's initializer and state together, never a derived
    # target's stale one-shot helper. No Cargo, identity rewrite, or pack removal
    # belongs to this portable projection.
    script = (
        "import sys; from pathlib import Path; "
        "sys.path.insert(0, '.'); "
        "from init import project_portable; "
        "from state import parse_json_bytes, validate_lock; "
        "project_portable(Path(sys.argv[1]), "
        "validate_lock(parse_json_bytes(sys.stdin.buffer.read(), 'target lock')))"
    )
    try:
        result = subprocess.run(
            [sys.executable, "-B", "-c", script, os.fspath(snapshot)],
            cwd=snapshot / "scripts/template",
            input=json.dumps(target_lock).encode("utf-8"),
            capture_output=True,
            check=False,
        )
    except OSError as error:
        raise ToolFailure("committed profile projection helper is unavailable") from error
    if result.returncode:
        raise Refusal("committed profile projection refused portable source")
    return True


def _projection_commands(
    snapshot: Path, root: Path, harness: str, mode: str, *, require_materialized: bool = False
) -> None:
    command = [
        sys.executable,
        "-B",
        os.fspath(snapshot / "scripts/harness/sync.py"),
        f"--{mode}",
        "--repo",
        os.fspath(root),
        "--harness",
        harness,
    ]
    if require_materialized and mode == "check":
        command.append("--require-materialized")
    try:
        result = subprocess.run(command, cwd=snapshot, capture_output=True, check=False)
    except OSError as error:
        raise ToolFailure("required snapshot helper is unavailable") from error
    if result.returncode:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        suffix = f": {detail}" if detail else ""
        raise Refusal(f"snapshot helper refused selected projection: scripts/harness/sync.py{suffix}")


def _skill_frontmatter_is_valid(path: Path, name: str) -> bool:
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return False
    if not text.startswith("---\n"):
        return False
    end = text.find("\n---\n", 4)
    if end < 0:
        return False
    fields: dict[str, str] = {}
    for line in text[4:end].splitlines():
        if not line.strip() or line.startswith((" ", "\t")):
            continue
        key, separator, value = line.partition(":")
        if not separator or not key.strip() or key.strip() in fields:
            return False
        fields[key.strip()] = value.strip().strip('"')
    return fields.get("name") == name and bool(fields.get("description"))


def _service_skills(root: Path, source_names: set[str]) -> set[str]:
    skills = root / ".agents/skills"
    if not _path_exists(skills):
        return set()
    if skills.is_symlink() or not skills.is_dir():
        raise Refusal("target canonical skills directory is unsafe")
    reserved: set[str] = set()
    for entry in skills.iterdir():
        marker = entry / ".service-owned"
        if not _path_exists(marker):
            continue
        name = entry.name
        if name in source_names:
            raise Refusal(f"service-owned skill collides with a template skill: {name}")
        if entry.is_symlink() or not entry.is_dir():
            raise Refusal(f"service-owned skill is not a real directory: {name}")
        if marker.is_symlink() or not marker.is_file() or marker.read_bytes():
            raise Refusal(f"service-owned marker is invalid: {name}")
        skill = entry / "SKILL.md"
        if skill.is_symlink() or not skill.is_file() or not _skill_frontmatter_is_valid(skill, name):
            raise Refusal(f"service-owned skill metadata is invalid: {name}")
        for descendant in entry.rglob("*"):
            if descendant.is_symlink():
                raise Refusal(f"service-owned skill contains a symlink: {name}")
        reserved.add(name)
    return reserved


def _is_reserved_skill_path(relative: str, names: set[str]) -> bool:
    return any(_under(relative, f".agents/skills/{name}") for name in names)


def _is_reserved_generated_link(root: Path, relative: str, names: set[str]) -> bool:
    for prefix in (".claude/skills/", ".qwen/skills/"):
        if relative.startswith(prefix):
            name = relative.removeprefix(prefix)
            return name in names and _link_target(relative, root / relative) is not None
    return False


def _remove_private(path: Path) -> None:
    if path.is_symlink() or path.is_file():
        path.unlink()
    elif path.is_dir():
        shutil.rmtree(path)


def _prune_empty_dirs(directory: Path) -> None:
    if not directory.is_dir() or directory.is_symlink():
        return
    for child in sorted(directory.iterdir()):
        if child.is_dir() and not child.is_symlink():
            _prune_empty_dirs(child)
    if not any(directory.iterdir()):
        directory.rmdir()


def _copy_manifest_to_stage(snapshot: Path, stage: Path, entries: Sequence[Scope], reserved_skills: set[str]) -> None:
    for scope in entries:
        source_nodes = _tree(snapshot, scope, links=False)
        target_nodes = _tree(stage, scope, links=False)
        for relative in set(source_nodes).intersection(target_nodes):
            if source_nodes[relative].kind != target_nodes[relative].kind:
                raise Refusal(f"selected target path has a type collision: {relative}")
        for relative in sorted(set(target_nodes) - set(source_nodes), key=lambda item: item.count("/"), reverse=True):
            if _is_reserved_skill_path(relative, reserved_skills):
                continue
            path = stage / relative
            if _path_exists(path):
                _remove_private(path)
        for relative, node in sorted(source_nodes.items(), key=lambda item: item[0].count("/")):
            destination = stage / relative
            if node.kind == "directory":
                destination.mkdir(parents=True, exist_ok=True)
                os.chmod(destination, node.mode)
        for relative, node in source_nodes.items():
            if node.kind != "file":
                continue
            destination = stage / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(node.data or b"")
            os.chmod(destination, node.mode)


def _remove_unselected_from_stage(stage: Path, selected: set[str]) -> None:
    for adapter, pack in ADAPTERS.items():
        if adapter in selected:
            continue
        for relative in (*pack.canonical, *pack.generated, *pack.settings):
            path = stage / relative.rstrip("/")
            if _path_exists(path):
                _remove_private(path)
    _prune_empty_dirs(stage / ".agents/adapters")


def _required_authorities(root: Path) -> None:
    for relative in REQUIRED_AUTHORITIES:
        _regular_file(root / relative, f"required local authority {relative}")


def _standard_targets(snapshot: Path) -> set[str]:
    path = snapshot / "make/template.mk"
    _regular_file(path, "make/template.mk")
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError as error:
        raise Refusal("make/template.mk is not UTF-8") from error
    lines = text.splitlines()
    start = next((index for index, line in enumerate(lines) if re.match(r"^TEMPLATE_STANDARD_TARGETS\s*:=", line)), None)
    if start is None:
        raise Refusal("make/template.mk has no static TEMPLATE_STANDARD_TARGETS registry")
    pieces: list[str] = []
    for index in range(start, len(lines)):
        declaration = re.match(r"^TEMPLATE_STANDARD_TARGETS\s*(:=|\+=)\s*(.*)$", lines[index])
        if declaration is None:
            continue
        if declaration.group(1) == ":=" and index != start:
            raise Refusal("TEMPLATE_STANDARD_TARGETS has more than one owner")
        pieces.append(declaration.group(2))
        while pieces[-1].rstrip().endswith("\\"):
            pieces[-1] = pieces[-1].rstrip()[:-1]
            index += 1
            if index >= len(lines):
                raise Refusal("TEMPLATE_STANDARD_TARGETS has an incomplete continuation")
            pieces.append(lines[index].strip())
    raw = " ".join(pieces)
    if "$" in raw:
        raise Refusal("TEMPLATE_STANDARD_TARGETS must be static data")
    targets = set(raw.split())
    if not targets or any(not re.fullmatch(r"[A-Za-z0-9_-]+", value) for value in targets):
        raise Refusal("TEMPLATE_STANDARD_TARGETS is invalid")
    return targets


def _make_rules(path: Path, *, allowed_includes: set[str] | None = None) -> set[str]:
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except UnicodeDecodeError as error:
        raise Refusal(f"{path.name} is not UTF-8") from error
    rules: set[str] = set()
    for line in lines:
        stripped = line.strip()
        if not stripped or stripped.startswith("#") or line.startswith("\t"):
            continue
        # `eval` is executable Make syntax even when it appears inside an
        # otherwise-supported variable assignment.  Reject it before treating
        # that line as inert service data.
        if "$(eval" in line or "${eval" in line:
            raise Refusal(f"{path.name} contains an unsafe evaluation directive")
        if _ASSIGNMENT.match(line):
            continue
        if re.match(r"^(?:-?include|sinclude)\s+", stripped):
            if allowed_includes is None or stripped not in allowed_includes:
                raise Refusal(f"{path.name} contains an unsafe include directive")
            continue
        matched = _RULE_HEADER.match(line)
        if matched is None:
            continue
        if "$" in matched.group(1):
            raise Refusal(f"{path.name} contains a nonstatic target definition")
        for target in matched.group(1).split():
            if target != ".PHONY":
                rules.add(target)
    return rules


def _admit_make(snapshot: Path, target: Path) -> None:
    standard = _standard_targets(snapshot)
    service = target / "make/service.mk"
    _regular_file(service, "make/service.mk")
    service_rules = _make_rules(service)
    if any("%" in rule or rule in standard for rule in service_rules):
        raise Refusal("make/service.mk overrides or patterns a portable standard target")
    root_make = target / "Makefile"
    _regular_file(root_make, "Makefile")
    root_rules = _make_rules(
        root_make,
        allowed_includes={"include make/service.mk", "include make/template.mk", "-include make/source.mk"},
    )
    if any("%" in rule or rule not in standard for rule in root_rules):
        raise Refusal("Makefile contains unsplit service recipes")
    text = root_make.read_text(encoding="utf-8")
    if "include make/service.mk" not in text or "include make/template.mk" not in text:
        raise Refusal("Makefile does not use the supported service/template split")


def _scope_shape(root: Path, scopes: Iterable[Scope], *, links: bool) -> None:
    for scope in scopes:
        stem = scope.relative.rstrip("/")
        parent = root
        for part in Path(stem).parts[:-1]:
            parent = parent / part
            if parent.is_symlink():
                raise Refusal(f"selected path has a symlink parent: {stem}")
            if _path_exists(parent) and not parent.is_dir():
                raise Refusal(f"selected path has a non-directory parent: {stem}")
        node = _node(root, stem, links=links)
        if node is not None and (node.kind == "directory") != scope.directory:
            raise Refusal(f"selected path has an unexpected type: {stem}")


def _target_dirty(root: Path, scopes: Sequence[Scope], service_skills: set[str]) -> None:
    for relative in _status_paths(root):
        preserved = _is_reserved_skill_path(relative, service_skills) or _is_reserved_generated_link(root, relative, service_skills)
        if _scope_matches(relative, scopes) and not preserved:
            raise Refusal(f"dirty selected target path: {relative}")
    _check_ignored_destinations(root, [scope.relative.rstrip("/") for scope in scopes])


def _source_dirty(root: Path, scopes: Sequence[Scope]) -> None:
    for relative in _status_paths(root):
        if _scope_matches(relative, scopes):
            raise Refusal(f"dirty selected source path: {relative}")


def _admit_target_layout(target: Path, target_lock: dict) -> None:
    for old_hook, new_hook in (
        ("scripts/ci/service-changed-surfaces.sh", "scripts/service/changed-surfaces.sh"),
        ("scripts/ci/service-database.sh", "scripts/service/database.sh"),
        ("scripts/ci/service-runtime-image.sh", "scripts/service/runtime-image.sh"),
        ("scripts/ci/service-runtime-binaries.json", "scripts/service/runtime-binaries.json"),
    ):
        if _path_exists(target / old_hook):
            raise Refusal(f"legacy service hook {old_hook} must move to {new_hook} before full sync")

    profiles = target_lock["profiles"]
    if profiles["database"] == "postgres" and not (target / "scripts/profiles/migration_history.sh").is_file():
        raise Refusal("retained postgres profile requires scripts/profiles/ migration before full sync")
    if profiles["grpc"] == "enabled" and not (target / "scripts/profiles/grpc_check.sh").is_file():
        raise Refusal("retained grpc profile requires scripts/profiles/ migration before full sync")
    if (
        any(
            (
                profiles["database"] == "postgres",
                profiles["messaging"] == "nats-jetstream",
                profiles["cache"] == "redis",
                profiles["object_storage"] == "s3",
                profiles["outbound_auth"] == "oauth2-client-credentials",
            )
        )
        and not (target / "scripts/profiles/integration.sh").is_file()
    ):
        raise Refusal("retained container integration profile requires scripts/profiles/integration.sh before full sync")


def _retired_manifest_scopes(
    target: Path, active_manifest: Sequence[Scope], *, instructions_only: bool
) -> tuple[Scope, ...]:
    if instructions_only or not (target / "template-owned.paths").is_file():
        return ()
    active_stems = [scope.relative.rstrip("/") for scope in active_manifest]
    retired: list[Scope] = []
    for entry in manifest_entries(target):
        stem = entry.rstrip("/")
        if any(stem == active or stem.startswith(f"{active}/") for active in active_stems):
            continue
        if _protected_manifest_owner(entry):
            continue
        retired.append(Scope(entry, entry.endswith("/")))
    return tuple(retired)


def _plan_scopes(
    manifest: Sequence[Scope],
    selected: set[str],
    instructions_only: bool,
    retired: Sequence[Scope] = (),
) -> tuple[Scope, ...]:
    scopes = list(manifest)
    scopes.extend(retired)
    scopes.extend(_adapter_scopes(selected, "generated"))
    scopes.extend(_adapter_scopes(selected, "settings"))
    if not instructions_only:
        unselected = set(ADAPTERS) - selected
        scopes.extend(_adapter_scopes(unselected, "canonical"))
        scopes.extend(_adapter_scopes(unselected, "generated"))
        scopes.extend(_adapter_scopes(unselected, "settings"))
    unique: dict[str, Scope] = {}
    for scope in scopes:
        existing = unique.get(scope.relative)
        if existing is not None and existing != scope:
            raise Refusal("selected sync ownership has a type collision")
        unique[scope.relative] = scope
    return tuple(unique.values())


def _stage_target(target: Path, destination: Path) -> None:
    def _ignore(_root: str, names: list[str]) -> set[str]:
        return {
            name
            for name in names
            if name in {".git", "__pycache__"} or name.endswith((".pyc", ".pyo", ".pyd"))
        }

    try:
        shutil.copytree(target, destination, symlinks=True, ignore=_ignore)
    except OSError as error:
        raise ToolFailure("cannot create private sync staging tree") from error


def _build_plan(target: Path, stage: Path, scopes: Sequence[Scope]) -> tuple[list[PlannedWrite | PlannedLink], list[str], list[tuple[str, int]]]:
    actual: dict[str, Node] = {}
    expected: dict[str, Node] = {}
    for scope in scopes:
        actual.update(_tree(target, scope, links=True))
        expected.update(_tree(stage, scope, links=True))
    writes: list[PlannedWrite | PlannedLink] = []
    removals: list[str] = []
    directory_modes: list[tuple[str, int]] = []
    for relative in sorted(set(actual) | set(expected)):
        before, after = actual.get(relative), expected.get(relative)
        if after is None:
            if before is not None:
                removals.append(relative)
            continue
        if before is not None and before.kind != after.kind:
            raise Refusal(f"selected path has a type collision: {relative}")
        if after.kind == "directory":
            if before is None or before.mode != after.mode:
                directory_modes.append((relative, after.mode))
            continue
        if after.kind == "file" and (before is None or before.data != after.data or before.mode != after.mode):
            writes.append(PlannedWrite(relative, after.data or b"", after.mode))
        if after.kind == "link" and (before is None or before.link_target != after.link_target):
            writes.append(PlannedLink(relative, after.link_target or ""))
    return writes, removals, directory_modes


def _create_and_mode_directories(root: Path, directories: Sequence[tuple[str, int]]) -> None:
    for relative, mode in sorted(directories, key=lambda item: item[0].count("/")):
        path = root / relative
        if _path_exists(path):
            if path.is_symlink() or not path.is_dir():
                raise Refusal(f"selected path has a type collision: {relative}")
        else:
            path.mkdir(parents=True, exist_ok=False)
        os.chmod(path, mode)


def _render(
    snapshot: Path,
    target: Path,
    stage: Path,
    manifest: Sequence[Scope],
    selected: set[str],
    harness: str,
    retired: Sequence[Scope] = (),
) -> None:
    source_names = {entry.name for entry in (snapshot / ".agents/skills").iterdir() if entry.is_dir() and not entry.is_symlink()}
    reserved = _service_skills(target, source_names)
    _stage_target(target, stage)
    _remove_unselected_from_stage(stage, selected)
    for scope in retired:
        candidate = stage / scope.relative.rstrip("/")
        if _path_exists(candidate):
            _remove_private(candidate)
    _prune_empty_dirs(stage / "scripts")
    _copy_manifest_to_stage(snapshot, stage, manifest, reserved)
    _projection_commands(snapshot, stage, harness, "apply")
    _projection_commands(snapshot, stage, harness, "check", require_materialized=True)


def _source_scopes(
    manifest: Sequence[Scope], *, profile_projection: bool = False
) -> tuple[Scope, ...]:
    scopes = list(manifest)
    scopes.extend(Scope(path, False) for path in SOURCE_HELPERS)
    if profile_projection:
        scopes.extend(Scope(path, False) for path in PROFILE_PROJECTION_HELPERS)
    scopes.append(Scope("template.lock", False))
    return tuple(scopes)


def _run(arguments: argparse.Namespace) -> int:
    source = git_root(Path(arguments.source))
    target = git_root(Path(arguments.repo))
    if source == target:
        raise Refusal("source and target must be distinct Git roots")
    if source.is_relative_to(target) or target.is_relative_to(source):
        raise Refusal("source and target Git roots may not overlap")

    revision = git_head(source)
    with tempfile.TemporaryDirectory(prefix="template-sync-") as temporary:
        work = Path(temporary)
        snapshot = work / "source"
        captured = snapshot_tree(source, snapshot, revision)
        if captured != revision:
            raise Refusal("source snapshot revision changed during admission")
        target_lock = load_lock(target, required=True)
        assert target_lock is not None
        if target_lock["state"] != "complete":
            raise Refusal("template.lock is incomplete; inspect the init-produced diff and use a fresh template checkout")
        harness = target_lock["profiles"]["agent_harness"]
        selected = set(selected_adapters(harness))
        source_lock = load_lock(snapshot)
        source_harness = source_lock["profiles"]["agent_harness"] if source_lock is not None else "all"
        profile_projection = _project_portable(snapshot, target_lock)
        manifest = parse_manifest(
            snapshot,
            harness=harness,
            target_repository=target_lock["identity"]["repository"],
        )
        chosen_manifest = _selected_manifest(snapshot, manifest, selected, arguments.instructions_only)
        _validate_source_inputs(snapshot, selected, manifest)
        _projection_commands(snapshot, snapshot, source_harness, "check")
        source_scopes = _source_scopes(chosen_manifest, profile_projection=profile_projection)
        _source_dirty(source, source_scopes)
        if git_head(source) != revision:
            raise Refusal("source HEAD changed during sync admission")

        _required_authorities(target)
        if not arguments.instructions_only:
            _admit_make(snapshot, target)
            _admit_target_layout(target, target_lock)
        retired_scopes = _retired_manifest_scopes(
            target, chosen_manifest, instructions_only=arguments.instructions_only
        )
        plan_scopes = _plan_scopes(chosen_manifest, selected, arguments.instructions_only, retired_scopes)
        _scope_shape(target, plan_scopes, links=True)
        source_names = {entry.name for entry in (snapshot / ".agents/skills").iterdir() if entry.is_dir() and not entry.is_symlink()}
        reserved = _service_skills(target, source_names)
        _target_dirty(target, plan_scopes, reserved)

        stage = work / "target"
        _render(snapshot, target, stage, chosen_manifest, selected, harness, retired_scopes)
        writes, removals, directory_modes = _build_plan(target, stage, plan_scopes)
        _check_ignored_destinations(
            target,
            [write.relative for write in writes] + removals + [path for path, _mode in directory_modes],
        )
        if arguments.mode == "check":
            if writes or removals or directory_modes:
                for path in sorted({write.relative for write in writes} | set(removals) | {path for path, _mode in directory_modes}):
                    print(f"template sync: drift {path}")
                return 1
            print(f"template sync: source={revision} mode={'instructions-only' if arguments.instructions_only else 'full'} parity")
            return 0

        started = False
        try:
            if writes or removals or directory_modes:
                # State owns file/link/removal admission.  Directories carry
                # source mode too, so their already-admitted type is checked
                # by the selected-tree plan before their mode changes here.
                admit_plan(target, writes, removals)
                started = True
                _create_and_mode_directories(target, directory_modes)
                write_plan(target, writes, removals)
                _prune_empty_dirs(target / "scripts")
                verify_plan(target, writes, removals)
            verify_stage = work / "verify"
            _render(snapshot, target, verify_stage, chosen_manifest, selected, harness, retired_scopes)
            final_writes, final_removals, final_modes = _build_plan(target, verify_stage, plan_scopes)
            if final_writes or final_removals or final_modes:
                raise ToolFailure("admitted sync plan did not reach selected parity")
        except (OSError, Refusal, ToolFailure) as error:
            if started:
                raise ToolFailure("partial sync after admitted writes; inspect the target before retrying") from error
            raise
        print(f"template sync: source={revision} mode={'instructions-only' if arguments.instructions_only else 'full'} applied")
        return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="synchronize one committed portable template snapshot")
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--check", dest="mode", action="store_const", const="check")
    modes.add_argument("--apply", dest="mode", action="store_const", const="apply")
    parser.add_argument("--instructions-only", action="store_true")
    parser.add_argument("--from", dest="source", required=True, type=Path)
    parser.add_argument("--repo", required=True, type=Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    try:
        return _run(build_parser().parse_args(argv))
    except (Refusal, ToolFailure) as error:
        print(f"template sync: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
