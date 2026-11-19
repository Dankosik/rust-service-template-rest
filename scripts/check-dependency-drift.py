#!/usr/bin/env python3
"""Check vendored [patch.crates-io] integrity, OSV advisories, crates.io yanks, and optional --upstream drift."""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tomllib
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
COOLDOWN_DAYS = 7
USER_AGENT = "rust-service-template dependency-drift"
VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+([.+_-][0-9A-Za-z.+_-]+)?$")
ISO_UTC_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.\d+)?Z$")

UPSTREAM_PINS: tuple[tuple[str, str, str, str], ...] = (
    ("CARGO_DENY_VERSION", "crates", "cargo-deny", ""),
    ("CARGO_NEXTEST_VERSION", "crates", "cargo-nextest", ""),
    ("CARGO_SHEAR_VERSION", "crates", "cargo-shear", ""),
    ("TYPOS_VERSION", "crates", "typos-cli", ""),
    ("ZIZMOR_VERSION", "crates", "zizmor", ""),
    ("CARGO_MUTANTS_VERSION", "crates", "cargo-mutants", ""),
    ("SQLX_CLI_VERSION", "crates", "sqlx-cli", ""),
    ("CARGO_CHEF_VERSION", "crates", "cargo-chef", ""),
    ("CARGO_AUDITABLE_VERSION", "crates", "cargo-auditable", ""),
    ("CARGO_ABOUT_VERSION", "crates", "cargo-about", ""),
    ("REDOCLY_CLI_VERSION", "npm", "@redocly/cli", ""),
    ("SQUAWK_CLI_VERSION", "npm", "squawk-cli", ""),
    ("RUFF_VERSION", "pypi", "ruff", ""),
    ("GO_VERSION", "go-toolchain", "golang.org/toolchain", ""),
    ("OASDIFF_VERSION", "go", "github.com/oasdiff/oasdiff", ""),
    ("GITLEAKS_VERSION", "go", "github.com/zricethezav/gitleaks/v8", ""),
    ("ACTIONLINT_VERSION", "go", "github.com/rhysd/actionlint", ""),
    ("BUF_VERSION", "go", "github.com/bufbuild/buf", ""),
    ("SHELLCHECK_IMAGE", "github", "koalaman/shellcheck", ""),
    ("TRIVY_IMAGE", "github", "aquasecurity/trivy", ""),
    ("LYCHEE_IMAGE", "github", "lycheeverse/lychee", ""),
    ("KEYCLOAK_IMAGE", "github", "keycloak/keycloak", ""),
    ("COSIGN_VERSION", "github", "sigstore/cosign", ""),
    ("BUILDKIT_IMAGE", "github", "moby/buildkit", ""),
    ("BUILDX_VERSION", "github", "docker/buildx", ""),
    ("PROMTOOL_VERSION", "github", "prometheus/prometheus", "3.5."),
)


class DriftError(RuntimeError):
    pass


def fetch_text(url: str, *, payload: bytes | None = None) -> str:
    headers = {"User-Agent": USER_AGENT}
    if url.startswith("https://api.github.com/") and os.environ.get("GH_TOKEN"):
        headers["Authorization"] = f"Bearer {os.environ['GH_TOKEN']}"
    if payload is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=payload, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.read().decode("utf-8")
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        raise DriftError(f"request failed for {url}: {exc}") from exc


def fetch_json(url: str, *, payload: dict[str, Any] | None = None) -> Any:
    raw = json.dumps(payload).encode("utf-8") if payload is not None else None
    try:
        return json.loads(fetch_text(url, payload=raw))
    except json.JSONDecodeError as exc:
        raise DriftError(f"invalid JSON from {url}: {exc}") from exc


def release_age_days(published: str) -> int:
    match = ISO_UTC_RE.match(published)
    if not match:
        raise DriftError(f"invalid release publish timestamp: {published!r}")
    dt = datetime.strptime(match.group(1), "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc)
    return int((datetime.now(timezone.utc) - dt).total_seconds() // 86400)


def crate_versions(name: str) -> list[dict[str, Any]]:
    data = fetch_json(f"https://crates.io/api/v1/crates/{name}")
    versions = data.get("versions")
    if data.get("crate", {}).get("name") != name or not isinstance(versions, list) or not versions:
        raise DriftError(f"invalid crates.io response for {name}")
    return versions


def latest_release(registry: str, package: str, line: str) -> tuple[str, int]:
    if registry == "crates":
        for entry in crate_versions(package):
            num = entry.get("num", "")
            if not entry.get("yanked") and "-" not in num and num.startswith(line):
                return num, release_age_days(entry["created_at"])
        raise DriftError(f"no stable crates.io release for {package}")
    if registry == "npm":
        data = fetch_json(f"https://registry.npmjs.org/{package}")
        ver = data["dist-tags"]["latest"]
        return ver, release_age_days(data["time"][ver])
    if registry == "pypi":
        data = fetch_json(f"https://pypi.org/pypi/{package}/json")
        return data["info"]["version"], release_age_days(data["urls"][0]["upload_time_iso_8601"])
    if registry == "go":
        data = fetch_json(f"https://proxy.golang.org/{package}/@latest")
        return data["Version"].lstrip("v"), release_age_days(data["Time"])
    if registry == "go-toolchain":
        prefix = f"v0.0.1-go{line}"
        candidates = [
            v for v in fetch_text(f"https://proxy.golang.org/{package}/@v/list").splitlines()
            if v.startswith(prefix) and re.fullmatch(r"v0\.0\.1-go[0-9]+\.[0-9]+\.[0-9]+\.linux-amd64", v)
        ]
        if not candidates:
            raise DriftError(f"no stable Go toolchain release for {package}")
        candidates.sort(key=lambda v: tuple(int(x) for x in re.search(r"-go([0-9.]+)\.", v).group(1).split(".")))  # type: ignore[union-attr]
        chosen = candidates[-1]
        data = fetch_json(f"https://proxy.golang.org/{package}/@v/{chosen}.info")
        ver = re.search(r"-go([0-9]+\.[0-9]+\.[0-9]+)\.", data["Version"]).group(1)  # type: ignore[union-attr]
        return ver, release_age_days(data["Time"])
    if registry == "github":
        url = f"https://api.github.com/repos/{package}/releases" + ("/latest" if not line else "?per_page=50")
        raw = fetch_json(url)
        items = [raw] if not line else raw
        for rel in items:
            if rel.get("draft") or rel.get("prerelease"):
                continue
            ver = re.sub(r"^[^0-9]*", "", rel.get("tag_name", ""))
            if ver and ver.startswith(line):
                return ver, release_age_days(rel["published_at"])
        raise DriftError(f"no stable GitHub release for {package}")
    raise DriftError(f"unknown registry: {registry}")


def parse_versions_env(path: Path) -> dict[str, str]:
    pins: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, val = line.split("=", 1)
        pins[key] = val
    return pins


def check_repo(root: Path, *, upstream: bool) -> list[str]:
    findings: list[str] = []
    workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))

    by_name: dict[str, list[dict[str, Any]]] = {}
    for pkg in lock.get("package", []):
        by_name.setdefault(pkg["name"], []).append(pkg)

    if lock.get("patch", {}).get("unused"):
        findings.append("Cargo.lock lists an unused [patch.crates-io] entry")

    codegen_manifest = root / "tools/grpc-codegen/Cargo.toml"
    if codegen_manifest.is_file():
        ws_deps = workspace.get("workspace", {}).get("dependencies", {})
        gen_deps = tomllib.loads(codegen_manifest.read_text(encoding="utf-8")).get("dependencies", {})
        for gen_name, ws_name in (("prost", "prost"), ("prost-types", "prost-types"), ("tonic-prost-build", "tonic-prost")):
            gen_entry, ws_entry = gen_deps.get(gen_name), ws_deps.get(ws_name)
            gen_ver = gen_entry.get("version") if isinstance(gen_entry, dict) else gen_entry
            ws_ver = ws_entry.get("version") if isinstance(ws_entry, dict) else ws_entry
            if ws_ver is not None and gen_ver != ws_ver:
                findings.append(f"tools/grpc-codegen/Cargo.toml pins {gen_name}={gen_ver}, Cargo.toml pins {ws_name}={ws_ver}")

    for key, patch in sorted(workspace.get("patch", {}).get("crates-io", {}).items()):
        rel_path = patch.get("path") if isinstance(patch, dict) else None
        if not isinstance(rel_path, str) or not rel_path:
            raise DriftError(f"[patch.crates-io] {key} must specify a local path")
        directory = (root / rel_path).resolve()
        if not directory.is_relative_to(root.resolve()):
            raise DriftError(f"[patch.crates-io] {key} points outside repository: {rel_path}")
        manifest, patches = directory / "Cargo.toml", directory / "PATCHES.md"
        for req_file in (manifest, patches):
            if not req_file.is_file():
                raise DriftError(f"[patch.crates-io] {key} requires {req_file.relative_to(root)}")
        pkg_meta = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
        name, version = pkg_meta.get("name", ""), pkg_meta.get("version", "")
        if name != patch.get("package", key) or not VERSION_RE.fullmatch(version):
            raise DriftError(f"invalid patched package metadata in {manifest.relative_to(root)}")

        locked = by_name.get(name, [])
        if not any(p["version"] == version and "source" not in p for p in locked):
            findings.append(f"{name}: Cargo.lock does not resolve vendored version {version}")
        if any("source" in p for p in locked):
            findings.append(f"{name}: Cargo.lock also resolves another source, bypassing the patch")

        reviewed_match = re.search(r"^Upstream reviewed:\s+([0-9][^\s]*)$", patches.read_text(encoding="utf-8"), re.M)
        reviewed = reviewed_match.group(1) if reviewed_match else ""
        if not reviewed:
            findings.append(f"{name}: {patches.relative_to(root)} has no `Upstream reviewed: <version>` line")

        osv = fetch_json(
            "https://api.osv.dev/v1/query",
            payload={"package": {"name": name, "ecosystem": "crates.io"}, "version": version},
        )
        vulns = osv.get("vulns", [])
        if vulns:
            findings.append(f"{name} {version}: fork base affected by {', '.join(v['id'] for v in vulns)}")

        versions = crate_versions(name)
        matches = [v for v in versions if v.get("num") == version]
        if not matches:
            findings.append(f"{name} {version}: fork base version is absent on crates.io")
        elif matches[0].get("yanked"):
            findings.append(f"{name} {version}: fork base is yanked on crates.io")

        if upstream:
            newest, age = latest_release("crates", name, "")
            if newest != version and newest != reviewed and age >= COOLDOWN_DAYS:
                findings.append(
                    f"{name}: upstream {newest} ({age} days old) is unreviewed; fork base {version}, reviewed {reviewed or 'none'}"
                )

    if upstream:
        pins = parse_versions_env(root / "tools/versions.env")
        for pin, registry, package, line in UPSTREAM_PINS:
            value = pins.get(pin)
            if not value:
                continue
            current = value.split(":", 1)[-1].split("@", 1)[0].lstrip("v")
            newest, age = latest_release(registry, package, line)
            if newest != current and age >= COOLDOWN_DAYS:
                findings.append(f"tools/versions.env {pin}: {current} -> {newest} ({age} days old)")

    return findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", action="store_true", help="also check upstream crate and tool-pin releases")
    args = parser.parse_args()
    try:
        findings = check_repo(ROOT, upstream=args.upstream)
    except (DriftError, OSError, KeyError, ValueError) as exc:
        print(f"dependency-drift: {exc}", file=sys.stderr)
        return 2
    if findings:
        for item in findings:
            print(f"- {item}")
        return 1
    print("dependency-drift: no finding")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
