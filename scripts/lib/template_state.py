#!/usr/bin/env python3
"""Shared state, snapshot, and admission primitives for template lifecycle tools.

The initializer and synchronizer deliberately share this small stdlib-only
surface.  It owns format and filesystem admission, while each command owns its
own transformations and user-facing operation semantics.
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
import unicodedata
from collections.abc import Iterable, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit


LOCK_NAME = "template.lock"
LOCK_SCHEMA_VERSION = 1
TEMPLATE_REPOSITORY = "https://github.com/Dankosik/rust-service-template-rest"
DATABASE_CHOICES = ("none", "postgres")
AUTHN_CHOICES = ("none", "oidc-jwt", "oidc-introspection")
OUTBOUND_HTTP_CHOICES = ("none", "bounded")
HTTP_IDEMPOTENCY_CHOICES = ("none", "postgres")
HARNESS_CHOICES = ("core", "codex", "claude", "qwen", "cursor", "grok", "opencode", "all")


@dataclass(frozen=True)
class AdapterPack:
    """The selected-adapter paths which are not ordinary manifest ownership."""

    canonical: tuple[str, ...]
    generated: tuple[str, ...]
    settings: tuple[str, ...] = ()


# The manifest remains the sole whole-file full-sync authority.  These are
# generator inputs/projections and lexical settings exceptions only.
ADAPTERS: dict[str, AdapterPack] = {
    "codex": AdapterPack(
        canonical=(".agents/codex-project.toml",),
        generated=(".codex/config.toml", ".codex/agents/"),
    ),
    "claude": AdapterPack(
        canonical=("CLAUDE.md",),
        generated=(".claude/agents/", ".claude/skills/"),
        settings=(".claude/settings.json",),
    ),
    "qwen": AdapterPack(
        canonical=("QWEN.md",),
        generated=(".qwen/agents/", ".qwen/skills/"),
        settings=(".qwen/settings.json",),
    ),
    "cursor": AdapterPack(
        canonical=(".cursor/rules/agent-harness.mdc",),
        generated=(".cursor/agents/",),
    ),
    "grok": AdapterPack(
        canonical=("Grok.md", ".grok/rules/harness.md"),
        generated=(".grok/agents/", ".grok/roles/"),
    ),
    "opencode": AdapterPack(
        canonical=(
            "opencode.json",
            ".opencode/rules/harness.md",
            ".opencode/commands/orchestrator.md",
            ".opencode/plugins/task-subagents.js",
        ),
        generated=(".opencode/agents/",),
    ),
}

# These remain deliberately small.  They are service-owned documents that the
# portable agent and validation methods refer to for concrete local facts.
REQUIRED_AUTHORITIES = frozenset(
    {
        "Cargo.toml",
        "rust-toolchain.toml",
        "docs/repo-architecture.md",
        "docs/architecture/boundaries.md",
        "docs/architecture/persistence.md",
        "docs/project-structure-and-module-organization.md",
        "docs/validation/containers.md",
        "docs/validation/delivery.md",
        "docs/validation/postgres.md",
        "docs/build-test-and-development-commands.md",
        "docs/ci-cd-production-ready.md",
        "docs/configuration-source-policy.md",
    }
)

_SERVICE_NAME_RE = re.compile(r"^[a-z](?:[a-z0-9]|-(?=[a-z0-9])){0,63}$")
_CODEOWNER_RE = re.compile(r"^@[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?(?:/[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)?$")
_GITHUB_OWNER_RE = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?$")
_GITHUB_REPOSITORY_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
_OBJECT_ID_RE = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
# Profile names are part of the source-owned inventory rather than a fixed
# parser constant. Keep matching limited to executable whole-line markers.
_PROFILE_MARKER_LINE_RE = re.compile(
    rb"^(?:[ \t]*(?:#|//)[ \t]*template:(?:begin|end)[ \t]+"
    rb"[a-z0-9]+(?:-[a-z0-9]+)*:[a-z0-9]+(?:-[a-z0-9]+)*[ \t]*|"
    rb"[ \t]*<!--[ \t]*template:(?:begin|end)[ \t]+"
    rb"[a-z0-9]+(?:-[a-z0-9]+)*:[a-z0-9]+(?:-[a-z0-9]+)*[ \t]*-->[ \t]*)\r?$",
    re.MULTILINE,
)
_PROFILE_MARKER_PREFIX_RE = re.compile(
    rb"^(?:[ \t]*(?:(?:#|//)[ \t]*template:(?:begin|end)(?:[ \t]|$)|<!--[ \t]*template:(?:begin|end)(?:[ \t]|$)))",
    re.MULTILINE,
)

_RESERVED_CRATE_NAMES = {
    "as", "async", "await", "break", "const", "continue", "crate", "dyn",
    "else", "enum", "extern", "false", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait",
    "true", "try", "type", "unsafe", "use", "where", "while", "yield",
    # Rust Reference, Keywords: reserved keywords remain refused even where the
    # current compiler permits a raw identifier.  `gen` is reserved in edition
    # 2024, which this template pins.
    "abstract", "become", "box", "do", "final", "override", "priv", "typeof",
    "unsized", "virtual",
}


class Refusal(ValueError):
    """An expected admission failure that must occur before real writes."""


class ToolFailure(RuntimeError):
    """A required local command could not establish an input."""


def _reject_duplicate_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise Refusal("JSON object has a duplicate key")
        result[key] = value
    return result


def _reject_constant(value: str) -> Any:
    raise Refusal(f"JSON contains unsupported constant {value}")


def parse_json_bytes(raw: bytes, label: str) -> Any:
    """Parse a UTF-8 JSON value while rejecting duplicate and non-finite input."""

    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise Refusal(f"{label} is not UTF-8") from error
    try:
        return json.loads(
            text,
            object_pairs_hook=_reject_duplicate_pairs,
            parse_constant=_reject_constant,
        )
    except (json.JSONDecodeError, Refusal) as error:
        if isinstance(error, Refusal):
            raise Refusal(f"{label}: {error}") from error
        raise Refusal(f"{label} is not valid JSON") from error


def validate_service_name(value: object) -> str:
    if not isinstance(value, str) or not _SERVICE_NAME_RE.fullmatch(value):
        raise Refusal("service_name must be a lowercase Cargo-compatible name")
    if value.replace("-", "_") in _RESERVED_CRATE_NAMES:
        raise Refusal("service_name conflicts with a Rust reserved identifier")
    return value


def validate_repository(value: object) -> str:
    if not isinstance(value, str):
        raise Refusal("repository must be an HTTPS GitHub URL")
    parsed = urlsplit(value)
    if (
        parsed.scheme != "https"
        or parsed.netloc != "github.com"
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or value.endswith("/")
    ):
        raise Refusal("repository must be an HTTPS GitHub owner/repository URL")
    pieces = parsed.path.split("/")
    if (
        len(pieces) != 3
        or pieces[0]
        or not _GITHUB_OWNER_RE.fullmatch(pieces[1])
        or not _GITHUB_REPOSITORY_RE.fullmatch(pieces[2])
        or pieces[2] in {".", ".."}
    ):
        raise Refusal("repository must have exactly owner/repository path components")
    return value


def validate_codeowner(value: object) -> str:
    if not isinstance(value, str) or not _CODEOWNER_RE.fullmatch(value):
        raise Refusal("codeowner must be one GitHub owner token")
    return value


def validate_description(value: object) -> str:
    if not isinstance(value, str) or not value or len(value) > 256:
        raise Refusal("description must be nonempty and at most 256 characters")
    if any(unicodedata.category(character) in {"Cc", "Cs", "Zl", "Zp"} for character in value):
        raise Refusal("description must be single-line text without control characters")
    if "\u2028" in value or "\u2029" in value:
        raise Refusal("description must be single-line text without separators")
    return value


def validate_identity(value: object) -> dict[str, str]:
    if not isinstance(value, dict) or set(value) != {
        "service_name", "repository", "description", "codeowner"
    }:
        raise Refusal("identity has an unsupported shape")
    return {
        "service_name": validate_service_name(value["service_name"]),
        "repository": validate_repository(value["repository"]),
        "description": validate_description(value["description"]),
        "codeowner": validate_codeowner(value["codeowner"]),
    }


def http_idempotency_requirement(database: str, authn: str) -> str | None:
    """Name the unmet HTTP_IDEMPOTENCY=postgres requirement, or none when met.

    This is the one owner of the combination rule. Both `validate_profiles`
    below and `template_init.py`'s `parse_inputs` call it and word their own
    refusals from the field it names.
    """

    if database != "postgres":
        return "database"
    if authn not in ("oidc-jwt", "oidc-introspection"):
        return "authn"
    return None


def validate_profiles(value: object) -> dict[str, str]:
    if not isinstance(value, dict) or set(value) not in (
        {"database", "agent_harness"},
        {"database", "authn", "agent_harness"},
        {"database", "authn", "outbound_http", "agent_harness"},
        {"database", "authn", "outbound_http", "http_idempotency", "agent_harness"},
    ):
        raise Refusal("profiles has an unsupported shape")
    database = value["database"]
    authn = value.get("authn", "none")
    outbound_http = value.get("outbound_http", "none")
    http_idempotency = value.get("http_idempotency", "none")
    harness = value["agent_harness"]
    if not isinstance(database, str) or database not in DATABASE_CHOICES:
        raise Refusal("profiles.database is unsupported")
    if not isinstance(authn, str) or authn not in AUTHN_CHOICES:
        raise Refusal("profiles.authn is unsupported")
    if not isinstance(outbound_http, str) or outbound_http not in OUTBOUND_HTTP_CHOICES:
        raise Refusal("profiles.outbound_http is unsupported")
    if not isinstance(http_idempotency, str) or http_idempotency not in HTTP_IDEMPOTENCY_CHOICES:
        raise Refusal("profiles.http_idempotency is unsupported")
    if http_idempotency == "postgres":
        requirement = http_idempotency_requirement(database, authn)
        if requirement == "database":
            raise Refusal("profiles.http_idempotency=postgres requires profiles.database=postgres")
        if requirement == "authn":
            raise Refusal("profiles.http_idempotency=postgres requires profiles.authn=oidc-jwt or oidc-introspection")
    if not isinstance(harness, str) or harness not in HARNESS_CHOICES:
        raise Refusal("profiles.agent_harness is unsupported")
    # Schema-1 locks issued before authentication existed select the historical
    # provider-independent profile. Normalization is read-only, so matching
    # replay preserves their bytes exactly.
    return {
        "database": database,
        "authn": authn,
        "outbound_http": outbound_http,
        "http_idempotency": http_idempotency,
        "agent_harness": harness,
    }


def validate_lock(value: object) -> dict[str, Any]:
    """Return a schema-1 lock or refuse every unsupported representation."""

    expected = {"schema_version", "state", "identity", "profiles", "source"}
    if not isinstance(value, dict) or set(value) != expected:
        raise Refusal("template.lock has an unsupported schema")
    if type(value["schema_version"]) is not int or value["schema_version"] != LOCK_SCHEMA_VERSION:
        raise Refusal("template.lock has an unsupported schema_version")
    if not isinstance(value["state"], str) or value["state"] not in {"incomplete", "complete"}:
        raise Refusal("template.lock has an unsupported state")
    source = value["source"]
    if not isinstance(source, dict) or set(source) != {"repository", "checkout_revision", "provenance"}:
        raise Refusal("template.lock source has an unsupported shape")
    if source["repository"] != TEMPLATE_REPOSITORY:
        raise Refusal("template.lock source repository is unsupported")
    revision = source["checkout_revision"]
    if not isinstance(revision, str) or not _OBJECT_ID_RE.fullmatch(revision):
        raise Refusal("template.lock checkout_revision is invalid")
    if source["provenance"] != "local-checkout":
        raise Refusal("template.lock source provenance is unsupported")
    return {
        "schema_version": LOCK_SCHEMA_VERSION,
        "state": value["state"],
        "identity": validate_identity(value["identity"]),
        "profiles": validate_profiles(value["profiles"]),
        "source": {
            "repository": TEMPLATE_REPOSITORY,
            "checkout_revision": revision,
            "provenance": "local-checkout",
        },
    }


def _regular_file(path: Path, label: str) -> None:
    try:
        mode = path.lstat().st_mode
    except FileNotFoundError:
        raise Refusal(f"{label} is missing") from None
    if stat.S_ISLNK(mode) or not stat.S_ISREG(mode):
        raise Refusal(f"{label} is not a regular file")


def load_lock(root: Path, required: bool = False) -> dict[str, Any] | None:
    """Load and validate the local one-shot lock without following a symlink."""

    path = Path(root) / LOCK_NAME
    if not path.exists() and not path.is_symlink():
        if required:
            raise Refusal("template.lock is required")
        return None
    _regular_file(path, LOCK_NAME)
    return validate_lock(parse_json_bytes(path.read_bytes(), LOCK_NAME))


def lock_has_explicit_authn(root: Path, required: bool = False) -> bool:
    """Tell a normalized lock from a historical lock without changing its bytes."""

    path = Path(root) / LOCK_NAME
    if not path.exists() and not path.is_symlink():
        if required:
            raise Refusal("template.lock is required")
        return False
    _regular_file(path, LOCK_NAME)
    raw = parse_json_bytes(path.read_bytes(), LOCK_NAME)
    validate_lock(raw)
    assert isinstance(raw, dict)
    profiles = raw["profiles"]
    assert isinstance(profiles, dict)
    return "authn" in profiles


def lock_has_explicit_outbound_http(root: Path, required: bool = False) -> bool:
    """Tell a current lock from historical lock shapes without changing bytes."""

    path = Path(root) / LOCK_NAME
    if not path.exists() and not path.is_symlink():
        if required:
            raise Refusal("template.lock is required")
        return False
    _regular_file(path, LOCK_NAME)
    raw = parse_json_bytes(path.read_bytes(), LOCK_NAME)
    validate_lock(raw)
    assert isinstance(raw, dict)
    profiles = raw["profiles"]
    assert isinstance(profiles, dict)
    return "outbound_http" in profiles


def lock_has_explicit_http_idempotency(root: Path, required: bool = False) -> bool:
    """Tell a current lock from historical lock shapes without changing bytes."""

    path = Path(root) / LOCK_NAME
    if not path.exists() and not path.is_symlink():
        if required:
            raise Refusal("template.lock is required")
        return False
    _regular_file(path, LOCK_NAME)
    raw = parse_json_bytes(path.read_bytes(), LOCK_NAME)
    validate_lock(raw)
    assert isinstance(raw, dict)
    profiles = raw["profiles"]
    assert isinstance(profiles, dict)
    return "http_idempotency" in profiles


def selected_profiles(root: Path) -> tuple[str, str]:
    """Return database and harness; the uninitialized source is postgres/all."""

    lock = load_lock(root)
    if lock is None:
        return ("postgres", "all")
    if lock["state"] != "complete":
        raise Refusal("template.lock is incomplete; inspect the init-produced diff and use a fresh template checkout")
    profiles = lock["profiles"]
    return (profiles["database"], profiles["agent_harness"])


def selected_authn(root: Path) -> str:
    """Return the normalized authentication profile without changing the lock."""

    lock = load_lock(root)
    if lock is None:
        # The source contains all optional implementations, but has not made a
        # derived-service profile selection.
        return "none"
    if lock["state"] != "complete":
        raise Refusal("template.lock is incomplete; inspect the init-produced diff and use a fresh template checkout")
    return lock["profiles"]["authn"]


def selected_outbound_http(root: Path) -> str:
    """Return the normalized outbound choice, or the source capability."""

    root = Path(root)
    lock = load_lock(root)
    if lock is None:
        return "bounded" if (root / "crates/infra-outbound-http").is_dir() else "none"
    if lock["state"] != "complete":
        raise Refusal("template.lock is incomplete; inspect the init-produced diff and use a fresh template checkout")
    return lock["profiles"]["outbound_http"]


def selected_http_idempotency(root: Path) -> str:
    """Return the normalized idempotency choice, or the source capability."""

    root = Path(root)
    lock = load_lock(root)
    if lock is None:
        return "postgres" if (root / "crates/infra-idempotency-store").is_dir() else "none"
    if lock["state"] != "complete":
        raise Refusal("template.lock is incomplete; inspect the init-produced diff and use a fresh template checkout")
    return lock["profiles"]["http_idempotency"]


def selected_adapters(harness: str) -> tuple[str, ...]:
    if harness == "core":
        return ()
    if harness == "all":
        return tuple(ADAPTERS)
    if harness not in ADAPTERS:
        raise Refusal("agent_harness is unsupported")
    return (harness,)


def safe_relative(path: str, *, allow_directory: bool = False) -> str:
    """Validate an inventory path and retain its explicit directory suffix."""

    if not isinstance(path, str) or not path:
        raise Refusal("owner path is empty")
    directory = path.endswith("/")
    if directory and not allow_directory:
        raise Refusal("owner path may not end in a slash")
    candidate = path[:-1] if directory else path
    if (
        not candidate
        or candidate.startswith("/")
        or "\\" in candidate
        or any(unicodedata.category(character) in {"Cc", "Cs"} for character in candidate)
    ):
        raise Refusal("owner path is unsafe")
    pieces = candidate.split("/")
    if any(piece in {"", ".", "..", ".git"} for piece in pieces):
        raise Refusal("owner path is unsafe")
    return path


_PROTECTED_MANIFEST_FILES = frozenset(
    {
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "README.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
        ".gitleaks.toml",
        "deny.toml",
        "template.lock",
        "make/service.mk",
        "make/profile-postgres.mk",
        "docs/repo-architecture.md",
        "docs/configuration-source-policy.md",
        "docs/project-structure-and-module-organization.md",
        "docs/build-test-and-development-commands.md",
        "docs/ci-cd-production-ready.md",
        "docs/railway-deployment-profile.md",
        "docs/production-contract.md",
        "docs/first-production-feature.md",
        "docs/authentication.md",
        "docs/validation/postgres.md",
        "docs/validation/containers.md",
        "docs/validation/delivery.md",
        "make/source.mk",
        "scripts/ci/template-init-check.sh",
        "docs/roadmap.md",
    }
)
_PROTECTED_MANIFEST_PREFIXES = (
    "api/",
    "build/",
    "crates/",
    "env/",
    "migrations/",
    "test/",
    ".github/",
    "docs/architecture/",
    "specs/",
    "evals/",
    "scripts/tests/template-",
)
_TEMPLATE_PROVENANCE_OWNERS = frozenset(
    {
        "scripts/lib/template_state.py",
        "scripts/ci/changed-surfaces.sh",
        "scripts/ci/verify.sh",
    }
)


def _protected_manifest_owner(entry: str) -> bool:
    plain = entry.rstrip("/")
    protected = (*_PROTECTED_MANIFEST_FILES, *(_prefix.rstrip("/") for _prefix in _PROTECTED_MANIFEST_PREFIXES))
    return any(plain.startswith(prefix) for prefix in _PROTECTED_MANIFEST_PREFIXES) or any(
        plain == candidate
        or plain.startswith(f"{candidate}/")
        or candidate.startswith(f"{plain}/")
        for candidate in protected
    )


def _derived_service_skill_roots(root: Path, *, initializing: bool = False) -> frozenset[Path]:
    """Admit the narrow local-skill reservation only in a complete derived tree."""

    lock = load_lock(root)
    if lock is not None and lock["state"] != "complete" and not initializing:
        raise Refusal("template.lock is incomplete; inspect the init-produced diff and use a fresh template checkout")
    skills = root / ".agents/skills"
    if not skills.exists() or skills.is_symlink() or not skills.is_dir():
        return frozenset()
    markers = [path for path in skills.rglob(".service-owned") if path.is_file() or path.is_symlink()]
    if not markers:
        # Initializer finality writes an intentional incomplete lock before its
        # first real target operation. Marker-free canonical skills need no
        # derived-tree reservation decision at that boundary.
        return frozenset()
    if lock is None:
        raise Refusal("template source contains a service-owned skill marker")
    if lock["state"] != "complete":
        raise Refusal("incomplete template.lock cannot reserve a service skill")
    roots: set[Path] = set()
    for marker in markers:
        relative = marker.relative_to(skills)
        if len(relative.parts) != 2 or relative.name != ".service-owned":
            raise Refusal("service-owned skill marker has an invalid location")
        skill = marker.parent
        if skill.is_symlink() or marker.is_symlink() or not marker.is_file() or marker.stat().st_size != 0:
            raise Refusal("service-owned skill marker has an invalid shape")
        document = skill / "SKILL.md"
        if document.is_symlink() or not document.is_file() or document.stat().st_size == 0:
            raise Refusal("service-owned skill has no valid SKILL.md")
        roots.add(skill)
    return frozenset(roots)


def _manifest_files(root: Path, entry: str) -> tuple[Path, ...]:
    owner = root / entry.rstrip("/")
    if entry.endswith("/"):
        files: list[Path] = []
        for candidate in owner.rglob("*"):
            mode = candidate.lstat().st_mode
            if stat.S_ISLNK(mode):
                raise Refusal(f"manifest owner contains a symlink: {entry}")
            if stat.S_ISREG(mode):
                files.append(candidate)
            elif not stat.S_ISDIR(mode):
                raise Refusal(f"manifest owner contains an unsupported type: {entry}")
        if not files:
            raise Refusal(f"manifest directory owner is empty: {entry}")
        return tuple(files)
    return (owner,)


def _contains_profile_marker(contents: bytes) -> bool:
    """Only whole-line host-comment markers are executable profile syntax."""

    return (
        _PROFILE_MARKER_LINE_RE.search(contents) is not None
        or _PROFILE_MARKER_PREFIX_RE.search(contents) is not None
    )


def parse_manifest(
    snapshot_root: Path, *, harness: str = "all", target_repository: str | None = None,
    initializing: bool = False,
) -> tuple[str, ...]:
    """Validate ownership; selected outputs may omit unselected adapter inputs.

    Committed template sources use the default all-adapter closure. A derived
    repository keeps that same portable manifest, with its lock selecting which
    adapter inputs must physically exist.
    """

    manifest = Path(snapshot_root) / "template-owned.paths"
    _regular_file(manifest, "template-owned.paths")
    entries: list[str] = []
    for line in manifest.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped != line:
            raise Refusal("template-owned.paths has surrounding whitespace")
        entries.append(safe_relative(stripped, allow_directory=True))
    if not entries:
        raise Refusal("template-owned.paths contains no owners")
    if len(entries) != len(set(entries)):
        raise Refusal("template-owned.paths has duplicate owners")
    plain = [entry.rstrip("/") for entry in entries]
    for index, current in enumerate(plain):
        for other_index, other in enumerate(plain):
            if index != other_index and other.startswith(f"{current}/"):
                raise Refusal("template-owned.paths has overlapping owners")
    if target_repository is not None and not isinstance(target_repository, str):
        raise Refusal("target repository identity is invalid")
    selected = set(selected_adapters(harness))
    service_skill_roots = _derived_service_skill_roots(Path(snapshot_root), initializing=initializing)
    unselected_inputs = {
        path
        for adapter, pack in ADAPTERS.items()
        if adapter not in selected
        for path in pack.canonical
    }
    for entry in entries:
        if _protected_manifest_owner(entry):
            raise Refusal(f"manifest owner is service-owned: {entry}")
        source = Path(snapshot_root) / entry.rstrip("/")
        try:
            mode = source.lstat().st_mode
        except FileNotFoundError:
            if entry in unselected_inputs:
                continue
            raise Refusal(f"manifest owner is missing: {entry}") from None
        if stat.S_ISLNK(mode):
            raise Refusal(f"manifest owner is a symlink: {entry}")
        if entry.endswith("/"):
            if not stat.S_ISDIR(mode):
                raise Refusal(f"manifest directory owner is not a directory: {entry}")
        elif not stat.S_ISREG(mode):
            raise Refusal(f"manifest file owner is not a regular file: {entry}")
        for owned_file in _manifest_files(Path(snapshot_root), entry):
            local_service_skill = any(
                owned_file == skill or skill in owned_file.parents for skill in service_skill_roots
            )
            if local_service_skill:
                continue
            if owned_file.stat().st_size == 0:
                raise Refusal(f"manifest owner is empty: {entry}")
            contents = owned_file.read_bytes()
            if _contains_profile_marker(contents):
                raise Refusal(f"manifest owner contains a profile marker: {entry}")
            if target_repository and target_repository.encode("utf-8") in contents:
                raise Refusal(f"manifest owner contains target repository identity: {entry}")
    return tuple(entries)


def git(root: Path, args: Sequence[str], *, input_bytes: bytes | None = None) -> bytes:
    """Run read-only Git plumbing without shell interpolation or index writes."""

    command = [
        "git",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-C",
        os.fspath(root),
        *args,
    ]
    environment = os.environ.copy()
    environment["GIT_OPTIONAL_LOCKS"] = "0"
    try:
        result = subprocess.run(
            command,
            input=input_bytes,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env=environment,
        )
    except OSError as error:
        raise ToolFailure("git is unavailable") from error
    if result.returncode:
        raise Refusal("git admission failed")
    return result.stdout


def git_root(root: Path) -> Path:
    supplied = Path(root)
    if supplied.is_symlink():
        raise Refusal("repository root may not be a symlink")
    resolved = supplied.resolve(strict=True)
    value = git(resolved, ["rev-parse", "--show-toplevel"]).decode("utf-8").strip()
    git_root_path = Path(value)
    if git_root_path != resolved:
        raise Refusal("repository path must name its Git root")
    return git_root_path


def git_head(root: Path) -> str:
    head = git(root, ["rev-parse", "--verify", "HEAD^{commit}"]).decode("ascii").strip()
    if not _OBJECT_ID_RE.fullmatch(head):
        raise Refusal("Git HEAD is not a supported object id")
    return head


def _parse_tree_record(record: bytes) -> tuple[str, str, str, str]:
    try:
        metadata, raw_path = record.split(b"\t", 1)
        mode, kind, object_id = metadata.decode("ascii").split(" ")
        path = raw_path.decode("utf-8")
    except (UnicodeDecodeError, ValueError) as error:
        raise Refusal("source tree contains an invalid entry") from error
    if kind != "blob" or mode not in {"100644", "100755", "120000"} or not _OBJECT_ID_RE.fullmatch(object_id):
        raise Refusal("source tree contains an unsupported entry")
    safe_relative(path)
    return mode, kind, object_id, path


def _batch_blobs(root: Path, object_ids: Sequence[str]) -> dict[str, bytes]:
    """Read the already-admitted blobs in one declared-length Git batch."""

    if not object_ids:
        return {}
    command = [
        "git",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-C",
        os.fspath(root),
        "cat-file",
        "--batch",
    ]
    environment = os.environ.copy()
    environment["GIT_OPTIONAL_LOCKS"] = "0"
    try:
        process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
        )
    except OSError as error:
        raise ToolFailure("git is unavailable") from error
    assert process.stdin is not None
    assert process.stdout is not None
    try:
        process.stdin.write("".join(f"{object_id}\n" for object_id in object_ids).encode("ascii"))
        process.stdin.close()
        blobs: dict[str, bytes] = {}
        for expected in object_ids:
            header = process.stdout.readline()
            try:
                returned, kind, raw_size = header.rstrip(b"\n").decode("ascii").split(" ")
                size = int(raw_size)
            except (UnicodeDecodeError, ValueError) as error:
                raise Refusal("git batch returned an invalid blob header") from error
            if returned != expected or kind != "blob" or size < 0:
                raise Refusal("git batch returned an unexpected object")
            contents = process.stdout.read(size)
            if len(contents) != size or process.stdout.read(1) != b"\n":
                raise Refusal("git batch returned a truncated blob")
            blobs[expected] = contents
        process.stderr.read()
        if process.wait() != 0:
            raise Refusal("git batch failed")
        return blobs
    except Exception:
        process.kill()
        process.wait()
        raise


def _generated_skill_link(path: str, contents: bytes) -> str | None:
    """Return the canonical skill name for the sole permitted source symlink."""

    for prefix in (".claude/skills/", ".qwen/skills/"):
        if path.startswith(prefix):
            name = path.removeprefix(prefix)
            expected = f"../../.agents/skills/{name}".encode("utf-8")
            if "/" not in name and contents == expected:
                return name
    return None


def snapshot_tree(root: Path, destination: Path, revision: str | None = None) -> str:
    """Materialize regular tracked blobs from one immutable commit into destination."""

    root = git_root(root)
    revision = revision or git_head(root)
    if not _OBJECT_ID_RE.fullmatch(revision):
        raise Refusal("source revision is invalid")
    if destination.exists() or destination.is_symlink():
        raise Refusal("snapshot destination already exists")
    records = git(root, ["ls-tree", "-r", "-z", "--full-tree", revision]).split(b"\0")
    parsed = [_parse_tree_record(record) for record in records if record]
    normalized = [entry[3].casefold() for entry in parsed]
    if len(normalized) != len(set(normalized)):
        raise Refusal("source tree has case-folding aliases")
    destination.mkdir(parents=True)
    try:
        blobs = _batch_blobs(root, [entry[2] for entry in parsed])
        for mode, _kind, object_id, relative in parsed:
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            if target.exists() or target.is_symlink():
                raise Refusal("source tree has file/directory collisions")
            contents = blobs[object_id]
            if mode == "120000":
                if _generated_skill_link(relative, contents) is None:
                    raise Refusal("source tree contains an unsupported symlink")
                target.symlink_to(contents.decode("utf-8"))
            else:
                target.write_bytes(contents)
                target.chmod(0o755 if mode == "100755" else 0o644)
    except Exception:
        shutil.rmtree(destination, ignore_errors=True)
        raise
    return revision


def _has_symlink_parent(root: Path, relative: str) -> bool:
    current = root
    for part in Path(relative).parts[:-1]:
        current = current / part
        if current.is_symlink():
            return True
    return False


def ensure_safe_destination(root: Path, relative: str, *, missing_ok: bool = True) -> Path:
    safe_relative(relative)
    root = Path(root)
    if root.is_symlink() or not root.is_dir():
        raise Refusal("repository root is unsafe")
    current = root
    for part in Path(relative).parts[:-1]:
        current = current / part
        if current.is_symlink():
            raise Refusal(f"destination has a symlink parent: {relative}")
        if current.exists() and not current.is_dir():
            raise Refusal(f"destination has a non-directory parent: {relative}")
    target = root / relative
    if not missing_ok and not target.exists() and not target.is_symlink():
        raise Refusal(f"destination is missing: {relative}")
    return target


@dataclass(frozen=True)
class PlannedWrite:
    relative: str
    data: bytes
    mode: int


@dataclass(frozen=True)
class PlannedLink:
    relative: str
    target: str


def atomic_write(path: Path, data: bytes, mode: int) -> None:
    """Replace one regular target using a same-parent temporary file."""

    parent = path.parent
    if parent.is_symlink() or not parent.is_dir():
        raise Refusal("write parent is unsafe")
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=parent)
    try:
        with os.fdopen(descriptor, "wb") as file:
            file.write(data)
            file.flush()
            os.fsync(file.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    except Exception:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def admit_plan(
    root: Path, writes: Iterable[PlannedWrite | PlannedLink], removals: Iterable[str] = ()
) -> tuple[list[PlannedWrite], list[PlannedLink], list[str]]:
    """Prove every operation's type and parent shape before its first write."""
    regular_writes: list[PlannedWrite] = []
    link_writes: list[PlannedLink] = []
    for write in writes:
        if isinstance(write, PlannedWrite):
            regular_writes.append(write)
        elif isinstance(write, PlannedLink):
            safe_relative(write.relative)
            if _generated_skill_link(write.relative, write.target.encode("utf-8")) is None:
                raise Refusal(f"planned link is unsupported: {write.relative}")
            link_writes.append(write)
        else:
            raise Refusal("planned write is unsupported")
    removal_paths: list[str] = []
    for relative in removals:
        safe_relative(relative)
        target = ensure_safe_destination(root, relative, missing_ok=False)
        mode = target.lstat().st_mode
        if not (stat.S_ISREG(mode) or stat.S_ISDIR(mode) or stat.S_ISLNK(mode)):
            raise Refusal(f"planned removal is unsupported: {relative}")
        removal_paths.append(relative)
    write_names = {write.relative for write in (*regular_writes, *link_writes)}
    if len(write_names) != len(regular_writes) + len(link_writes):
        raise Refusal("plan has duplicate writes")
    if len(removal_paths) != len(set(removal_paths)):
        raise Refusal("plan has duplicate removals")
    for write in regular_writes:
        target = ensure_safe_destination(root, write.relative)
        if target.is_symlink() or (target.exists() and not target.is_file()):
            raise Refusal(f"planned file collides with a non-file: {write.relative}")
    for write in link_writes:
        target = ensure_safe_destination(root, write.relative)
        if target.exists() and not target.is_symlink():
            raise Refusal(f"planned link collides with a real path: {write.relative}")
    for name in write_names:
        if name in removal_paths or any(name.startswith(f"{removal.rstrip('/')}/") for removal in removal_paths):
            raise Refusal("plan writes inside a planned removal")
    return regular_writes, link_writes, removal_paths


def verify_plan(root: Path, writes: Iterable[PlannedWrite | PlannedLink], removals: Iterable[str] = ()) -> None:
    """Read back exactly the operation effects; no broad tree comparison is implied."""

    for write in writes:
        target = root / write.relative
        if isinstance(write, PlannedWrite):
            if target.is_symlink() or not target.is_file() or target.read_bytes() != write.data:
                raise Refusal(f"planned file readback failed: {write.relative}")
            if (target.stat().st_mode & 0o777) != write.mode:
                raise Refusal(f"planned file mode readback failed: {write.relative}")
        elif isinstance(write, PlannedLink):
            if not target.is_symlink() or os.readlink(target) != write.target:
                raise Refusal(f"planned link readback failed: {write.relative}")
        else:
            raise Refusal("planned write is unsupported")
    for relative in removals:
        target = root / relative.rstrip("/")
        if target.exists() or target.is_symlink():
            raise Refusal(f"planned removal readback failed: {relative}")


def write_plan(root: Path, writes: Iterable[PlannedWrite | PlannedLink], removals: Iterable[str] = ()) -> None:
    """Apply an admitted plan in finality order and read every effect back."""

    regular_writes, link_writes, removal_paths = admit_plan(root, writes, removals)
    for write in regular_writes:
        target = ensure_safe_destination(root, write.relative)
        target.parent.mkdir(parents=True, exist_ok=True)
        atomic_write(target, write.data, write.mode)
    files: list[Path] = []
    directories: list[Path] = []
    for relative in removal_paths:
        target = ensure_safe_destination(root, relative, missing_ok=False)
        mode = target.lstat().st_mode
        if stat.S_ISDIR(mode):
            directories.append(target)
        elif stat.S_ISREG(mode):
            files.append(target)
        elif stat.S_ISLNK(mode) and _generated_skill_link(relative, os.readlink(target).encode("utf-8")) is not None:
            files.append(target)
        else:
            raise Refusal(f"planned removal is unsupported: {relative}")
    for target in files:
        target.unlink()
    for target in sorted(directories, key=lambda item: len(item.parts), reverse=True):
        target.rmdir()
    for write in link_writes:
        target = ensure_safe_destination(root, write.relative)
        target.parent.mkdir(parents=True, exist_ok=True)
        if target.exists() or target.is_symlink():
            target.unlink()
        target.symlink_to(write.target)
    verify_plan(root, (*regular_writes, *link_writes), removal_paths)


def _profile_command(arguments: argparse.Namespace) -> int:
    try:
        root = Path(arguments.repo)
        if root.is_symlink() or not root.is_dir():
            raise Refusal("repository directory is unsafe")
        root = root.resolve(strict=True)
        database, harness = selected_profiles(root)
        value = {
            "database": database,
            "authn": selected_authn(root),
            "outbound_http": selected_outbound_http(root),
            "http_idempotency": selected_http_idempotency(root),
            "agent_harness": harness,
        }[arguments.field]
        print(value)
        return 0
    except (Refusal, ToolFailure) as error:
        print(f"template state: {error}", file=sys.stderr)
        return 2


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="template lifecycle state helper")
    commands = parser.add_subparsers(dest="command", required=True)
    profile = commands.add_parser("profile", help="print selected profile data")
    profile.add_argument("--repo", required=True, type=Path)
    profile.add_argument(
        "--field", required=True, choices=("database", "authn", "outbound_http", "http_idempotency", "agent_harness")
    )
    profile.set_defaults(handler=_profile_command)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = build_parser().parse_args(argv)
    return arguments.handler(arguments)


if __name__ == "__main__":
    raise SystemExit(main())
