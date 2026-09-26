#!/usr/bin/env python3
"""Committed-snapshot synchronization for initialized derived services.

The synchronizer deliberately treats a source revision as data: it materializes
one Git-object snapshot, renders the selected portable result in a private
directory, and admits every target write before it touches the target.  It
never evaluates the target Makefile or a target helper.
"""

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

from template_state import (
    ADAPTERS,
    REQUIRED_AUTHORITIES,
    PlannedLink,
    PlannedWrite,
    Refusal,
    ToolFailure,
    admit_plan,
    git_head,
    git_root,
    load_lock,
    parse_json_bytes,
    parse_manifest,
    safe_relative,
    selected_adapters,
    selected_http_idempotency,
    selected_inbound_webhooks,
    selected_jobs,
    selected_messaging,
    selected_outbox,
    selected_outbound_auth,
    selected_outbound_http,
    selected_profiles,
    selected_webhooks,
    snapshot_tree,
    verify_plan,
    write_plan,
)


SOURCE_HELPERS = (
    "scripts/template-sync.sh",
    "scripts/lib/template_sync.py",
    "scripts/lib/template_state.py",
    "scripts/agent-roles-sync.sh",
    "scripts/codex-agents-sync.sh",
    "scripts/harness-skills-sync.sh",
    "scripts/lib/sync-cli.sh",
)
_INSTRUCTION_PREFIXES = ("docs/",)
_SETTING_LEAVES = {
    "claude": (".claude/settings.json", "env", "CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH"),
    "qwen": (".qwen/settings.json", "model", "maxSubagentDepth"),
}
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


@dataclass(frozen=True)
class JsonNode:
    kind: str
    start: int
    end: int
    members: dict[str, "JsonNode"] | None = None


class _JsonScanner:
    """Small JSON token/span reader after strict parsing has accepted bytes."""

    def __init__(self, text: str) -> None:
        self.text = text
        self.index = 0

    def parse(self) -> JsonNode:
        node = self._value()
        self._space()
        if self.index != len(self.text):
            raise Refusal("settings JSON has trailing data")
        return node

    def _space(self) -> None:
        while self.index < len(self.text) and self.text[self.index] in " \t\r\n":
            self.index += 1

    def _string(self) -> tuple[str, int, int]:
        start = self.index
        if self.index >= len(self.text) or self.text[self.index] != '"':
            raise Refusal("settings JSON has an invalid object member")
        self.index += 1
        while self.index < len(self.text):
            character = self.text[self.index]
            if character == "\\":
                self.index += 2
                continue
            self.index += 1
            if character == '"':
                raw = self.text[start:self.index]
                try:
                    return json.loads(raw), start, self.index
                except json.JSONDecodeError as error:
                    raise Refusal("settings JSON has an invalid string token") from error
        raise Refusal("settings JSON has an unterminated string")

    def _value(self) -> JsonNode:
        self._space()
        start = self.index
        if start >= len(self.text):
            raise Refusal("settings JSON has an incomplete value")
        character = self.text[start]
        if character == "{":
            self.index += 1
            members: dict[str, JsonNode] = {}
            self._space()
            if self.index < len(self.text) and self.text[self.index] == "}":
                self.index += 1
                return JsonNode("object", start, self.index, members)
            while True:
                self._space()
                key, _key_start, _key_end = self._string()
                self._space()
                if self.index >= len(self.text) or self.text[self.index] != ":":
                    raise Refusal("settings JSON has an invalid object member")
                self.index += 1
                value = self._value()
                if key in members:
                    raise Refusal("settings JSON has a duplicate key")
                members[key] = value
                self._space()
                if self.index < len(self.text) and self.text[self.index] == "}":
                    self.index += 1
                    return JsonNode("object", start, self.index, members)
                if self.index >= len(self.text) or self.text[self.index] != ",":
                    raise Refusal("settings JSON has an invalid object separator")
                self.index += 1
        if character == "[":
            self.index += 1
            self._space()
            if self.index < len(self.text) and self.text[self.index] == "]":
                self.index += 1
                return JsonNode("array", start, self.index)
            while True:
                self._value()
                self._space()
                if self.index < len(self.text) and self.text[self.index] == "]":
                    self.index += 1
                    return JsonNode("array", start, self.index)
                if self.index >= len(self.text) or self.text[self.index] != ",":
                    raise Refusal("settings JSON has an invalid array separator")
                self.index += 1
        if character == '"':
            _value, _start, end = self._string()
            return JsonNode("string", start, end)
        for literal, kind in (("true", "literal"), ("false", "literal"), ("null", "literal")):
            if self.text.startswith(literal, start):
                self.index += len(literal)
                return JsonNode(kind, start, self.index)
        while self.index < len(self.text) and self.text[self.index] not in " \t\r\n,]}":
            self.index += 1
        if self.index == start:
            raise Refusal("settings JSON has an invalid value")
        return JsonNode("number", start, self.index)


def _regular(path: Path, label: str) -> None:
    try:
        mode = path.lstat().st_mode
    except FileNotFoundError:
        raise Refusal(f"{label} is missing") from None
    if stat.S_ISLNK(mode) or not stat.S_ISREG(mode):
        raise Refusal(f"{label} is not a regular file")


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
    """Return an lstat-only selected tree, including the directory root."""

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
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
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
    records = [record for record in raw.split(b"\0") if record]
    paths: list[str] = []
    index = 0
    while index < len(records):
        record = records[index]
        if len(record) < 4 or record[2:3] != b" ":
            raise Refusal("git status returned an invalid record")
        status = record[:2]
        path = record[3:].decode("utf-8", "surrogateescape")
        paths.append(path)
        if status[:1] in {b"R", b"C"} or status[1:] in {b"R", b"C"}:
            index += 1
            if index >= len(records):
                raise Refusal("git status returned an incomplete rename")
            paths.append(records[index].decode("utf-8", "surrogateescape"))
        index += 1
    return tuple(paths)


def _check_ignored_destinations(root: Path, paths: Iterable[str]) -> None:
    for relative in sorted(set(paths)):
        safe_relative(relative)
        try:
            result = subprocess.run(
                ["git", "-C", os.fspath(root), "check-ignore", "-q", "--no-index", "--", relative],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
                env={**os.environ, "GIT_OPTIONAL_LOCKS": "0"},
            )
        except OSError as error:
            raise ToolFailure("git is unavailable") from error
        if result.returncode == 0:
            raise Refusal(f"ignored destination overlaps selected sync path: {relative}")
        if result.returncode not in {0, 1}:
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
        _regular(snapshot / helper, helper)
    for adapter in selected:
        for path in ADAPTERS[adapter].canonical:
            _regular(snapshot / path, path)
            if path not in listed:
                raise Refusal(f"adapter canonical source is absent from manifest: {path}")
    skills = snapshot / ".agents/skills"
    if not skills.is_dir() or skills.is_symlink():
        raise Refusal("source canonical skills directory is unsafe")
    for entry in skills.iterdir():
        if entry.is_dir() and not entry.is_symlink() and _path_exists(entry / ".service-owned"):
            raise Refusal("source canonical skills must not reserve service-owned content")


def _run_helper(snapshot: Path, arguments: Sequence[str]) -> None:
    command = ["bash", os.fspath(snapshot / arguments[0]), *arguments[1:]]
    try:
        result = subprocess.run(command, cwd=snapshot, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    except OSError as error:
        raise ToolFailure("required snapshot helper is unavailable") from error
    if result.returncode:
        raise Refusal(f"snapshot helper refused selected projection: {arguments[0]}")


def _projection_commands(snapshot: Path, root: Path, harness: str, mode: str) -> None:
    common = ["--repo", os.fspath(root), "--harness", harness]
    _run_helper(snapshot, ("scripts/agent-roles-sync.sh", f"--{mode}", *common))
    if harness in {"all", "codex"}:
        _run_helper(snapshot, ("scripts/codex-agents-sync.sh", f"--{mode}", *common))
    if harness in {"all", "claude"}:
        _run_helper(snapshot, ("scripts/harness-skills-sync.sh", "claude", f"--{mode}", *common))
    if harness in {"all", "qwen"}:
        _run_helper(snapshot, ("scripts/harness-skills-sync.sh", "qwen", f"--{mode}", *common))


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


def _strict_json(text: str, label: str) -> JsonNode:
    parse_json_bytes(text.encode("utf-8"), label)
    return _JsonScanner(text).parse()


def _setting_source_token(snapshot: Path, adapter: str) -> str:
    relative, parent_key, leaf_key = _SETTING_LEAVES[adapter]
    path = snapshot / relative
    _regular(path, relative)
    try:
        text = path.read_bytes().decode("utf-8")
    except UnicodeDecodeError as error:
        raise Refusal(f"{relative} is not UTF-8") from error
    root = _strict_json(text, relative)
    if root.kind != "object" or root.members is None:
        raise Refusal(f"{relative} root must be an object")
    parent = root.members.get(parent_key)
    leaf = parent.members.get(leaf_key) if parent is not None and parent.kind == "object" and parent.members else None
    if leaf is None:
        raise Refusal(f"{relative} has no managed setting")
    token = text[leaf.start:leaf.end]
    if adapter == "claude":
        if leaf.kind != "string" or re.fullmatch(r'"[0-9]+"', token) is None:
            raise Refusal(f"{relative} managed setting has an invalid type")
    elif leaf.kind != "number" or re.fullmatch(r"[1-9][0-9]*", token) is None:
        raise Refusal(f"{relative} managed setting has an invalid type")
    return token


def _append_member(text: str, node: JsonNode, key: str, token: str) -> str:
    assert node.kind == "object" and node.members is not None
    prefix = "," if node.members else ""
    member = f"{json.dumps(key, ensure_ascii=False, separators=(',', ':'))}:{token}"
    return f"{text[:node.end - 1]}{prefix}{member}{text[node.end - 1:]}"


def _render_setting(snapshot: Path, stage: Path, adapter: str) -> None:
    relative, parent_key, leaf_key = _SETTING_LEAVES[adapter]
    token = _setting_source_token(snapshot, adapter)
    destination = stage / relative
    if not _path_exists(destination):
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes((json.dumps({parent_key: {leaf_key: json.loads(token)}}, indent=2) + "\n").encode("utf-8"))
        return
    _regular(destination, relative)
    try:
        text = destination.read_bytes().decode("utf-8")
    except UnicodeDecodeError as error:
        raise Refusal(f"{relative} is not UTF-8") from error
    root = _strict_json(text, relative)
    if root.kind != "object" or root.members is None:
        raise Refusal(f"{relative} root must be an object")
    parent = root.members.get(parent_key)
    if parent is not None and parent.kind != "object":
        raise Refusal(f"{relative} managed parent must be an object")
    if parent is None:
        rendered = _append_member(text, root, parent_key, "{" + json.dumps(leaf_key) + ":" + token + "}")
    else:
        assert parent.members is not None
        leaf = parent.members.get(leaf_key)
        if leaf is None:
            rendered = _append_member(text, parent, leaf_key, token)
        else:
            # This leaf is the explicit ownership exception.  Its current
            # JSON type/value is managed drift, so replace the whole token;
            # strict parsing above still rejects malformed documents.
            rendered = f"{text[:leaf.start]}{token}{text[leaf.end:]}"
    _strict_json(rendered, relative)
    destination.write_bytes(rendered.encode("utf-8"))


def _required_authorities(root: Path) -> None:
    for relative in REQUIRED_AUTHORITIES:
        _regular(root / relative, f"required local authority {relative}")


def _standard_targets(snapshot: Path) -> set[str]:
    path = snapshot / "make/template.mk"
    _regular(path, "make/template.mk")
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError as error:
        raise Refusal("make/template.mk is not UTF-8") from error
    lines = text.splitlines()
    start = next((index for index, line in enumerate(lines) if re.match(r"^TEMPLATE_STANDARD_TARGETS\s*:=", line)), None)
    if start is None:
        raise Refusal("make/template.mk has no static TEMPLATE_STANDARD_TARGETS registry")
    first = re.match(r"^TEMPLATE_STANDARD_TARGETS\s*:=\s*(.*)$", lines[start])
    assert first is not None
    pieces = [first.group(1)]
    index = start
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
    _regular(service, "make/service.mk")
    service_rules = _make_rules(service)
    if any("%" in rule or rule in standard for rule in service_rules):
        raise Refusal("make/service.mk overrides or patterns a portable standard target")
    root_make = target / "Makefile"
    _regular(root_make, "Makefile")
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


def _plan_scopes(manifest: Sequence[Scope], selected: set[str], instructions_only: bool) -> tuple[Scope, ...]:
    scopes = list(manifest)
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
    try:
        shutil.copytree(target, destination, symlinks=True, ignore=lambda _root, names: {".git"} & set(names))
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


def _render(snapshot: Path, target: Path, stage: Path, manifest: Sequence[Scope], selected: set[str], harness: str) -> None:
    source_names = {entry.name for entry in (snapshot / ".agents/skills").iterdir() if entry.is_dir() and not entry.is_symlink()}
    reserved = _service_skills(target, source_names)
    _stage_target(target, stage)
    _remove_unselected_from_stage(stage, selected)
    _copy_manifest_to_stage(snapshot, stage, manifest, reserved)
    _projection_commands(snapshot, stage, harness, "preflight")
    _projection_commands(snapshot, stage, harness, "apply")
    for adapter in selected.intersection(_SETTING_LEAVES):
        _render_setting(snapshot, stage, adapter)
    _projection_commands(snapshot, stage, harness, "check")


def _source_scopes(manifest: Sequence[Scope], selected: set[str]) -> tuple[Scope, ...]:
    scopes = list(manifest)
    # The source snapshot verifies every committed generator projection before
    # it renders the selected target shape, so none of those inputs may be a
    # dirty worktree view even when this target selected a narrower adapter.
    scopes.extend(_adapter_scopes(ADAPTERS, "generated"))
    scopes.extend(_adapter_scopes(selected, "settings"))
    scopes.extend(Scope(path, False) for path in SOURCE_HELPERS)
    return tuple(scopes)


def _run(arguments: argparse.Namespace) -> int:
    source = git_root(Path(arguments.source))
    target = git_root(Path(arguments.repo))
    if source == target:
        raise Refusal("source and target must be distinct Git roots")
    source_inside_target = False
    target_inside_source = False
    try:
        source.relative_to(target)
        source_inside_target = True
    except ValueError:
        pass
    try:
        target.relative_to(source)
        target_inside_source = True
    except ValueError:
        pass
    if source_inside_target or target_inside_source:
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
        _database, harness = selected_profiles(target)
        selected_outbound_http(target)
        selected_outbound_auth(target)
        selected_http_idempotency(target)
        selected_jobs(target)
        selected_messaging(target)
        selected_outbox(target)
        selected_webhooks(target)
        selected_inbound_webhooks(target)
        selected = set(selected_adapters(harness))
        manifest = parse_manifest(
            snapshot,
            harness=harness,
            target_repository=target_lock["identity"]["repository"],
        )
        chosen_manifest = _selected_manifest(snapshot, manifest, selected, arguments.instructions_only)
        _validate_source_inputs(snapshot, selected, manifest)
        _projection_commands(snapshot, snapshot, "all", "check")
        source_scopes = _source_scopes(chosen_manifest, selected)
        _source_dirty(source, source_scopes)
        if git_head(source) != revision:
            raise Refusal("source HEAD changed during sync admission")

        _required_authorities(target)
        if not arguments.instructions_only:
            _admit_make(snapshot, target)
        plan_scopes = _plan_scopes(chosen_manifest, selected, arguments.instructions_only)
        _scope_shape(target, plan_scopes, links=True)
        source_names = {entry.name for entry in (snapshot / ".agents/skills").iterdir() if entry.is_dir() and not entry.is_symlink()}
        reserved = _service_skills(target, source_names)
        _target_dirty(target, plan_scopes, reserved)

        stage = work / "target"
        _render(snapshot, target, stage, chosen_manifest, selected, harness)
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
                verify_plan(target, writes, removals)
            verify_stage = work / "verify"
            _render(snapshot, target, verify_stage, chosen_manifest, selected, harness)
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
