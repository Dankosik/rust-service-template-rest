#!/usr/bin/env python3
"""Validate tool pins, container image digests, publish-age cooldowns, and npx wrapper usage."""

from __future__ import annotations

import argparse
import os
import re
import shlex
import shutil
import subprocess
import sys
import tomllib
from collections.abc import Mapping, Sequence
from pathlib import Path

sys.dont_write_bytecode = True

MANIFEST = "tools/versions.env"
DOCKERFILE = "build/docker/Dockerfile"
TOOLCHAIN = "rust-toolchain.toml"
COMPOSE_FILE = "env/docker-compose.yml"

_PIN_LINE_RE = re.compile(r"^([A-Z][A-Z0-9_]*)=([A-Za-z0-9._:@/+-]+)$")
_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
_IMAGE_DIGEST_RE = re.compile(r"^[^\s@]+@sha256:[0-9a-f]{64}$")
_SEMVER_RE = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+")
_EXACT_SEMVER_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")

_DOCKERFILE_TOOL_ARGS = (
    "CARGO_CHEF_VERSION",
    "CARGO_CHEF_X86_64_SHA256",
    "CARGO_CHEF_AARCH64_SHA256",
    "CARGO_AUDITABLE_VERSION",
    "CARGO_AUDITABLE_X86_64_SHA256",
    "CARGO_AUDITABLE_AARCH64_SHA256",
)

_CARGO_TOOLS = (
    ("CARGO_DENY", "cargo-deny", "CARGO_DENY_VERSION"),
    ("CARGO_NEXTEST", "cargo-nextest", "CARGO_NEXTEST_VERSION"),
    ("CARGO_SHEAR", "cargo-shear", "CARGO_SHEAR_VERSION"),
    ("ZIZMOR", "zizmor", "ZIZMOR_VERSION"),
)

_NPX_SCAN_ROOTS = (
    "Makefile",
    "make",
    ".github",
    "scripts/verify",
    "scripts/checks",
    "scripts/image",
    "scripts/harness",
    "scripts/template",
    "scripts/profiles",
    "scripts/service",
)
_NPX_SCAN_EXCLUDES = frozenset({"scripts/checks/npx.py", "scripts/checks/tools.py"})

_PY_RAW_NPX_PATTERNS = (
    re.compile(r"\[\s*['\"]npx['\"]\s*(?:,|\])"),
    re.compile(r"\(\s*['\"]npx['\"]\s*,\s*['\"](?:-[A-Za-z-]|@|[A-Za-z0-9_.-]+@)"),
    re.compile(r"(?:subprocess\.\w+|os\.exec\w*)\([^#\n]*[\[\(\s]['\"]npx['\"]"),
    re.compile(r"['\"]npx\s+(?:-[A-Za-z-]|@[A-Za-z0-9_.-]|[A-Za-z0-9_.-]+@)"),
)
_SHELL_RAW_NPX_RE = re.compile(r"(?<![A-Za-z0-9_./-])npx(?![A-Za-z0-9_./-])\s+(?=[-@A-Za-z0-9_.$'\"])")
_BENIGN_NPX_PREFIX_RE = re.compile(
    r"\b(?:command\s+-v|which|type|for\s+[A-Za-z_][A-Za-z0-9_]*\s+in|echo|printf)\b[^;&|`]*$"
)


class ToolsCheckError(ValueError):
    pass


def parse_versions_env(repo: Path) -> dict[str, str]:
    manifest_path = repo / MANIFEST
    if not manifest_path.is_file():
        raise ToolsCheckError(f"missing {MANIFEST}")
    pins: dict[str, str] = {}
    for raw_line in manifest_path.read_text(encoding="utf-8").splitlines():
        if not raw_line or raw_line.startswith("#"):
            continue
        match = _PIN_LINE_RE.fullmatch(raw_line)
        if match is None:
            raise ToolsCheckError(f"not a NAME=value line in {MANIFEST}: {raw_line}")
        name, value = match.group(1), match.group(2)
        if name in pins:
            raise ToolsCheckError(f"{name} is pinned twice in {MANIFEST}")
        if name.endswith("_SHA256") and not _SHA256_RE.fullmatch(value):
            raise ToolsCheckError(f"{name} is not a SHA256 digest: {value}")
        if name.endswith("_IMAGE") and not _IMAGE_DIGEST_RE.fullmatch(value):
            raise ToolsCheckError(f"{name} is not pinned by digest: {value}")
        pins[name] = value
    if not pins:
        raise ToolsCheckError(f"{MANIFEST} pins nothing")
    go_version = pins.get("GO_VERSION", "<unset>")
    if not _EXACT_SEMVER_RE.fullmatch(go_version):
        raise ToolsCheckError(f"GO_VERSION in {MANIFEST} is not X.Y.Z: {go_version}")
    return pins


def check_cargo_tools(
    pins: Mapping[str, str], environ: Mapping[str, str], *, skip_cargo_tools: bool = False
) -> list[str]:
    if skip_cargo_tools:
        return []
    messages: list[str] = []
    for var, crate, version_key in _CARGO_TOOLS:
        binary = environ.get(var, "").strip()
        if not binary:
            raise ToolsCheckError(f"{var} is unset; run through make tools-check")
        version = pins.get(version_key)
        if not version:
            raise ToolsCheckError(f"{version_key} is missing in {MANIFEST}")
        resolved = shutil.which(binary, path=environ.get("PATH"))
        if resolved is None:
            raise ToolsCheckError(f"{crate} {version} is not installed at {binary}")
        try:
            result = subprocess.run(
                [resolved, "--version"],
                env=dict(environ),
                capture_output=True,
                text=True,
                check=False,
            )
        except OSError as error:
            raise ToolsCheckError(f"{binary} --version failed: {error}")
        if result.returncode != 0:
            raise ToolsCheckError(f"{binary} --version failed")
        match = _SEMVER_RE.search(result.stdout)
        reported = match.group(0) if match is not None else "<none>"
        if reported != version:
            raise ToolsCheckError(f"{binary} reports {crate} {reported}, {MANIFEST} pins {version}")
        messages.append(f"tools-check: {crate} {version} resolves at {binary}")
    return messages


def _expand_dockerfile_from(image: str, global_args: Mapping[str, str]) -> str:
    def _replace(match: re.Match[str]) -> str:
        braced_key, fallback, bare_key = match.group(1), match.group(2), match.group(3)
        key = braced_key or bare_key
        assert key is not None
        value = global_args.get(key)
        if value:
            return value
        if fallback is not None:
            return fallback
        return match.group(0)

    return re.sub(
        r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}|\$([A-Za-z_][A-Za-z0-9_]*)",
        _replace,
        image,
    )


def check_dockerfile_and_toolchain(repo: Path, pins: Mapping[str, str]) -> str:
    dockerfile_path = repo / DOCKERFILE
    toolchain_path = repo / TOOLCHAIN
    if not dockerfile_path.is_file():
        raise ToolsCheckError(f"missing {DOCKERFILE}")
    if not toolchain_path.is_file():
        raise ToolsCheckError(f"missing {TOOLCHAIN}")

    dockerfile_text = dockerfile_path.read_text(encoding="utf-8")
    for name in _DOCKERFILE_TOOL_ARGS:
        pinned = pins.get(name)
        if not pinned:
            raise ToolsCheckError(f"{name} is missing in {MANIFEST}")
        defaults = re.findall(rf"(?m)^ARG {re.escape(name)}=(.+)$", dockerfile_text)
        if not defaults:
            raise ToolsCheckError(f"{DOCKERFILE} declares no ARG {name} default")
        if len(defaults) != 1 or defaults[0] != pinned:
            raise ToolsCheckError(f"{DOCKERFILE} defaults {name} to {defaults[0]}, {MANIFEST} pins {pinned}")

    toolchain_data = tomllib.loads(toolchain_path.read_text(encoding="utf-8"))
    channel = toolchain_data.get("toolchain", {}).get("channel")
    if not isinstance(channel, str) or not channel:
        raise ToolsCheckError(f"{TOOLCHAIN} declares no channel")

    stripped = re.sub(r"(?m)^[ \t]*#.*(?:\r?\n|$)", "", dockerfile_text)
    logical_lines = re.sub(r"\\[ \t]*\r?\n", " ", stripped).splitlines()
    global_args: dict[str, str] = {}
    seen_from = False
    stages: set[str] = set()
    builder_seen = False
    for line in logical_lines:
        if not line.strip():
            continue
        try:
            parts = shlex.split(line)
        except ValueError as error:
            raise ToolsCheckError(f"{DOCKERFILE} has malformed instruction {line.strip()!r}: {error}")
        op = parts[0].upper()
        if op == "ARG" and not seen_from:
            for item in parts[1:]:
                if "=" in item:
                    key, val = item.split("=", 1)
                    global_args[key] = val
            continue
        if op != "FROM":
            continue
        seen_from = True
        index = 1
        while index < len(parts) and parts[index].startswith("--"):
            index += 1
        if index >= len(parts):
            raise ToolsCheckError(f"{DOCKERFILE} has incomplete FROM instruction: {line.strip()}")
        image = _expand_dockerfile_from(parts[index], global_args)
        alias = parts[-1] if len(parts) >= index + 3 and parts[-2].upper() == "AS" else None
        if image != "scratch" and image not in stages:
            if not _IMAGE_DIGEST_RE.fullmatch(image):
                raise ToolsCheckError(f"{DOCKERFILE} FROM {image} is not pinned by digest")
            if image.startswith("rust:"):
                builder_seen = True
                version = image.removeprefix("rust:").split("@", 1)[0].split("-", 1)[0]
                if version != channel:
                    raise ToolsCheckError(
                        f"{DOCKERFILE} builds with rust {version}, {TOOLCHAIN} pins {channel}"
                    )
        if alias:
            stages.add(alias)

    if not builder_seen:
        raise ToolsCheckError(f"{DOCKERFILE} has no rust:<version> builder stage")
    return f"tools-check: {DOCKERFILE} defaults and base images agree with {MANIFEST} and {TOOLCHAIN}"


def check_compose_digests(repo: Path) -> None:
    compose_path = repo / COMPOSE_FILE
    if not compose_path.is_file():
        return
    images: list[str] = []
    for line_no, raw_line in enumerate(compose_path.read_text(encoding="utf-8").splitlines(), 1):
        stripped = raw_line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        match = re.match(r"^\s*image:\s*(.+?)\s*$", raw_line)
        if match is None:
            continue
        value = re.sub(r"\s+#.*$", "", match.group(1)).strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in ("'", '"'):
            value = value[1:-1]
        if not _IMAGE_DIGEST_RE.fullmatch(value):
            raise ToolsCheckError(f"{COMPOSE_FILE}:{line_no} image {value} is not pinned by digest")
        images.append(value)
    if not images:
        raise ToolsCheckError(f"{COMPOSE_FILE} declares no service images")


def check_cooldown_agreement(repo: Path) -> str:
    cargo_cfg_path = repo / ".cargo/config.toml"
    if not cargo_cfg_path.is_file():
        raise ToolsCheckError("missing .cargo/config.toml")
    cargo_cfg = tomllib.loads(cargo_cfg_path.read_text(encoding="utf-8"))
    cargo_age = cargo_cfg.get("registry", {}).get("global-min-publish-age")
    if cargo_age != "7 days":
        raise ToolsCheckError(
            f".cargo/config.toml registry.global-min-publish-age is {cargo_age!r}, expected '7 days'"
        )

    npx_wrapper = repo / "scripts/checks/npx.py"
    if not npx_wrapper.is_file():
        raise ToolsCheckError("missing scripts/checks/npx.py")
    npx_matches = re.findall(
        r"(?m)^DEFAULT_COOLDOWN_DAYS\s*=\s*([0-9]+)\s*$",
        npx_wrapper.read_text(encoding="utf-8"),
    )
    if npx_matches != ["7"]:
        raise ToolsCheckError(
            f"scripts/checks/npx.py DEFAULT_COOLDOWN_DAYS is {npx_matches!r}, expected ['7']"
        )

    drift_script = repo / "scripts/checks/dependency_drift.sh"
    if not drift_script.is_file():
        raise ToolsCheckError("missing scripts/checks/dependency_drift.sh")
    drift_rel = drift_script.relative_to(repo).as_posix()
    drift_matches = re.findall(r"(?m)^cooldown_days=([0-9]+)$", drift_script.read_text(encoding="utf-8"))
    if drift_matches != ["7"]:
        raise ToolsCheckError(f"{drift_rel} cooldown_days is {drift_matches!r}, expected ['7']")

    if (repo / ".railway/package.json").is_file():
        railway_npmrc = repo / ".railway/.npmrc"
        if not railway_npmrc.is_file():
            raise ToolsCheckError("missing .railway/.npmrc alongside .railway/package.json")
        npmrc_kv: dict[str, list[str]] = {}
        for raw_line in railway_npmrc.read_text(encoding="utf-8").splitlines():
            stripped = raw_line.strip()
            if not stripped or stripped.startswith(("#", ";")):
                continue
            if "=" not in stripped:
                raise ToolsCheckError(f".railway/.npmrc has non-assignment line: {stripped!r}")
            key, value = (part.strip() for part in stripped.split("=", 1))
            npmrc_kv.setdefault(key, []).append(value)
        if npmrc_kv.get("ignore-scripts") != ["true"]:
            raise ToolsCheckError(
                f".railway/.npmrc ignore-scripts is {npmrc_kv.get('ignore-scripts')!r}, expected ['true']"
            )
        if npmrc_kv.get("min-release-age") != ["7"]:
            raise ToolsCheckError(
                f".railway/.npmrc min-release-age is {npmrc_kv.get('min-release-age')!r}, expected ['7']"
            )

    codegen_manifest = repo / "tools/grpc-codegen/Cargo.toml"
    root_manifest = repo / "Cargo.toml"
    if codegen_manifest.is_file() and root_manifest.is_file():
        ws_deps = (
            tomllib.loads(root_manifest.read_text(encoding="utf-8"))
            .get("workspace", {})
            .get("dependencies", {})
        )
        gen_deps = tomllib.loads(codegen_manifest.read_text(encoding="utf-8")).get("dependencies", {})
        for gen_name, ws_name in (
            ("prost", "prost"),
            ("prost-types", "prost-types"),
            ("tonic-prost-build", "tonic-prost"),
        ):
            gen_entry = gen_deps.get(gen_name)
            ws_entry = ws_deps.get(ws_name)
            gen_ver = gen_entry.get("version") if isinstance(gen_entry, dict) else gen_entry
            ws_ver = ws_entry.get("version") if isinstance(ws_entry, dict) else ws_entry
            if ws_ver is not None and gen_ver != ws_ver:
                raise ToolsCheckError(
                    f"tools/grpc-codegen/Cargo.toml pins {gen_name} to {gen_ver!r}, "
                    f"Cargo.toml pins {ws_name} to {ws_ver!r}"
                )

    dependabot = repo / ".github/dependabot.yml"
    if dependabot.is_file():
        ecosystems = 0
        current_cooldowns: list[str] = []
        in_updates = False
        in_cooldown = False
        for raw_line in dependabot.read_text(encoding="utf-8").splitlines():
            stripped = raw_line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            if not raw_line.startswith(" "):
                in_updates = stripped == "updates:"
                in_cooldown = False
                continue
            if not in_updates:
                continue
            if re.match(r"^  -\s+package-ecosystem:", raw_line):
                if ecosystems > 0 and current_cooldowns != ["7"]:
                    raise ToolsCheckError(
                        f".github/dependabot.yml update entry #{ecosystems} has "
                        f"cooldown.default-days {current_cooldowns!r}, expected ['7']"
                    )
                ecosystems += 1
                current_cooldowns = []
                in_cooldown = False
            elif re.match(r"^    [A-Za-z0-9_-]+:", raw_line):
                in_cooldown = stripped == "cooldown:"
            elif in_cooldown:
                match = re.match(r"^      default-days:\s*(\S+)\s*$", raw_line)
                if match:
                    current_cooldowns.append(match.group(1).strip("\"'"))
        if ecosystems == 0:
            raise ToolsCheckError(".github/dependabot.yml declares no package-ecosystem entries")
        if current_cooldowns != ["7"]:
            raise ToolsCheckError(
                f".github/dependabot.yml update entry #{ecosystems} has "
                f"cooldown.default-days {current_cooldowns!r}, expected ['7']"
            )

    return "tools-check: 7-day publish-age cooldown and generator pins agree across Cargo, npm, dependency-drift, and Dependabot"


def _line_has_raw_npx(line: str, *, is_python: bool) -> bool:
    stripped = line.strip()
    if not stripped or stripped.startswith("#") or "npx" not in line:
        return False
    if is_python:
        code = re.sub(r"\s+#.*$", "", line)
        return any(pattern.search(code) is not None for pattern in _PY_RAW_NPX_PATTERNS)
    uncommented = re.sub(r"(?<!\$)\s+#.*$", "", line)
    for match in _SHELL_RAW_NPX_RE.finditer(uncommented):
        prefix = uncommented[: match.start()]
        if _BENIGN_NPX_PREFIX_RE.search(prefix) is not None:
            continue
        return True
    return False


def check_no_raw_npx(repo: Path) -> None:
    for root_rel in _NPX_SCAN_ROOTS:
        scan_root = repo / root_rel
        if scan_root.is_symlink() or not scan_root.exists():
            continue
        candidates = [scan_root] if scan_root.is_file() else sorted(scan_root.rglob("*"))
        for path in candidates:
            if path.is_symlink() or not path.is_file():
                continue
            rel = path.relative_to(repo).as_posix()
            if rel in _NPX_SCAN_EXCLUDES:
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue
            is_python = path.suffix == ".py"
            for line_no, line in enumerate(text.splitlines(), 1):
                if _line_has_raw_npx(line, is_python=is_python):
                    raise ToolsCheckError(
                        f"{rel}:{line_no} invokes npx directly; route through scripts/checks/npx.py"
                    )


def check(
    repo: Path,
    environ: Mapping[str, str] | None = None,
    *,
    skip_cargo_tools: bool = False,
) -> list[str]:
    env = os.environ if environ is None else environ
    pins = parse_versions_env(repo)
    output: list[str] = []
    output.extend(check_cargo_tools(pins, env, skip_cargo_tools=skip_cargo_tools))
    output.append(check_dockerfile_and_toolchain(repo, pins))
    check_compose_digests(repo)
    output.append(check_cooldown_agreement(repo))
    check_no_raw_npx(repo)
    output.append(f"tools-check: {MANIFEST} passed ({len(pins)} pins)")
    return output


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--skip-cargo-tools", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    try:
        lines = check(args.repo.resolve(), os.environ, skip_cargo_tools=args.skip_cargo_tools)
    except (OSError, tomllib.TOMLDecodeError, ToolsCheckError) as error:
        print(f"tools-check: {error}", file=sys.stderr)
        return 1
    for line in lines:
        print(line)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
