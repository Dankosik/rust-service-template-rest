#!/usr/bin/env python3

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
from dataclasses import dataclass, fields, replace
from pathlib import Path
from typing import Any, Sequence

sys.dont_write_bytecode = True

_SCRIPT_DIR = Path(__file__).resolve().parent
_SCRIPT_REPO_ROOT = _SCRIPT_DIR.parents[1]
if os.fspath(_SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, os.fspath(_SCRIPT_DIR))

from state import (  # noqa: E402
    ADAPTERS,
    AUTHN_CHOICES,
    CACHE_CHOICES,
    DATABASE_CHOICES,
    GRPC_CHOICES,
    HARNESS_CHOICES,
    HTTP_IDEMPOTENCY_CHOICES,
    INBOUND_WEBHOOKS_CHOICES,
    JOBS_CHOICES,
    LOCK_NAME,
    LOCK_SCHEMA_VERSION,
    MESSAGING_CHOICES,
    OBJECT_STORAGE_CHOICES,
    OUTBOUND_AUTH_CHOICES,
    OUTBOUND_HTTP_CHOICES,
    OUTBOX_CHOICES,
    SERVICE_AUTH_CHOICES,
    TEMPLATE_REPOSITORY,
    WEBHOOKS_CHOICES,
    PlannedLink,
    PlannedWrite,
    Refusal,
    ToolFailure,
    _contains_profile_marker,
    _manifest_files,
    admit_plan,
    atomic_write,
    git,
    git_head,
    git_root,
    http_idempotency_requirement,
    inbound_webhooks_requirement,
    is_bytecode_path,
    jobs_requirement,
    load_lock,
    manifest_entries,
    outbox_requirement,
    parse_json_bytes,
    parse_manifest,
    safe_relative,
    selected_adapters,
    service_auth_requirement,
    snapshot_tree,
    validate_codeowner,
    validate_description,
    validate_repository,
    validate_service_name,
    webhooks_requirement,
    write_plan,
)


_PROFILE_FILE = "scripts/template/profiles.json"
_SELF_REMOVED_FILES = ("scripts/template/init.py", "scripts/template/profiles.json")
_MARKER_PREFIX_RE = re.compile(
    r"^[ \t]*(?:(?:#|//)[ \t]*template:(?:begin|end)(?:[ \t]|$)|<!--[ \t]*template:(?:begin|end)(?:[ \t]|$))"
)

# Recognizes only complete host-comment marker lines so ordinary prose is inert.
_MARKER_RE = re.compile(
    r"^(?P<indent>[ \t]*)(?:(?P<hash>#)|(?P<slash>//)|(?P<html><!--))"
    r"[ \t]*template:(?P<kind>begin|end)[ \t]+(?P<profile>[a-z0-9]+(?:-[a-z0-9]+)*):(?P<id>[a-z0-9]+(?:-[a-z0-9]+)*)"
    r"(?:(?(html)[ \t]*-->|))[ \t]*$"
)


@dataclass(frozen=True)
class InitInputs:
    service_name: str
    repository: str
    description: str
    codeowner: str
    database: str
    authn: str
    outbound_http: str
    outbound_auth: str
    grpc: str
    http_idempotency: str
    jobs: str
    messaging: str
    outbox: str
    webhooks: str
    inbound_webhooks: str
    cache: str
    object_storage: str
    service_auth: str
    agent_harness: str

    def identity(self) -> dict[str, str]:
        return {
            "service_name": self.service_name,
            "repository": self.repository,
            "description": self.description,
            "codeowner": self.codeowner,
        }

    def profiles(self) -> dict[str, str]:
        return {
            "database": self.database,
            "authn": self.authn,
            "outbound_http": self.outbound_http,
            "outbound_auth": self.outbound_auth,
            "grpc": self.grpc,
            "http_idempotency": self.http_idempotency,
            "jobs": self.jobs,
            "messaging": self.messaging,
            "outbox": self.outbox,
            "webhooks": self.webhooks,
            "inbound_webhooks": self.inbound_webhooks,
            "cache": self.cache,
            "object_storage": self.object_storage,
            "service_auth": self.service_auth,
            "agent_harness": self.agent_harness,
        }


@dataclass(frozen=True)
class ProfileData:
    source_only: tuple[str, ...]
    removals: dict[str, tuple[str, ...]]
    identity: tuple[dict[str, Any], ...]


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
        authn=_argument_value(arguments, "authn", default="none"),
        outbound_http=_argument_value(arguments, "outbound_http", default="none"),
        outbound_auth=_argument_value(arguments, "outbound_auth", default="none"),
        grpc=_argument_value(arguments, "grpc", default="none"),
        http_idempotency=_argument_value(arguments, "http_idempotency", default="none"),
        jobs=_argument_value(arguments, "jobs", default="none"),
        messaging=_argument_value(arguments, "messaging", default="none"),
        outbox=_argument_value(arguments, "outbox", default="none"),
        webhooks=_argument_value(arguments, "webhooks", default="none"),
        inbound_webhooks=_argument_value(arguments, "inbound_webhooks", default="none"),
        cache=_argument_value(arguments, "cache", default="none"),
        object_storage=_argument_value(arguments, "object_storage", default="none"),
        service_auth=_argument_value(arguments, "service_auth", default="none"),
        agent_harness=_argument_value(arguments, "agent_harness", default="all"),
    )
    if inputs.database not in DATABASE_CHOICES:
        raise Refusal("DATABASE is unsupported")
    if inputs.authn not in AUTHN_CHOICES:
        raise Refusal("AUTHN is unsupported")
    if inputs.outbound_http not in OUTBOUND_HTTP_CHOICES:
        raise Refusal("OUTBOUND_HTTP is unsupported")
    if inputs.outbound_auth not in OUTBOUND_AUTH_CHOICES:
        raise Refusal("OUTBOUND_AUTH is unsupported")
    if inputs.grpc not in GRPC_CHOICES:
        raise Refusal("GRPC is unsupported")
    if inputs.outbound_auth == "oauth2-client-credentials":
        inputs = replace(inputs, outbound_http="bounded")
    if inputs.http_idempotency not in HTTP_IDEMPOTENCY_CHOICES:
        raise Refusal("HTTP_IDEMPOTENCY is unsupported")
    if inputs.agent_harness not in HARNESS_CHOICES:
        raise Refusal("AGENT_HARNESS is unsupported")
    if inputs.jobs not in JOBS_CHOICES:
        raise Refusal("JOBS is unsupported")
    if inputs.messaging not in MESSAGING_CHOICES:
        raise Refusal("MESSAGING is unsupported")
    if inputs.cache not in CACHE_CHOICES:
        raise Refusal("CACHE is unsupported")
    if inputs.object_storage not in OBJECT_STORAGE_CHOICES:
        raise Refusal("OBJECT_STORAGE is unsupported")
    if inputs.service_auth not in SERVICE_AUTH_CHOICES:
        raise Refusal("SERVICE_AUTH is unsupported")
    if inputs.outbox not in OUTBOX_CHOICES:
        raise Refusal("OUTBOX is unsupported")
    if inputs.webhooks not in WEBHOOKS_CHOICES:
        raise Refusal("WEBHOOKS is unsupported")
    if inputs.inbound_webhooks not in INBOUND_WEBHOOKS_CHOICES:
        raise Refusal("INBOUND_WEBHOOKS is unsupported")
    if inputs.http_idempotency == "postgres":
        requirement = http_idempotency_requirement(inputs.database, inputs.authn)
        if requirement == "database":
            raise Refusal("HTTP_IDEMPOTENCY=postgres requires DATABASE=postgres")
        if requirement == "authn":
            raise Refusal("HTTP_IDEMPOTENCY=postgres requires AUTHN=oidc-jwt or oidc-introspection")
    if inputs.jobs == "postgres":
        requirement = jobs_requirement(inputs.database)
        if requirement is not None:
            raise Refusal(requirement)
    if inputs.outbox == "postgres":
        requirement = outbox_requirement(inputs.database, inputs.jobs, inputs.messaging)
        if requirement is not None:
            raise Refusal(requirement)
    if inputs.webhooks == "durable":
        requirement = webhooks_requirement(inputs.database, inputs.jobs, inputs.outbound_http)
        if requirement is not None:
            raise Refusal(requirement)
    if inputs.inbound_webhooks == "standard-webhooks":
        requirement = inbound_webhooks_requirement(inputs.database, inputs.jobs)
        if requirement is not None:
            raise Refusal(requirement)
    if inputs.service_auth == "jwt":
        requirement = service_auth_requirement(inputs.grpc)
        if requirement is not None:
            raise Refusal(requirement)
    return inputs


# The first refusal must remain independent of JSON key order.
_PROFILE_SECTION_ORDER = (
    "source-template",
    "authn",
    "oidc-jwt",
    "oidc-introspection",
    "outbound-http",
    "request-budget",
    "outbound-auth",
    "grpc",
    "grpc-authn",
    "grpc-jwt",
    "outbound-auth-grpc",
    "runtime-progress",
    "test-digest",
    "service-auth",
    "test-metrics",
    "test-metrics-util",
    "jobs-reference",
    "config-url",
    "tls-fixtures",
    "http-idempotency",
    "http-idempotency-mounted",
    "postgres-grpc-consumers",
    "postgres-grpc-auth",
    "test-introspection-fixtures",
    "jobs",
    "jobs-http-idempotency",
    "webhooks-common",
    "webhooks",
    "inbound-webhooks",
    "messaging",
    "worker",
    "service-secrets",
    "service-test-tempfile",
    "integration",
    "container-integration",
    "outbox",
    "cache",
    "object-storage",
    "rustls",
    "jsonwebtoken",
)
_VALID_MARKER_PROFILES = frozenset({"postgres", *_PROFILE_SECTION_ORDER})
_PROFILE_INVENTORY_KEYS = frozenset(
    {"schema_version", "source_only", "postgres", *_PROFILE_SECTION_ORDER, "identity"}
)


def _profile_data(snapshot: Path) -> ProfileData:
    profile_path = snapshot / _PROFILE_FILE
    if profile_path.is_symlink():
        raise Refusal("template profile inventory is unsafe")
    try:
        raw = parse_json_bytes(profile_path.read_bytes(), _PROFILE_FILE)
    except FileNotFoundError:
        raise Refusal("template profile inventory is missing") from None
    if not isinstance(raw, dict) or raw.get("schema_version") != 1 or frozenset(raw) != _PROFILE_INVENTORY_KEYS:
        raise Refusal("template profile inventory has an unsupported schema")
    source_only = _path_list(raw["source_only"], "source_only")
    postgres = raw["postgres"]
    if not isinstance(postgres, dict) or set(postgres) != {"remove_when_none"}:
        raise Refusal("template PostgreSQL inventory has an unsupported shape")
    removals = {"postgres": tuple(_path_list(postgres["remove_when_none"], "remove_when_none"))}
    for profile in _PROFILE_SECTION_ORDER:
        section = raw[profile]
        if not isinstance(section, dict) or set(section) != {"remove_when_unselected"}:
            raise Refusal(f"template {profile} inventory has an unsupported shape")
        removals[profile] = tuple(_path_list(section["remove_when_unselected"], f"{profile} remove_when_unselected"))
    identity = raw["identity"]
    if not isinstance(identity, list):
        raise Refusal("template identity inventory has an unsupported shape")
    _validate_identity_inventory(identity)
    return ProfileData(tuple(source_only), removals, tuple(identity))


def _path_list(value: object, label: str) -> list[str]:
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise Refusal(f"template {label} inventory is invalid")
    paths = [safe_relative(item, allow_directory=True) for item in value]
    if len(paths) != len(set(paths)):
        raise Refusal(f"template {label} inventory has duplicate paths")
    return paths


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


def _marker(line: str) -> tuple[str, str, str] | None:
    matched = _MARKER_RE.fullmatch(line)
    if matched is None:
        return None
    return (matched.group("kind"), matched.group("profile"), matched.group("id"))


def _selected_marker_profiles(inputs: InitInputs) -> set[str]:
    selected = {"postgres"} if inputs.database == "postgres" else set()
    if inputs.authn != "none":
        selected.update(("authn", inputs.authn))
    if inputs.outbound_http == "bounded":
        selected.add("outbound-http")
    if inputs.outbound_auth == "oauth2-client-credentials":
        selected.add("outbound-auth")
    if inputs.grpc == "enabled":
        selected.add("grpc")
        if inputs.authn != "none":
            selected.add("grpc-authn")
        if inputs.database == "postgres" and inputs.authn in {"none", "oidc-introspection"}:
            selected.add("postgres-grpc-consumers")
        if inputs.database == "postgres" and inputs.authn == "oidc-introspection":
            selected.add("postgres-grpc-auth")
        if inputs.authn == "oidc-jwt":
            selected.add("grpc-jwt")
        if inputs.outbound_auth == "oauth2-client-credentials":
            selected.add("outbound-auth-grpc")
    if (
        inputs.grpc == "enabled"
        and inputs.authn == "oidc-jwt"
        and inputs.messaging == "nats-jetstream"
        and inputs.object_storage == "s3"
    ):
        selected.add("runtime-progress")
    if inputs.messaging == "nats-jetstream":
        selected.add("config-url")
    if inputs.service_auth == "jwt":
        selected.add("service-auth")
    if (
        inputs.authn == "oidc-jwt"
        or inputs.outbound_auth == "oauth2-client-credentials"
        or inputs.service_auth == "jwt"
    ):
        selected.add("jsonwebtoken")
    if (
        inputs.authn != "none"
        or inputs.outbound_http == "bounded"
        or inputs.grpc == "enabled"
        or inputs.cache == "redis"
        or inputs.database == "postgres"
    ):
        selected.add("tls-fixtures")
    if inputs.outbound_http == "bounded":
        selected.add("request-budget")
    if inputs.http_idempotency == "postgres":
        selected.update(("http-idempotency", "request-budget"))
        if inputs.authn == "oidc-introspection":
            selected.add("http-idempotency-mounted")
    if inputs.authn == "oidc-introspection" and (
        inputs.http_idempotency == "postgres"
        or (inputs.database == "postgres" and inputs.grpc == "enabled")
    ):
        selected.add("test-introspection-fixtures")
    if inputs.http_idempotency == "postgres" or inputs.inbound_webhooks == "standard-webhooks":
        selected.update(("test-metrics", "test-metrics-util"))
    if inputs.jobs == "postgres":
        selected.update(("jobs", "test-metrics"))
        if inputs.http_idempotency == "postgres":
            selected.add("jobs-http-idempotency")
    if inputs.messaging == "nats-jetstream":
        selected.add("messaging")
    if inputs.outbox == "postgres":
        selected.add("outbox")
        if inputs.jobs == "postgres" and inputs.webhooks == "durable":
            selected.add("jobs-reference")
    if "outbox" in selected:
        selected.add("test-digest")
    if inputs.jobs == "postgres" or inputs.messaging == "nats-jetstream":
        selected.add("worker")
    if inputs.cache == "redis":
        selected.add("cache")
    if inputs.object_storage == "s3":
        selected.add("object-storage")
    if (
        inputs.grpc == "enabled"
        or inputs.cache == "redis"
        or inputs.outbound_http == "bounded"
        or inputs.object_storage == "s3"
    ):
        selected.add("rustls")
    # The service crate itself exposes only the PostgreSQL DSN and the inbound
    # webhook signing keys, which require PostgreSQL; other providers read
    # their secrets in their own adapters.
    if inputs.database == "postgres":
        selected.add("service-secrets")
    if (
        inputs.database == "postgres"
        or inputs.messaging == "nats-jetstream"
        or inputs.cache == "redis"
        or inputs.object_storage == "s3"
    ):
        selected.add("integration")
    if (
        inputs.database == "postgres"
        or inputs.messaging == "nats-jetstream"
        or inputs.cache == "redis"
        or inputs.object_storage == "s3"
        or inputs.outbound_auth == "oauth2-client-credentials"
    ):
        selected.add("container-integration")
    if inputs.webhooks == "durable" or inputs.inbound_webhooks == "standard-webhooks":
        selected.add("webhooks-common")
    if inputs.webhooks == "durable":
        selected.add("webhooks")
    if inputs.inbound_webhooks == "standard-webhooks":
        selected.update(("inbound-webhooks", "request-budget"))
    if selected & {"postgres", "cache", "grpc-jwt"}:
        selected.add("service-test-tempfile")
    return selected


def _marker_files(root: Path) -> tuple[tuple[str, Path], ...]:
    files: list[tuple[str, Path]] = []
    for path in root.rglob("*"):
        relative = path.relative_to(root)
        if (
            any(part in {".git", "target"} for part in relative.parts)
            or is_bytecode_path(relative)
            or path.is_symlink()
            or not path.is_file()
        ):
            continue
        files.append((relative.as_posix(), path))
    return tuple(sorted(files))


def _apply_markers(
    snapshot: Path,
    _profiles: ProfileData,
    inputs: InitInputs,
    *,
    files: Sequence[tuple[str, Path]] | None = None,
) -> None:
    selected = _selected_marker_profiles(inputs)
    seen: set[tuple[str, str, str]] = set()
    for relative, path in _marker_files(snapshot) if files is None else files:
        raw = path.read_bytes()
        if not _contains_profile_marker(raw):
            continue
        try:
            lines = raw.decode("utf-8").splitlines(keepends=True)
        except UnicodeDecodeError as error:
            raise Refusal(f"profile marker surface is not UTF-8: {relative}") from error
        stack: list[tuple[str, str]] = []
        transformed: list[str] = []
        found = False
        for line in lines:
            parsed = _marker(line.rstrip("\r\n"))
            if parsed is None:
                if _MARKER_PREFIX_RE.match(line):
                    raise Refusal(f"malformed profile marker in {relative}")
                if stack and stack[-1][0] not in selected:
                    continue
                transformed.append(line)
                continue
            found = True
            kind, profile, marker_id = parsed
            if profile not in _VALID_MARKER_PROFILES:
                raise Refusal(f"unknown profile marker in {relative}")
            key = (profile, relative, marker_id)
            if kind == "begin":
                if stack:
                    raise Refusal(f"nested profile marker in {relative}")
                if key in seen:
                    raise Refusal(f"duplicate profile marker in {relative}")
                stack.append((profile, marker_id))
                seen.add(key)
            else:
                if stack != [(profile, marker_id)]:
                    raise Refusal(f"mismatched profile marker in {relative}")
                stack.pop()
        if stack:
            raise Refusal(f"unclosed profile marker in {relative}")
        if found:
            path.write_text("".join(transformed), encoding="utf-8")


def project_portable(snapshot: Path, target_lock: dict[str, Any]) -> None:
    files = tuple(
        (path.relative_to(snapshot).as_posix(), path)
        for entry in manifest_entries(snapshot)
        for path in _manifest_files(snapshot, entry)
    )
    profiles = _profile_data(snapshot)
    inputs = InitInputs(**target_lock["identity"], **target_lock["profiles"])
    _apply_markers(snapshot, profiles, inputs, files=files)


def _remove_paths(snapshot: Path, paths: Sequence[str]) -> None:
    independent: list[str] = []
    for relative in sorted(set(paths), key=lambda item: (len(Path(item.rstrip("/")).parts), item)):
        plain = relative.rstrip("/")
        if any(plain.startswith(f"{ancestor.rstrip('/')}/") for ancestor in independent):
            continue
        independent.append(relative)
    for relative in independent:
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


def _prune_empty_dirs(directory: Path) -> None:
    if not directory.is_dir() or directory.is_symlink():
        return
    for child in sorted(directory.iterdir()):
        if child.is_dir() and not child.is_symlink():
            _prune_empty_dirs(child)
    if not any(directory.iterdir()):
        directory.rmdir()


def _remove_unselected_adapters(snapshot: Path, harness: str) -> None:
    selected = set(selected_adapters(harness))
    for adapter, pack in ADAPTERS.items():
        if adapter in selected:
            continue
        _remove_paths(snapshot, pack.canonical)
        for relative in (*pack.generated, *pack.settings):
            path = snapshot / relative.rstrip("/")
            if path.is_symlink() or path.is_file():
                path.unlink()
            elif path.is_dir():
                shutil.rmtree(path)
    _prune_empty_dirs(snapshot / ".agents/adapters")


def _tracked_checkout_is_clean(root: Path) -> None:
    if git(root, ["status", "--porcelain=v1", "-z", "--untracked-files=no"]):
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


def _postconditions(
    root: Path,
    inputs: InitInputs,
    profiles: ProfileData,
    *,
    initial: bool = False,
    replay_lock: dict[str, Any] | None = None,
) -> None:
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
        for relative in (*profiles.source_only, *_SELF_REMOVED_FILES):
            candidate = root / relative.rstrip("/")
            if candidate.exists() or candidate.is_symlink():
                raise Refusal(f"source-only output remains: {relative}")
    _validate_profile_packs(root, profiles, _selected_marker_profiles(inputs))
    selected = set(selected_adapters(inputs.agent_harness))
    for adapter, pack in ADAPTERS.items():
        paths = (*pack.canonical, *pack.generated, *pack.settings)
        if adapter in selected:
            if any(not (root / item.rstrip("/")).exists() and not (root / item.rstrip("/")).is_symlink() for item in paths):
                raise Refusal(f"selected adapter output is incomplete: {adapter}")
        elif any((root / item.rstrip("/")).exists() or (root / item.rstrip("/")).is_symlink() for item in paths):
            raise Refusal(f"unselected adapter output remains: {adapter}")
    parse_manifest(
        root,
        harness=inputs.agent_harness,
        target_repository=inputs.repository,
        initializing=initial,
        _replay_lock=replay_lock,
    )
    _validate_generated_ownership(root, inputs.agent_harness)
    for _relative, path in _marker_files(root):
        if _contains_profile_marker(path.read_bytes()):
            raise Refusal("initialized output retains an executable profile marker")


def _validate_profile_packs(root: Path, profiles: ProfileData, selected: set[str]) -> None:
    for profile, paths in profiles.removals.items():
        for relative in paths:
            candidate = root / relative.rstrip("/")
            exists = candidate.exists() or candidate.is_symlink()
            if profile not in selected:
                if exists:
                    raise Refusal(f"unselected {profile} output remains: {relative}")
                continue
            if not exists or candidate.is_symlink():
                raise Refusal(f"selected {profile} output is missing: {relative}")
            mode = candidate.lstat().st_mode
            if relative.endswith("/"):
                if not stat.S_ISDIR(mode):
                    raise Refusal(f"selected {profile} directory has an invalid type: {relative}")
            elif not stat.S_ISREG(mode):
                raise Refusal(f"selected {profile} file has an invalid type: {relative}")


def _validate_generated_ownership(root: Path, harness: str) -> None:
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


def _replay(root: Path, inputs: InitInputs, lock: dict[str, Any]) -> int:
    if lock["state"] != "complete":
        raise Refusal("template.lock is incomplete; inspect the init-produced diff and use a fresh template checkout")
    if lock["identity"] != inputs.identity() or lock["profiles"] != inputs.profiles():
        raise Refusal("template initialization choices differ from the complete template.lock")
    inventory_root = (
        root
        if (root / _PROFILE_FILE).exists() or (root / _PROFILE_FILE).is_symlink()
        else _SCRIPT_REPO_ROOT
    )
    profiles = _profile_data(inventory_root)
    _postconditions(root, inputs, profiles, replay_lock=lock)
    print("template init: matching complete lock; no changes")
    return 0


def _run_staged_command(snapshot: Path, command: Sequence[str], operation: str) -> bytes:
    try:
        result = subprocess.run(
            command,
            cwd=snapshot,
            capture_output=True,
            check=False,
            env=_staged_environment(snapshot),
        )
    except OSError as error:
        raise ToolFailure(f"staged {operation} tool is unavailable") from error
    if result.returncode:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        diagnostic = f"\n{detail[:8192]}" if detail else ""
        raise Refusal(f"staged {operation} failed (exit {result.returncode}){diagnostic}")
    return result.stdout


def _preflight_staged(snapshot: Path, inputs: InitInputs, profiles: ProfileData) -> None:
    _project_staged(snapshot, inputs, profiles)
    _validate_staged_runtime(snapshot, inputs)


def _project_staged(snapshot: Path, inputs: InitInputs, profiles: ProfileData) -> None:
    _check_harness_projection(snapshot, "all")
    _apply_markers(snapshot, profiles, inputs)
    _apply_identity(snapshot, profiles, inputs)
    _remove_paths(snapshot, (*profiles.source_only, *_SELF_REMOVED_FILES))
    unselected_removals = [
        relative
        for profile, removals in profiles.removals.items()
        if profile not in _selected_marker_profiles(inputs)
        for relative in removals
    ]
    _remove_paths(snapshot, unselected_removals)
    _remove_unselected_adapters(snapshot, inputs.agent_harness)
    _generate_harness_projection(snapshot, inputs.agent_harness)
    _check_harness_projection(snapshot, inputs.agent_harness, require_materialized=True)
    _project_workspace_dependencies(snapshot)


def _lock_identities(lock: Path) -> set[tuple[str, str, str, str]]:
    try:
        packages = tomllib.loads(lock.read_text(encoding="utf-8"))["package"]
        return {
            (package["name"], package["version"], package.get("source", ""), package.get("checksum", ""))
            for package in packages
        }
    except (KeyError, TypeError, UnicodeDecodeError, OSError, tomllib.TOMLDecodeError) as error:
        raise Refusal("Cargo.lock is not a readable package list") from error


def _prune_cargo_lock(snapshot: Path, inputs: InitInputs) -> bytes:
    """Let Cargo drop the packages the projected manifests no longer reach.

    Cargo keeps every locked version that still satisfies a manifest, so this
    only removes. A manifest the template lock does not satisfy would make it
    select another version instead; that selection is refused, so a derived
    service never locks a version, source or checksum the template did not.
    Returns the metadata of the pruned graph.
    """

    lock = snapshot / "Cargo.lock"
    pinned = _lock_identities(lock)
    metadata_bytes = _run_staged_command(
        snapshot,
        ["cargo", "metadata", "--offline", "--format-version", "1"],
        "offline Cargo.lock pruning",
    )
    for name, _version, source, checksum in _lock_identities(lock) - pinned:
        # The renamed service package is the one local identity the lock gains.
        if name != inputs.service_name or source or checksum:
            raise Refusal(f"Cargo resolved `{name}` to a package the template Cargo.lock does not pin")
    return metadata_bytes


def _validate_staged_runtime(snapshot: Path, inputs: InitInputs) -> None:
    metadata_bytes = _prune_cargo_lock(snapshot, inputs)
    _format_staged_rust(snapshot, metadata_bytes)
    try:
        generated = subprocess.run(
            ["cargo", "run", "-q", "-p", inputs.service_name, "--bin", "openapi", "--locked", "--offline"],
            cwd=snapshot,
            capture_output=True,
            check=False,
            env=_staged_environment(snapshot),
        )
    except OSError as error:
        raise ToolFailure("staged OpenAPI generator is unavailable") from error
    if generated.returncode:
        diagnostic = generated.stderr.decode("utf-8", errors="replace").strip()
        failure = f"staged OpenAPI generation failed (exit {generated.returncode})"
        raise Refusal(f"{failure}\n{diagnostic}" if diagnostic else failure)
    (snapshot / "api/openapi/service.yaml").write_bytes(generated.stdout)


def _format_staged_rust(snapshot: Path, metadata_bytes: bytes) -> None:
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
    # An explicit absolute caller cache is reused, so repeated initializations
    # compile the locked dependency graph once; otherwise the build stays
    # private to this attempt and outside the staged tree.
    if not os.path.isabs(environment.get("CARGO_TARGET_DIR", "")):
        environment["CARGO_TARGET_DIR"] = os.fspath(snapshot.parent / "cargo-target")
    return environment


def _run_harness(snapshot: Path, harness: str, mode: str, *, require_materialized: bool = False) -> None:
    command = [
        sys.executable,
        "-B",
        "scripts/harness/sync.py",
        f"--{mode}",
        "--repo",
        os.fspath(snapshot),
        "--harness",
        harness,
    ]
    if require_materialized and mode == "check":
        command.append("--require-materialized")
    _run_staged_command(snapshot, command, f"{mode} harness projection")


def _check_harness_projection(snapshot: Path, harness: str, *, require_materialized: bool = False) -> None:
    _run_harness(snapshot, harness, "check", require_materialized=require_materialized)


def _generate_harness_projection(snapshot: Path, harness: str) -> None:
    _run_harness(snapshot, harness, "apply")


def _dependency_sections(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    sections: list[Any] = []
    for section_name in ("dependencies", "dev-dependencies", "build-dependencies"):
        if section_name in manifest:
            sections.append(manifest[section_name])
    target = manifest.get("target", {})
    if not isinstance(target, dict):
        raise Refusal("Cargo manifest target dependency section is invalid")
    for conditions in target.values():
        if not isinstance(conditions, dict):
            raise Refusal("Cargo manifest target dependency condition is invalid")
        for section_name in ("dependencies", "dev-dependencies", "build-dependencies"):
            if section_name in conditions:
                sections.append(conditions[section_name])
    for section in sections:
        if not isinstance(section, dict):
            raise Refusal("Cargo manifest dependency section is invalid")
    return sections


def _member_manifests(snapshot: Path) -> list[Path]:
    manifests = [*sorted(snapshot.glob("crates/*/Cargo.toml")), snapshot / "test/Cargo.toml"]
    return [path for path in manifests if path.is_file() and not path.is_symlink()]


_WORKSPACE_DEPENDENCY_RE = re.compile(r"^([A-Za-z0-9_-]+)[ \t]*=")


def _project_workspace_dependencies(snapshot: Path) -> None:
    """Drop the workspace dependency declarations no retained member inherits.

    A dependency whose consumers sit in several profiles has no single profile
    marker. Once the selection removes every consumer, its declaration is
    unused, and the derived service's `make unused-deps` rejects it.
    """

    path = snapshot / "Cargo.toml"
    contents = path.read_text(encoding="utf-8")
    try:
        declared = set(tomllib.loads(contents)["workspace"]["dependencies"])
        inherited: set[str] = set()
        for manifest_path in _member_manifests(snapshot):
            for section in _dependency_sections(tomllib.loads(manifest_path.read_text(encoding="utf-8"))):
                inherited.update(
                    alias
                    for alias, definition in section.items()
                    if isinstance(definition, dict) and definition.get("workspace") is True
                )
    except (KeyError, TypeError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise Refusal("workspace dependency inputs are invalid") from error
    unused = declared - inherited
    if not unused:
        return
    kept: list[str] = []
    removed: set[str] = set()
    table = ""
    for line in contents.splitlines(keepends=True):
        if line.startswith("["):
            table = line.strip()
        matched = _WORKSPACE_DEPENDENCY_RE.match(line)
        if table == "[workspace.dependencies]" and matched is not None and matched.group(1) in unused:
            while kept and kept[-1].lstrip().startswith("#"):
                kept.pop()
            removed.add(matched.group(1))
            continue
        kept.append(line)
    projected = "".join(kept)
    try:
        remaining = set(tomllib.loads(projected)["workspace"]["dependencies"])
    except (KeyError, TypeError, tomllib.TOMLDecodeError) as error:
        raise Refusal("workspace dependency projection is invalid") from error
    if removed != unused or remaining != declared - unused:
        raise Refusal("unused workspace dependency declarations are not one line each")
    path.write_text(projected, encoding="utf-8")


def _is_admitted_adapter_output(relative: str, harness: str) -> bool:
    for adapter in selected_adapters(harness):
        pack = ADAPTERS[adapter]
        for owner in (*pack.generated, *pack.settings):
            if owner.endswith("/"):
                if relative.startswith(owner):
                    return True
            elif relative == owner:
                return True
    return False


def _collect_plan(
    staged: Path, root: Path, owned_removals: Sequence[str], harness: str = "all"
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
            if relative not in tracked and not _is_admitted_adapter_output(relative, harness):
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
        if relative not in tracked and not _is_admitted_adapter_output(relative, harness):
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
        for parent in Path(relative).parents[:-1]:
            candidate = parent.as_posix()
            if not any(path.startswith(f"{candidate}/") for path in staged_paths):
                candidate_directories.add(candidate)
    removal_set = set(removals)
    for relative in sorted(candidate_directories, key=lambda item: (len(Path(item).parts), item), reverse=True):
        directory = root / relative
        if not directory.exists() and not directory.is_symlink():
            continue
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
        return _replay(root, inputs, existing)
    _tracked_checkout_is_clean(root)
    revision = git_head(root)
    with tempfile.TemporaryDirectory(prefix="template-init-") as temporary:
        staged = Path(temporary) / "snapshot"
        snapshot_tree(root, staged, revision)
        profiles = _profile_data(staged)
        _preflight_staged(staged, inputs, profiles)
        _postconditions(staged, inputs, profiles, initial=True)
        owned_removals = [*profiles.source_only, *_SELF_REMOVED_FILES]
        for profile, removals in profiles.removals.items():
            if profile not in _selected_marker_profiles(inputs):
                owned_removals.extend(removals)
        for adapter, pack in ADAPTERS.items():
            if adapter not in selected_adapters(inputs.agent_harness):
                owned_removals.extend((*pack.canonical, *pack.generated, *pack.settings))
        writes, removals = _collect_plan(staged, root, owned_removals, inputs.agent_harness)
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
        def __call__(
            self,
            parser: argparse.ArgumentParser,
            namespace: argparse.Namespace,
            values: object,
            option_string: str | None = None,
        ) -> None:
            if getattr(namespace, self.dest, None) is not None:
                raise argparse.ArgumentError(self, f"{option_string} may be supplied once")
            setattr(namespace, self.dest, values)

    parser = argparse.ArgumentParser(description="initialize one derived service from this template")
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    for field in fields(InitInputs):
        parser.add_argument("--" + field.name.replace("_", "-"), action=SingleValue)
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
