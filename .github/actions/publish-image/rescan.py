#!/usr/bin/env python3
"""Reassess published deployable GHCR image digests with pinned Trivy."""

from __future__ import annotations

import json
import os
import re
import secrets
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
_DEPLOYABLE_TAG_RE = re.compile(r"^(?:main|latest|v[A-Za-z0-9_.-]{0,127}|sha-[0-9a-f]{12})$")


def _trivy_image() -> str:
    if value := os.environ.get("TRIVY_IMAGE"):
        return value
    return str(tomllib.loads((ROOT / "mise.toml").read_text(encoding="utf-8"))["env"]["TRIVY_IMAGE"])


def _gh_json(*args: str) -> object:
    res = subprocess.run(["gh", "api", *args], capture_output=True, text=True, check=False)
    if res.returncode != 0:
        raise RuntimeError(res.stderr.strip() or f"gh api {' '.join(args)} failed")
    return json.loads(res.stdout)


def main() -> int:
    repository = os.environ["GITHUB_REPOSITORY"].lower()
    summary = Path(os.environ["GITHUB_STEP_SUMMARY"])
    token = os.environ["GH_TOKEN"]
    actor = os.environ["GITHUB_ACTOR"]
    trivy_image = _trivy_image()

    owner_type = _gh_json(f"repos/{repository}")["owner"]["type"]  # type: ignore[index]
    namespace = {"User": "users", "Organization": "orgs"}.get(owner_type)
    if not namespace:
        print(f"Unsupported repository owner type: {owner_type}", file=sys.stderr)
        return 2

    owner, repo = repository.split("/", 1)
    pages = _gh_json("--paginate", "--slurp", f"{namespace}/{owner}/packages/container/{repo}/versions?per_page=100")
    by_digest: dict[str, set[str]] = {}
    for page in pages:  # type: ignore[union-attr]
        for entry in page:
            tags = [t for t in entry.get("metadata", {}).get("container", {}).get("tags", []) if _DEPLOYABLE_TAG_RE.fullmatch(t)]
            if tags:
                by_digest.setdefault(entry["name"], set()).update(tags)

    with summary.open("a", encoding="utf-8") as handle:
        handle.write(f"### Published image scans for `ghcr.io/{repository}`\n\n")
        if not by_digest:
            msg = "No published digest carries a deployable tag.\n"
            print(msg, end="")
            handle.write(msg)
            return 0
        handle.write("| Digest | Deployable tags | Result |\n| --- | --- | --- |\n")

    trivy_run = [
        "docker", "run", "--rm", "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges",
        "--user", f"{os.getuid()}:{os.getgid()}", "--tmpfs", "/tmp:rw,noexec,nosuid,nodev", "-e", "HOME=/tmp",
    ]
    trivy_args = ["image", "--cache-dir", "/cache", "--disable-telemetry", "--skip-version-check", "--scanners", "vuln"]

    finding_count = error_count = 0
    with tempfile.TemporaryDirectory(prefix="rescan-") as tmp:
        tmp_path = Path(tmp)
        cache_dir = tmp_path / "cache"
        reg_dir = tmp_path / "registry"
        cache_dir.mkdir(mode=0o700)
        reg_dir.mkdir(mode=0o700)

        subprocess.run(
            [*trivy_run, "-i", "-e", "DOCKER_CONFIG=/registry", "-v", f"{reg_dir}:/registry",
             trivy_image, "registry", "login", "--username", actor, "--password-stdin", "ghcr.io"],
            input=f"{token}\n",
            text=True,
            check=True,
        )
        subprocess.run([*trivy_run, "-v", f"{cache_dir}:/cache", trivy_image, *trivy_args, "--download-db-only"], check=True)

        for digest, tag_set in sorted(by_digest.items()):
            tags = ", ".join(sorted(tag_set))
            print(f"\nScanning ghcr.io/{repository}@{digest} ({tags})", flush=True)
            stop_token = secrets.token_hex(16)
            print(f"::stop-commands::{stop_token}", flush=True)
            proc = subprocess.run(
                [
                    *trivy_run, "-e", "DOCKER_CONFIG=/registry",
                    "-v", f"{reg_dir}:/registry:ro", "-v", f"{cache_dir}:/cache:ro",
                    trivy_image, *trivy_args,
                    "--cache-backend", "memory", "--skip-db-update", "--skip-java-db-update", "--offline-scan",
                    "--image-src", "remote", "--platform", "linux/amd64", "--ignorefile", "",
                    "--severity", "HIGH,CRITICAL", "--exit-code", "42",
                    f"ghcr.io/{repository}@{digest}",
                ],
                check=False,
            )
            print(f"::{stop_token}::", flush=True)
            if proc.returncode == 0:
                result = "No HIGH or CRITICAL findings"
            elif proc.returncode == 42:
                result = "HIGH or CRITICAL findings require attention"
                finding_count += 1
            else:
                result = f"Scan failed (exit {proc.returncode})"
                error_count += 1
            with summary.open("a", encoding="utf-8") as handle:
                handle.write(f"| `{digest}` | {tags} | {result} |\n")

    print(f"{finding_count} digests with HIGH or CRITICAL findings. {error_count} failed scans.")
    if error_count:
        return 2
    return 1 if finding_count else 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, RuntimeError, subprocess.CalledProcessError, OSError) as exc:
        print(f"rescan: {exc}", file=sys.stderr)
        raise SystemExit(2)
