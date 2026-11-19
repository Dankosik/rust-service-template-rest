#!/usr/bin/env python3
"""One-shot service initializer: sets identity, prunes unselected profiles, removes template/."""

from __future__ import annotations

import argparse
import html
import importlib.util
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import tomllib
from collections.abc import Sequence
from dataclasses import MISSING, dataclass, fields, replace
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

sys.dont_write_bytecode = True

CHOICES: dict[str, tuple[str, ...]] = {
    "database": ("none", "postgres"),
    "authn": ("none", "oidc-jwt", "oidc-introspection"),
    "outbound_http": ("none", "bounded"),
    "outbound_auth": ("none", "oauth2-client-credentials"),
    "grpc": ("none", "enabled"),
    "http_idempotency": ("none", "postgres"),
    "jobs": ("none", "postgres"),
    "messaging": ("none", "nats-jetstream"),
    "outbox": ("none", "postgres"),
    "webhooks": ("none", "durable"),
    "inbound_webhooks": ("none", "standard-webhooks"),
    "cache": ("none", "redis"),
    "object_storage": ("none", "s3"),
    "service_auth": ("none", "jwt"),
    "agent_harness": ("core", "codex", "claude", "qwen", "cursor", "grok", "opencode", "antigravity", "all"),
}

_SERVICE_NAME_RE = re.compile(r"^[a-z](?:[a-z0-9]|-(?=[a-z0-9])){0,63}$")
_CODEOWNER_RE = re.compile(r"^@[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?(?:/[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)?$")
_RESERVED_CRATE_NAMES = frozenset(
    {
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
        "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "macro", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true",
        "try", "type", "unsafe", "use", "where", "while", "yield", "abstract", "become", "box", "do",
        "final", "override", "priv", "typeof", "unsized", "virtual",
    }
)
_MARKER_PREFIX_RE = re.compile(
    r"^[ \t]*(?:(?:#|//)[ \t]*template:(?:begin|end)(?:[ \t]|$)|<!--[ \t]*template:(?:begin|end)(?:[ \t]|$))"
)
_MARKER_RE = re.compile(
    r"^(?P<indent>[ \t]*)(?:(?P<hash>#)|(?P<slash>//)|(?P<html><!--))"
    r"[ \t]*template:(?P<kind>begin|end)[ \t]+(?P<profile>[a-z0-9]+(?:-[a-z0-9]+)*):(?P<id>[a-z0-9]+(?:-[a-z0-9]+)*)"
    r"(?:(?(html)[ \t]*-->|))[ \t]*$"
)
_MARKER_BYTES_RE = re.compile(
    rb"^[ \t]*(?:(?:#|//)[ \t]*template:(?:begin|end)[ \t]|<!--[ \t]*template:(?:begin|end)[ \t])",
    re.MULTILINE,
)
_WORKSPACE_DEP_RE = re.compile(r"^([A-Za-z0-9_-]+)[ \t]*=")


class InitError(ValueError):
    pass


@dataclass(frozen=True)
class InitInputs:
    service_name: str
    repository: str
    description: str
    codeowner: str
    database: str = "none"
    authn: str = "none"
    outbound_http: str = "none"
    outbound_auth: str = "none"
    grpc: str = "none"
    http_idempotency: str = "none"
    jobs: str = "none"
    messaging: str = "none"
    outbox: str = "none"
    webhooks: str = "none"
    inbound_webhooks: str = "none"
    cache: str = "none"
    object_storage: str = "none"
    service_auth: str = "none"
    agent_harness: str = "all"


def validate_inputs(inputs: InitInputs) -> InitInputs:
    if not _SERVICE_NAME_RE.fullmatch(inputs.service_name) or inputs.service_name.replace("-", "_") in _RESERVED_CRATE_NAMES:
        raise InitError(f"invalid SERVICE_NAME: {inputs.service_name!r}")
    parsed = urlsplit(inputs.repository)
    parts = [p for p in parsed.path.split("/") if p]
    if parsed.scheme != "https" or parsed.netloc != "github.com" or len(parts) != 2 or parsed.query or parsed.fragment:
        raise InitError(f"REPOSITORY must be https://github.com/<owner>/<repo>, got {inputs.repository!r}")
    if not inputs.description.strip() or any(ch in inputs.description for ch in ("\n", "\r")):
        raise InitError("DESCRIPTION must be a single non-empty line")
    if not _CODEOWNER_RE.fullmatch(inputs.codeowner):
        raise InitError(f"invalid CODEOWNER: {inputs.codeowner!r}")

    for name, allowed in CHOICES.items():
        if getattr(inputs, name) not in allowed:
            raise InitError(f"{name.upper()} must be one of {', '.join(allowed)}")

    if inputs.outbound_auth == "oauth2-client-credentials":
        inputs = replace(inputs, outbound_http="bounded")
    if inputs.http_idempotency == "postgres":
        if inputs.database != "postgres":
            raise InitError("HTTP_IDEMPOTENCY=postgres requires DATABASE=postgres")
        if inputs.authn == "none":
            raise InitError("HTTP_IDEMPOTENCY=postgres requires AUTHN=oidc-jwt or oidc-introspection")
    if inputs.jobs == "postgres" and inputs.database != "postgres":
        raise InitError("JOBS=postgres requires DATABASE=postgres")
    if inputs.outbox == "postgres" and (inputs.database != "postgres" or inputs.jobs != "postgres" or inputs.messaging != "nats-jetstream"):
        raise InitError("OUTBOX=postgres requires DATABASE=postgres, JOBS=postgres, and MESSAGING=nats-jetstream")
    if inputs.webhooks == "durable" and (inputs.database != "postgres" or inputs.jobs != "postgres" or inputs.outbound_http != "bounded"):
        raise InitError("WEBHOOKS=durable requires DATABASE=postgres, JOBS=postgres, and OUTBOUND_HTTP=bounded")
    if inputs.inbound_webhooks == "standard-webhooks" and (inputs.database != "postgres" or inputs.jobs != "postgres"):
        raise InitError("INBOUND_WEBHOOKS=standard-webhooks requires DATABASE=postgres and JOBS=postgres")
    if inputs.service_auth == "jwt" and inputs.grpc != "enabled":
        raise InitError("SERVICE_AUTH=jwt requires GRPC=enabled")
    return inputs


def selected_marker_profiles(inputs: InitInputs) -> set[str]:
    selected: set[str] = set()
    if inputs.database == "postgres":
        selected.update(("postgres", "service-secrets"))
    if inputs.authn != "none":
        selected.update(("authn", inputs.authn))
    if inputs.outbound_http == "bounded":
        selected.update(("outbound-http", "request-budget"))
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
    if inputs.grpc == "enabled" and inputs.authn == "oidc-jwt" and inputs.messaging == "nats-jetstream" and inputs.object_storage == "s3":
        selected.add("runtime-progress")
    if inputs.messaging == "nats-jetstream":
        selected.update(("messaging", "config-url"))
    if inputs.service_auth == "jwt":
        selected.add("service-auth")
    if inputs.service_auth == "jwt" or inputs.outbound_auth == "oauth2-client-credentials":
        selected.add("service-test-tls")
    if inputs.authn == "oidc-jwt" or inputs.outbound_auth == "oauth2-client-credentials" or inputs.service_auth == "jwt":
        selected.add("jsonwebtoken")
    if any((inputs.authn != "none", inputs.outbound_http == "bounded", inputs.grpc == "enabled", inputs.cache == "redis", inputs.database == "postgres", inputs.object_storage == "s3")):
        selected.add("tls-fixtures")
    if inputs.http_idempotency == "postgres":
        selected.update(("http-idempotency", "request-budget", "test-metrics"))
        if inputs.authn == "oidc-introspection":
            selected.add("http-idempotency-mounted")
    if inputs.authn == "oidc-introspection" and (inputs.http_idempotency == "postgres" or (inputs.database == "postgres" and inputs.grpc == "enabled")):
        selected.add("test-introspection-fixtures")
    if inputs.jobs == "postgres":
        selected.update(("jobs", "test-metrics"))
        if inputs.http_idempotency == "postgres":
            selected.add("jobs-http-idempotency")
    if inputs.outbox == "postgres":
        selected.update(("outbox", "test-digest"))
        if inputs.jobs == "postgres" and inputs.webhooks == "durable":
            selected.add("jobs-reference")
    if inputs.jobs == "postgres" or inputs.messaging == "nats-jetstream":
        selected.add("worker")
    if inputs.cache == "redis":
        selected.add("cache")
    if inputs.object_storage == "s3":
        selected.add("object-storage")
    if any((inputs.grpc == "enabled", inputs.cache == "redis", inputs.outbound_http == "bounded", inputs.object_storage == "s3")):
        selected.add("rustls")
    if any((inputs.database == "postgres", inputs.messaging == "nats-jetstream", inputs.cache == "redis", inputs.object_storage == "s3")):
        selected.add("integration")
    if any((inputs.database == "postgres", inputs.messaging == "nats-jetstream", inputs.cache == "redis", inputs.object_storage == "s3", inputs.outbound_auth == "oauth2-client-credentials")):
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


def _load_harness_module(repo: Path) -> Any:
    script = repo / "scripts/harness-sync.py"
    spec = importlib.util.spec_from_file_location("_template_harness_sync", script)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _load_profiles(root: Path) -> dict[str, Any]:
    return json.loads((root / "template/profiles.json").read_text(encoding="utf-8"))


def _profile_removals(raw: dict[str, Any]) -> dict[str, list[str]]:
    removals: dict[str, list[str]] = {}
    for key, val in raw.items():
        if key in {"schema_version", "source_only", "identity"}:
            continue
        removals[key] = list(val.get("remove_when_none", val.get("remove_when_unselected", [])))
    return removals


def _apply_markers(root: Path, valid_profiles: set[str], selected: set[str]) -> None:
    for path in sorted(root.rglob("*")):
        if any(p in {".git", "target", "__pycache__"} for p in path.relative_to(root).parts):
            continue
        if path.is_symlink() or not path.is_file():
            continue
        raw = path.read_bytes()
        if not _MARKER_BYTES_RE.search(raw):
            continue
        rel = path.relative_to(root).as_posix()
        lines = raw.decode("utf-8").splitlines(keepends=True)
        stack: list[tuple[str, str]] = []
        seen: set[tuple[str, str]] = set()
        out: list[str] = []
        changed = False
        for line in lines:
            matched = _MARKER_RE.fullmatch(line.rstrip("\r\n"))
            if matched is None:
                if _MARKER_PREFIX_RE.match(line):
                    raise InitError(f"malformed template marker in {rel}")
                if not stack or stack[-1][0] in selected:
                    out.append(line)
                continue
            changed = True
            kind, profile, marker_id = matched.group("kind"), matched.group("profile"), matched.group("id")
            if profile not in valid_profiles:
                raise InitError(f"unknown template profile {profile!r} in {rel}")
            if kind == "begin":
                if stack:
                    raise InitError(f"nested template marker in {rel}")
                if (profile, marker_id) in seen:
                    raise InitError(f"duplicate template marker {profile}:{marker_id} in {rel}")
                stack.append((profile, marker_id))
                seen.add((profile, marker_id))
            else:
                if stack != [(profile, marker_id)]:
                    raise InitError(f"mismatched template end marker in {rel}")
                stack.pop()
        if stack:
            raise InitError(f"unclosed template marker in {rel}")
        if changed:
            path.write_text("".join(out), encoding="utf-8")


def _encode_value(value: str, encoding: str) -> str:
    if encoding in {"text", "url"}:
        return value
    if encoding in {"rust_string", "toml_string", "json_string"}:
        return json.dumps(value, ensure_ascii=False)[1:-1]
    if encoding == "markdown":
        return re.sub(r"([\\`*_{}\[\]()<>#+.!|])", r"\\\1", html.escape(value, quote=False))
    raise InitError(f"unsupported encoding: {encoding}")


def _apply_identity(root: Path, anchors: Sequence[dict[str, Any]], inputs: InitInputs) -> None:
    repo_path = inputs.repository.removeprefix("https://github.com/")
    for anchor in anchors:
        path = root / anchor["path"]
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8")
        if anchor["path"] == "crates/service/Cargo.toml" and anchor["old"] == 'name = "service"':
            section, count, out = "", 0, []
            for line in text.splitlines(keepends=True):
                if line.strip() in {"[package]", "[lib]", "[[bin]]"}:
                    section = line.strip()
                if line == 'name = "service"\n' and section in {"[package]", "[[bin]]"}:
                    out.append(f'name = "{inputs.service_name}"\n')
                    count += 1
                    continue
                out.append(line)
            if count != 2:
                raise InitError("crates/service/Cargo.toml identity anchors changed")
            path.write_text("".join(out), encoding="utf-8")
            continue
        if text.count(anchor["old"]) != anchor["count"]:
            raise InitError(f"identity anchor count mismatch in {anchor['path']}: {anchor['old']!r}")
        enc = anchor["encoding"]
        values = {
            "service_name": _encode_value(inputs.service_name, enc),
            "service_name_underscore": _encode_value(inputs.service_name.replace("-", "_"), enc),
            "repository": _encode_value(inputs.repository, enc),
            "repository_path": _encode_value(repo_path, enc),
            "description": _encode_value(inputs.description, enc),
            "codeowner": _encode_value(inputs.codeowner, enc),
        }
        path.write_text(text.replace(anchor["old"], anchor["template"].format_map(values)), encoding="utf-8")


def _remove_paths(root: Path, paths: Sequence[str]) -> None:
    ordered = sorted(set(paths), key=lambda p: (len(Path(p.rstrip("/")).parts), p))
    kept: list[str] = []
    for item in ordered:
        plain = item.rstrip("/")
        if any(plain.startswith(f"{parent.rstrip('/')}/") for parent in kept):
            continue
        kept.append(item)
    for item in kept:
        target = root / item.rstrip("/")
        if not target.exists() and not target.is_symlink():
            raise InitError(f"planned removal is missing: {item}")
        if target.is_symlink() or target.is_file():
            target.unlink()
        elif target.is_dir():
            shutil.rmtree(target)


def _prune_empty_dirs(root: Path) -> None:
    if not root.is_dir() or root.is_symlink():
        return
    for child in sorted(root.iterdir()):
        if child.is_dir() and not child.is_symlink():
            _prune_empty_dirs(child)
    if not any(root.iterdir()):
        root.rmdir()


def _prune_workspace_dependencies(root: Path) -> None:
    cargo_path = root / "Cargo.toml"
    text = cargo_path.read_text(encoding="utf-8")
    declared = set(tomllib.loads(text)["workspace"]["dependencies"])
    inherited: set[str] = set()
    manifests = [*sorted(root.glob("crates/*/Cargo.toml")), root / "test/Cargo.toml"]
    for manifest in manifests:
        if not manifest.is_file():
            continue
        parsed = tomllib.loads(manifest.read_text(encoding="utf-8"))
        sections = [parsed.get(k, {}) for k in ("dependencies", "dev-dependencies", "build-dependencies")]
        for cond in parsed.get("target", {}).values():
            if isinstance(cond, dict):
                sections.extend(cond.get(k, {}) for k in ("dependencies", "dev-dependencies", "build-dependencies"))
        for sec in sections:
            if isinstance(sec, dict):
                inherited.update(k for k, v in sec.items() if isinstance(v, dict) and v.get("workspace") is True)
    unused = declared - inherited
    if not unused:
        return
    kept_lines: list[str] = []
    table = ""
    for line in text.splitlines(keepends=True):
        if line.startswith("["):
            table = line.strip()
        m = _WORKSPACE_DEP_RE.match(line)
        if table == "[workspace.dependencies]" and m and m.group(1) in unused:
            while kept_lines and kept_lines[-1].lstrip().startswith("#"):
                kept_lines.pop()
            continue
        kept_lines.append(line)
    cargo_path.write_text("".join(kept_lines), encoding="utf-8")


def _prune_cargo_lock_and_generate(staged: Path, inputs: InitInputs) -> None:
    env = os.environ.copy()
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    if not os.path.isabs(env.get("CARGO_TARGET_DIR", "")):
        env["CARGO_TARGET_DIR"] = os.fspath(staged.parent / "cargo-target")

    def run_cmd(cmd: Sequence[str], label: str) -> bytes:
        res = subprocess.run(cmd, cwd=staged, env=env, capture_output=True, check=False)
        if res.returncode != 0:
            err = res.stderr.decode("utf-8", errors="replace").strip()
            raise InitError(f"{label} failed (exit {res.returncode})\n{err[:4096]}")
        return res.stdout

    def lock_ids(lock_path: Path) -> set[tuple[str, str, str, str]]:
        pkgs = tomllib.loads(lock_path.read_text(encoding="utf-8"))["package"]
        return {(p["name"], p["version"], p.get("source", ""), p.get("checksum", "")) for p in pkgs}

    lock_path = staged / "Cargo.lock"
    before = lock_ids(lock_path)
    run_cmd(["cargo", "metadata", "--offline", "--format-version", "1"], "offline Cargo.lock pruning")
    for name, _ver, src, csum in lock_ids(lock_path) - before:
        if name != inputs.service_name or src or csum:
            raise InitError(f"Cargo resolved unpinned package `{name}`")

    run_cmd(["cargo", "fmt", "--all"], "cargo fmt")
    openapi = run_cmd(
        ["cargo", "run", "-q", "-p", inputs.service_name, "--bin", "openapi", "--locked", "--offline"],
        "OpenAPI regeneration",
    )
    (staged / "api/openapi/service.yaml").write_bytes(openapi)


def project_tree(
    source_root: Path,
    staged: Path,
    inputs: InitInputs,
    *,
    run_cargo: bool = True,
    from_git_head: bool = True,
) -> None:
    inputs = validate_inputs(inputs)
    if from_git_head:
        staged.mkdir(parents=True, exist_ok=True)
        archive = subprocess.run(
            ["git", "-C", os.fspath(source_root), "archive", "--format=tar", "HEAD"],
            capture_output=True,
            check=True,
        )
        subprocess.run(["tar", "-xf", "-", "-C", os.fspath(staged)], input=archive.stdout, check=True)
    else:
        files = subprocess.run(
            ["git", "-C", os.fspath(source_root), "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
            capture_output=True,
            check=True,
        ).stdout.split(b"\0")
        for raw in files:
            if not raw:
                continue
            rel = raw.decode("utf-8")
            if rel.startswith(("specs/", "target/")) or "__pycache__" in rel.split("/"):
                continue
            src = source_root / rel
            if not src.exists() or src.is_symlink() or not src.is_file():
                continue
            dst = staged / rel
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dst)

    profiles_raw = _load_profiles(staged)
    removals = _profile_removals(profiles_raw)
    selected = selected_marker_profiles(inputs)

    _apply_markers(staged, set(removals), selected)
    _apply_identity(staged, profiles_raw["identity"], inputs)

    to_remove = list(profiles_raw["source_only"])
    for profile, paths in removals.items():
        if profile not in selected:
            to_remove.extend(paths)
    _remove_paths(staged, to_remove)

    harness = _load_harness_module(staged)
    selected_adapters = set(harness.selected_adapters(inputs.agent_harness))
    for name, pack in harness.ADAPTERS.items():
        if name not in selected_adapters:
            for rel in (*pack.canonical, *pack.generated, *pack.settings):
                target = staged / rel.rstrip("/")
                if target.is_symlink() or target.is_file():
                    target.unlink()
                elif target.is_dir():
                    shutil.rmtree(target)
    _prune_empty_dirs(staged / ".agents/adapters")

    res = subprocess.run(
        [sys.executable, "-B", "scripts/harness-sync.py", "--apply", "--repo", os.fspath(staged), "--harness", inputs.agent_harness],
        cwd=staged,
        capture_output=True,
        text=True,
        check=False,
    )
    if res.returncode != 0:
        raise InitError(f"harness sync failed: {res.stderr.strip()}")

    _prune_workspace_dependencies(staged)
    if run_cargo:
        _prune_cargo_lock_and_generate(staged, inputs)

    for path in sorted(staged.rglob("*")):
        if path.is_symlink() or not path.is_file():
            continue
        if _MARKER_BYTES_RE.search(path.read_bytes()):
            raise InitError(f"unstripped template marker remains in {path.relative_to(staged).as_posix()}")


def _sync_staged_to_repo(staged: Path, repo: Path) -> None:
    tracked = {
        raw.decode("utf-8")
        for raw in subprocess.run(
            ["git", "-C", os.fspath(repo), "ls-files", "-z"], capture_output=True, check=True
        ).stdout.split(b"\0")
        if raw
    }
    staged_rel: set[str] = set()
    for path in sorted(staged.rglob("*")):
        rel = path.relative_to(staged).as_posix()
        if path.is_dir() and not path.is_symlink():
            continue
        staged_rel.add(rel)
        dst = repo / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        if path.is_symlink():
            if dst.exists() or dst.is_symlink():
                dst.unlink()
            os.symlink(os.readlink(path), dst)
        else:
            mode = 0o755 if path.stat().st_mode & stat.S_IXUSR else 0o644
            dst.write_bytes(path.read_bytes())
            os.chmod(dst, mode)

    for rel in sorted(tracked - staged_rel):
        target = repo / rel
        if target.is_symlink() or target.is_file():
            target.unlink()
    for rel in sorted(tracked - staged_rel, key=lambda p: len(Path(p).parts), reverse=True):
        parent = (repo / rel).parent
        while parent != repo and parent.is_dir() and not any(parent.iterdir()):
            parent.rmdir()
            parent = parent.parent


def _read_arg(args: argparse.Namespace, name: str, default: str | None) -> str:
    cli_val = getattr(args, name)
    env_val = os.environ.get(name.upper())
    if cli_val is not None and env_val is not None:
        raise InitError(f"{name.upper()} supplied via both CLI flag and environment")
    val = cli_val if cli_val is not None else env_val
    if val is None:
        if default is None:
            raise InitError(f"{name.upper()} is required")
        return default
    return val


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    for f in fields(InitInputs):
        parser.add_argument("--" + f.name.replace("_", "-"), default=None)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        defaults = {f.name: (None if f.default is MISSING else f.default) for f in fields(InitInputs)}
        inputs = validate_inputs(
            InitInputs(**{f.name: _read_arg(args, f.name, defaults[f.name]) for f in fields(InitInputs)})
        )
        repo = Path(
            subprocess.run(
                ["git", "-C", os.fspath(args.repo), "rev-parse", "--show-toplevel"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout.strip()
        ).resolve()
        status = subprocess.run(
            ["git", "-C", os.fspath(repo), "status", "--porcelain=v1", "--untracked-files=no"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        if status:
            raise InitError("initializer requires a clean tracked git checkout")
        with tempfile.TemporaryDirectory(prefix="template-init-") as tmp:
            staged = Path(tmp) / "staged"
            project_tree(repo, staged, inputs, run_cargo=True, from_git_head=True)
            _sync_staged_to_repo(staged, repo)
        print(f"template init: initialized {inputs.service_name}; review `git status` and commit")
        return 0
    except (InitError, subprocess.CalledProcessError, OSError) as exc:
        print(f"template init: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
