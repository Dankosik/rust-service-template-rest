#!/usr/bin/env python3
"""Compare all profile projections using the initializer's canonical projector."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import stat
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


sys.dont_write_bytecode = True

DATABASES = ("none", "postgres")
AUTHN = ("none", "oidc-jwt", "oidc-introspection")
OUTBOUND_HTTP = ("none", "bounded")
HARNESSES = ("core", "codex", "claude", "qwen", "cursor", "grok", "opencode", "all")
_RUNTIME_FILES = frozenset({"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "template.lock", "Makefile", "build.rs"})
_RUNTIME_PREFIXES = (
    "api/",
    "build/",
    "crates/",
    "docs/",
    "env/",
    "migrations/",
    "make/",
    "scripts/",
    "test/",
    ".cargo/",
    ".github/",
)
_DOT_ROOTS = frozenset({".agents", ".claude", ".codex", ".cursor", ".grok", ".opencode", ".qwen"})


@dataclass(frozen=True)
class Node:
    kind: str
    mode: int
    payload: bytes | str | None = None


@dataclass(frozen=True)
class AdmittedNode:
    kind: str
    mode: int
    target: str | None = None


_ROLE_LAYOUTS = {
    ".codex/agents": "toml",
    ".claude/agents": "md",
    ".qwen/agents": "md",
    ".grok/agents": "md",
    ".grok/roles": "toml",
    ".cursor/agents": "md",
    ".opencode/agents": "md",
}
_SKILL_LAYOUTS = (".claude/skills", ".qwen/skills")
_SESSION_ROLE_NAMES = {
    ".claude/agents": frozenset({"acceptance-unit-lead"}),
    ".qwen/agents": frozenset({"acceptance-unit-lead"}),
    ".grok/agents": frozenset({"orchestrator", "acceptance-unit-lead"}),
    ".cursor/agents": frozenset({"acceptance-unit-lead"}),
    ".opencode/agents": frozenset({"orchestrator", "acceptance-unit-lead"}),
}
_CARRIER_NAME_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")


def _load_initializer(source: Path):
    library = source / "scripts/lib"
    module_path = library / "template_init.py"
    if not library.is_dir() or not module_path.is_file():
        raise RuntimeError("source initializer is missing")
    sys.path.insert(0, os.fspath(library))
    try:
        spec = importlib.util.spec_from_file_location("template_profile_projections_init", module_path)
        if spec is None or spec.loader is None:
            raise RuntimeError("source initializer cannot be imported")
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        return module
    finally:
        sys.path.remove(os.fspath(library))


def _load_safety(source: Path):
    module_path = source / "scripts/tests/template-init-safety.py"
    if not module_path.is_file():
        raise RuntimeError("source initializer safety fixture is missing")
    spec = importlib.util.spec_from_file_location("template_profile_projections_safety", module_path)
    if spec is None or spec.loader is None:
        raise RuntimeError("source initializer safety fixture cannot be imported")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _checker_identity() -> str:
    return hashlib.sha256(Path(__file__).read_bytes()).hexdigest()


def _emit(record: str, **values: object) -> None:
    print(json.dumps({"record": record, **values}, ensure_ascii=False, sort_keys=True))


def _tree(root: Path, initializer) -> dict[str, Node]:
    result: dict[str, Node] = {}

    def visit(directory: Path) -> None:
        for path in sorted(directory.iterdir(), key=lambda item: item.name):
            relative = path.relative_to(root).as_posix()
            mode = path.lstat().st_mode
            if stat.S_ISDIR(mode):
                result[relative] = Node("directory", stat.S_IMODE(mode))
                visit(path)
            elif stat.S_ISREG(mode):
                result[relative] = Node("file", stat.S_IMODE(mode), path.read_bytes())
            elif stat.S_ISLNK(mode):
                # Git records a symlink's target, not host permission bits.
                # macOS commonly reports 0755 here while Linux reports 0777.
                result[relative] = Node("symlink", 0, os.readlink(path))
            else:
                raise initializer.Refusal(f"projection tree has an unsupported entry: {relative}")

    visit(root)
    return result


def _tree_digest(nodes: dict[str, Node]) -> str:
    digest = hashlib.sha256()
    for relative, node in sorted(nodes.items()):
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        digest.update(node.kind.encode("ascii"))
        digest.update(b"\0")
        digest.update(f"{node.mode:o}".encode("ascii"))
        digest.update(b"\0")
        if isinstance(node.payload, bytes):
            digest.update(node.payload)
        elif isinstance(node.payload, str):
            digest.update(node.payload.encode("utf-8"))
        digest.update(b"\0")
    return digest.hexdigest()


def _scope_intersects(left: str, right: str) -> bool:
    return left == right or left.startswith(f"{right}/") or right.startswith(f"{left}/")


def _adapter_scopes(initializer) -> tuple[str, ...]:
    scopes = {
        initializer.safe_relative(path, allow_directory=True)
        for pack in initializer.ADAPTERS.values()
        for path in (*pack.canonical, *pack.generated, *pack.settings)
    }
    return tuple(sorted(scopes))


def _validated_exclusions(initializer, requested: Iterable[str] | None = None) -> tuple[str, ...]:
    approved = set(_adapter_scopes(initializer))
    scopes = approved if requested is None else set(requested)
    if not scopes.issubset(approved):
        raise initializer.Refusal("projection exclusion is not an exact adapter-owned path")
    for scope in scopes:
        plain = scope.rstrip("/")
        if plain in _DOT_ROOTS:
            raise initializer.Refusal("projection exclusion may not name a dot-directory root")
        if plain in _RUNTIME_FILES or any(_scope_intersects(plain, owner.rstrip("/")) for owner in _RUNTIME_PREFIXES):
            raise initializer.Refusal("projection exclusion intersects a runtime owner")
    return tuple(sorted(scopes))


def _excluded(relative: str, scopes: Iterable[str]) -> bool:
    return any(relative == scope.rstrip("/") or relative.startswith(f"{scope.rstrip('/')}/") for scope in scopes)


def _canonical_role_names(source: Path, initializer) -> frozenset[str]:
    root = source / ".agents/roles"
    if root.is_symlink() or not root.is_dir():
        raise initializer.Refusal("canonical role source is missing or unsafe")
    names: set[str] = set()
    for path in root.iterdir():
        if path.is_symlink() or not path.is_file() or path.suffix != ".toml":
            raise initializer.Refusal("canonical role source has an unsupported entry")
        name = path.stem
        if not _CARRIER_NAME_RE.fullmatch(name):
            raise initializer.Refusal("canonical role source has an unsafe role name")
        names.add(name)
    if not names:
        raise initializer.Refusal("canonical role source has no roles")
    return frozenset(names)


def _canonical_skill_names(source: Path, initializer) -> frozenset[str]:
    root = source / ".agents/skills"
    if root.is_symlink() or not root.is_dir():
        raise initializer.Refusal("canonical skill source is missing or unsafe")
    names: set[str] = set()
    for path in root.iterdir():
        if not path.is_dir() or path.is_symlink():
            continue
        if not _CARRIER_NAME_RE.fullmatch(path.name):
            raise initializer.Refusal("canonical skill source has an unsafe skill name")
        names.add(path.name)
    if not names:
        raise initializer.Refusal("canonical skill source has no skills")
    return frozenset(names)


def _require_admitted(
    nodes: dict[str, Node], path: str, kind: str, mode: int, initializer, *, target: str | None = None
) -> AdmittedNode:
    node = nodes.get(path)
    if node is None or node.kind != kind or node.mode != mode:
        raise initializer.Refusal(f"all-harness carrier has an invalid type or mode: {path}")
    if target is not None and node.payload != target:
        raise initializer.Refusal(f"all-harness skill link has an invalid target: {path}")
    return AdmittedNode(kind, mode, target)


def _admitted_adapter_nodes(
    source: Path, nodes: dict[str, Node], exclusions: Iterable[str], initializer
) -> dict[str, AdmittedNode]:
    roles = _canonical_role_names(source, initializer)
    skills = _canonical_skill_names(source, initializer)
    admitted: dict[str, AdmittedNode] = {}
    for scope in exclusions:
        plain = scope.rstrip("/")
        if not scope.endswith("/"):
            admitted[plain] = _require_admitted(nodes, plain, "file", 0o644, initializer)
            continue
        if plain in _ROLE_LAYOUTS:
            extension = _ROLE_LAYOUTS[plain]
            admitted[plain] = _require_admitted(nodes, plain, "directory", 0o755, initializer)
            names = roles | _SESSION_ROLE_NAMES.get(plain, frozenset())
            for name in names:
                path = f"{plain}/{name}.{extension}"
                if path in nodes:
                    admitted[path] = _require_admitted(nodes, path, "file", 0o644, initializer)
            continue
        if plain in _SKILL_LAYOUTS:
            admitted[plain] = _require_admitted(nodes, plain, "directory", 0o755, initializer)
            for name in skills:
                path = f"{plain}/{name}"
                admitted[path] = _require_admitted(
                    nodes,
                    path,
                    "symlink",
                    0,
                    initializer,
                    target=f"../../.agents/skills/{name}",
                )
            continue
        raise initializer.Refusal("adapter generated owner has no admitted carrier rule")

    for path in nodes:
        if _excluded(path, exclusions) and path not in admitted:
            raise initializer.Refusal(f"all-harness carrier has an unadmitted entry: {path}")
    return admitted


def _validate_admitted_nodes(
    nodes: dict[str, Node], exclusions: Iterable[str], admitted: dict[str, AdmittedNode], initializer
) -> None:
    for path, node in nodes.items():
        if not _excluded(path, exclusions):
            continue
        expected = admitted.get(path)
        if expected is None:
            raise initializer.Refusal(f"projection carrier has an unadmitted entry: {path}")
        if node.kind != expected.kind or node.mode != expected.mode:
            raise initializer.Refusal(f"projection carrier has a type or mode drift: {path}")
        if expected.target is not None and node.payload != expected.target:
            raise initializer.Refusal(f"projection carrier has a symlink target drift: {path}")


def _compare(
    reference: dict[str, Node],
    actual: dict[str, Node],
    exclusions: Iterable[str],
    admitted: dict[str, AdmittedNode],
    initializer,
) -> None:
    _validate_admitted_nodes(reference, exclusions, admitted, initializer)
    _validate_admitted_nodes(actual, exclusions, admitted, initializer)
    reference_filtered = {path: node for path, node in reference.items() if path not in admitted}
    actual_filtered = {path: node for path, node in actual.items() if path not in admitted}
    if reference_filtered.keys() != actual_filtered.keys():
        missing = sorted(reference_filtered.keys() - actual_filtered.keys())
        extra = sorted(actual_filtered.keys() - reference_filtered.keys())
        raise initializer.Refusal(f"projection equality has path drift: missing={missing[:1]} extra={extra[:1]}")
    for path, expected in reference_filtered.items():
        if actual_filtered[path] != expected:
            raise initializer.Refusal(f"projection equality has type, mode, byte, or link drift: {path}")


def _compare_lock(reference: bytes, actual: bytes, harness: str, initializer) -> None:
    try:
        reference_value = json.loads(reference)
        actual_value = json.loads(actual)
    except json.JSONDecodeError as error:
        raise initializer.Refusal("projection lock serialization is not JSON") from error
    reference_profiles = reference_value.get("profiles") if isinstance(reference_value, dict) else None
    actual_profiles = actual_value.get("profiles") if isinstance(actual_value, dict) else None
    if not isinstance(reference_profiles, dict) or not isinstance(actual_profiles, dict):
        raise initializer.Refusal("projection lock serialization has an invalid profile shape")
    if actual_profiles.get("agent_harness") != harness:
        raise initializer.Refusal("projection lock serialization has the wrong harness")
    normalized = json.loads(json.dumps(actual_value))
    assert isinstance(normalized, dict)
    normalized_profiles = normalized.get("profiles")
    assert isinstance(normalized_profiles, dict)
    normalized_profiles["agent_harness"] = reference_profiles.get("agent_harness")
    normalized_bytes = (json.dumps(normalized, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
    if normalized_bytes != reference:
        raise initializer.Refusal("projection lock serialization differs beyond agent_harness")


def _inputs(initializer, database: str, authn: str, outbound_http: str, harness: str):
    service_name = f"matrix-{database}-{authn}-{outbound_http}-core"
    return initializer.InitInputs(
        service_name=service_name,
        repository=f"https://github.com/example/{service_name}",
        description=f"Matrix {database} {authn} {outbound_http} core",
        codeowner="@example/platform",
        database=database,
        authn=authn,
        outbound_http=outbound_http,
        agent_harness=harness,
    )


def _project(source: Path, candidate: str, initializer, inputs, destination: Path) -> dict[str, Node]:
    initializer.snapshot_tree(source, destination, candidate)
    profiles = initializer._profile_data(destination)
    initializer._project_staged(destination, inputs, profiles)
    initializer._postconditions(destination, inputs, profiles, initial=True)
    return _tree(destination, initializer)


def check(source: Path) -> None:
    initializer = _load_initializer(source)
    source = initializer.git_root(source)
    initializer._tracked_checkout_is_clean(source)
    candidate = initializer.git_head(source)
    exclusions = _validated_exclusions(initializer)
    _emit(
        "header",
        candidate=candidate,
        checker_sha256=_checker_identity(),
        exclusions=exclusions,
        selections=len(DATABASES) * len(AUTHN) * len(OUTBOUND_HTTP) * len(HARNESSES),
    )
    with tempfile.TemporaryDirectory(prefix="template-profile-projections-") as temporary:
        work = Path(temporary)
        for database in DATABASES:
            for authn in AUTHN:
                for outbound_http in OUTBOUND_HTTP:
                    reference: dict[str, Node] | None = None
                    reference_lock: bytes | None = None
                    identity = _inputs(initializer, database, authn, outbound_http, "core").identity()
                    all_inputs = _inputs(initializer, database, authn, outbound_http, "all")
                    with tempfile.TemporaryDirectory(
                        prefix=f"{database}-{authn}-{outbound_http}-all-", dir=work
                    ) as selection:
                        all_nodes = _project(source, candidate, initializer, all_inputs, Path(selection) / "tree")
                    admitted = _admitted_adapter_nodes(source, all_nodes, exclusions, initializer)
                    projected = {"all": all_nodes}
                    for harness in HARNESSES:
                        inputs = _inputs(initializer, database, authn, outbound_http, harness)
                        if inputs.identity() != identity:
                            raise initializer.Refusal("harness changed a runtime profile identity")
                        if harness in projected:
                            nodes = projected[harness]
                        else:
                            with tempfile.TemporaryDirectory(
                                prefix=f"{database}-{authn}-{outbound_http}-{harness}-", dir=work
                            ) as selection:
                                nodes = _project(source, candidate, initializer, inputs, Path(selection) / "tree")
                        digest = _tree_digest(nodes)
                        lock = initializer._lock_bytes(inputs, candidate, "complete")
                        lock_sha256 = hashlib.sha256(lock).hexdigest()
                        _emit(
                            "selection",
                            database=database,
                            authn=authn,
                            outbound_http=outbound_http,
                            harness=harness,
                            identity=inputs.identity(),
                            profiles=inputs.profiles(),
                            tree_sha256=digest,
                            lock_sha256=lock_sha256,
                        )
                        if harness == "core":
                            reference = nodes
                            reference_lock = lock
                            _emit(
                                "equality",
                                database=database,
                                authn=authn,
                                outbound_http=outbound_http,
                                harness=harness,
                                reference="core",
                                tree_result="reference",
                                lock_result="reference",
                            )
                            continue
                        assert reference is not None and reference_lock is not None
                        _compare(reference, nodes, exclusions, admitted, initializer)
                        _compare_lock(reference_lock, lock, harness, initializer)
                        _emit(
                            "equality",
                            database=database,
                            authn=authn,
                            outbound_http=outbound_http,
                            harness=harness,
                            reference="core",
                            tree_result="equal",
                            lock_result="agent_harness_only",
                        )


def _expect_refusal(initializer, action, label: str) -> None:
    try:
        action()
    except initializer.Refusal:
        return
    raise AssertionError(f"{label} was accepted")


def self_test(source: Path) -> None:
    initializer = _load_initializer(source)
    exclusions = _validated_exclusions(initializer)
    with tempfile.TemporaryDirectory(prefix="template-profile-projections-link-") as temporary:
        link_root = Path(temporary)
        (link_root / "target").write_text("target\n", encoding="utf-8")
        os.symlink("target", link_root / "link")
        if _tree(link_root, initializer).get("link") != Node("symlink", 0, "target"):
            raise AssertionError("real symlink did not normalize to Git semantics")
    carrier_scopes = (".claude/agents/", ".claude/skills/")
    admitted = {
        ".claude/agents": AdmittedNode("directory", 0o755),
        ".claude/agents/worker.md": AdmittedNode("file", 0o644),
        ".claude/skills": AdmittedNode("directory", 0o755),
        ".claude/skills/worker": AdmittedNode("symlink", 0, "../../.agents/skills/worker"),
    }
    base = {
        "Cargo.toml": Node("file", 0o644, b"manifest"),
        ".claude": Node("directory", 0o755),
        ".claude/agents": Node("directory", 0o755),
        ".claude/agents/worker.md": Node("file", 0o644, b"worker"),
        ".claude/skills": Node("directory", 0o755),
        ".claude/skills/worker": Node("symlink", 0, "../../.agents/skills/worker"),
        "link": Node("symlink", 0o777, "target"),
    }
    _compare(base, dict(base), carrier_scopes, admitted, initializer)
    _expect_refusal(initializer, lambda: _compare(base, {key: value for key, value in base.items() if key != "Cargo.toml"}, carrier_scopes, admitted, initializer), "path drift")
    byte_drift = dict(base)
    byte_drift["Cargo.toml"] = Node("file", 0o644, b"changed manifest")
    _expect_refusal(initializer, lambda: _compare(base, byte_drift, carrier_scopes, admitted, initializer), "byte drift")
    mode_drift = dict(base)
    mode_drift["Cargo.toml"] = Node("file", 0o755, b"manifest")
    _expect_refusal(initializer, lambda: _compare(base, mode_drift, carrier_scopes, admitted, initializer), "mode drift")
    type_drift = dict(base)
    type_drift[".claude"] = Node("file", 0o644, b"not a directory")
    _expect_refusal(initializer, lambda: _compare(base, type_drift, carrier_scopes, admitted, initializer), "type drift")
    link_drift = dict(base)
    link_drift["link"] = Node("symlink", 0o777, "other-target")
    _expect_refusal(initializer, lambda: _compare(base, link_drift, carrier_scopes, admitted, initializer), "symlink drift")
    role_mode_drift = dict(base)
    role_mode_drift[".claude/agents/worker.md"] = Node("file", 0o755, b"worker")
    _expect_refusal(initializer, lambda: _compare(base, role_mode_drift, carrier_scopes, admitted, initializer), "admitted role mode drift")
    skill_link_drift = dict(base)
    skill_link_drift[".claude/skills/worker"] = Node("symlink", 0, "../../.agents/skills/other")
    _expect_refusal(initializer, lambda: _compare(base, skill_link_drift, carrier_scopes, admitted, initializer), "admitted skill link drift")
    for label, path, node in (
        ("hidden runtime file", ".claude/agents/hidden.rs", Node("file", 0o644, b"rust")),
        ("hidden Cargo file", ".claude/agents/Cargo.toml", Node("file", 0o644, b"cargo")),
        ("unexpected nested directory", ".claude/agents/nested", Node("directory", 0o755)),
        ("excluded root symlink", ".claude/agents", Node("symlink", 0, "outside")),
    ):
        drift = dict(base)
        drift[path] = node
        _expect_refusal(initializer, lambda drift=drift: _compare(base, drift, carrier_scopes, admitted, initializer), label)
    _expect_refusal(initializer, lambda: _validated_exclusions(initializer, ("Cargo.toml",)), "runtime exclusion")
    _expect_refusal(initializer, lambda: _validated_exclusions(initializer, (".agents/",)), "broad dot-directory exclusion")
    pack_type = next(iter(initializer.ADAPTERS.values())).__class__
    initializer.ADAPTERS["malicious-runtime-owner"] = pack_type(
        canonical=(".cargo/config.toml",),
        generated=(),
        settings=(),
    )
    try:
        _expect_refusal(initializer, lambda: _validated_exclusions(initializer), "malicious adapter runtime exclusion")
    finally:
        initializer.ADAPTERS.pop("malicious-runtime-owner", None)
    candidate = "0" * 40
    core_inputs = _inputs(initializer, "none", "none", "none", "core")
    codex_inputs = _inputs(initializer, "none", "none", "none", "codex")
    core_lock = initializer._lock_bytes(core_inputs, candidate, "complete")
    codex_lock = initializer._lock_bytes(codex_inputs, candidate, "complete")
    _compare_lock(core_lock, codex_lock, "codex", initializer)
    lock_drift = json.loads(codex_lock)
    lock_drift["identity"]["service_name"] = "different"
    lock_drift_bytes = (json.dumps(lock_drift, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
    _expect_refusal(initializer, lambda: _compare_lock(core_lock, lock_drift_bytes, "codex", initializer), "lock drift")
    outbound_lock_drift = json.loads(core_lock)
    outbound_lock_drift["profiles"]["outbound_http"] = "bounded"
    outbound_lock_drift_bytes = (json.dumps(outbound_lock_drift, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
    _expect_refusal(
        initializer,
        lambda: _compare_lock(core_lock, outbound_lock_drift_bytes, "core", initializer),
        "outbound profile lock drift",
    )
    outbound_tree_drift = dict(base)
    outbound_tree_drift["crates/infra-outbound-http/src/lib.rs"] = Node(
        "file", 0o644, b"outbound profile runtime"
    )
    _expect_refusal(
        initializer,
        lambda: _compare(base, outbound_tree_drift, carrier_scopes, admitted, initializer),
        "outbound profile runtime drift",
    )
    safety = _load_safety(source)
    with tempfile.TemporaryDirectory(prefix="template-profile-projections-self-test-") as temporary:
        safety.assert_preflight_extraction(source, Path(temporary))
    _emit("self-test", checker_sha256=_checker_identity(), result="passed")


def main() -> int:
    parser = argparse.ArgumentParser(description="compare canonical template profile projections")
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--self-test", action="store_true")
    arguments = parser.parse_args()
    try:
        source = arguments.source.resolve(strict=True)
        if arguments.self_test:
            self_test(source)
        else:
            check(source)
        return 0
    except (OSError, RuntimeError, ValueError) as error:
        print(f"template profile projections: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
