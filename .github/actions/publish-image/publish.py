#!/usr/bin/env python3
"""Prepare, identify, push, and promote verified GHCR runtime image metadata."""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
import tomllib
from collections.abc import Mapping, Sequence
from pathlib import Path

_SHA40_RE = re.compile(r"^[0-9a-f]{40}$")
_DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
_POS_INT_RE = re.compile(r"^[1-9][0-9]*$")
_SEMVER_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")
_DOCKER_TAG_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$")
_SINGLE_MANIFEST_MEDIA_TYPES = frozenset(
    {
        "application/vnd.oci.image.manifest.v1+json",
        "application/vnd.docker.distribution.manifest.v2+json",
    }
)


class PublishError(ValueError):
    pass


def _fail(msg: str) -> PublishError:
    return PublishError(f"publish-image: {msg}")


def resolve_crate_version(repo: Path) -> str:
    manifest = tomllib.loads((repo / "crates/service/Cargo.toml").read_text(encoding="utf-8"))
    version = manifest.get("package", {}).get("version")
    if isinstance(version, dict) and version.get("workspace") is True:
        version = tomllib.loads((repo / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    if not isinstance(version, str) or not version:
        raise _fail("unable to resolve crates/service/Cargo.toml package.version")
    return version


def compute_metadata(
    *,
    mode: str,
    candidate_sha: str,
    repository: str,
    default_branch: str,
    ref_name: str,
    ref: str,
    run_id: str,
    run_attempt: str,
    crate_version: str,
) -> dict[str, str]:
    if not _SHA40_RE.fullmatch(candidate_sha):
        raise _fail("candidate SHA must be 40 lowercase hex characters")
    if "/" not in repository or not all(repository.split("/", 1)) or not default_branch:
        raise _fail("repository and default branch are required")
    if not _POS_INT_RE.fullmatch(run_id) or not _POS_INT_RE.fullmatch(run_attempt):
        raise _fail("run identity must be positive integers")
    if not _SEMVER_RE.fullmatch(crate_version):
        raise _fail("crate version must be a semantic version")

    short_sha = candidate_sha[:12]
    image_repo = f"ghcr.io/{repository.lower()}"
    if mode == "main":
        if ref_name != default_branch or ref != f"refs/heads/{default_branch}":
            raise _fail("main publication requires default branch ref")
        promote_tags = f"sha-{short_sha} main"
        artifact_name = f"sbom-main-{short_sha}-{run_id}-{run_attempt}"
    elif mode == "release":
        if not ref.startswith("refs/tags/v") or not _DOCKER_TAG_RE.fullmatch(ref_name) or ref != f"refs/tags/{ref_name}":
            raise _fail("release ref must be a Docker-compatible v* tag")
        if ref_name != f"v{crate_version}":
            raise _fail(f"release tag {ref_name} does not match crate version {crate_version}")
        promote_tags = f"{ref_name} latest"
        artifact_name = f"sbom-{ref_name}-{run_id}-{run_attempt}"
    else:
        raise _fail(f"unsupported mode {mode!r}")

    return {
        "image_repo": image_repo,
        "primary_image": f"{image_repo}:build-{run_id}-{run_attempt}",
        "version": crate_version,
        "promote_tags": promote_tags,
        "artifact_name": artifact_name,
        "identity": f"https://github.com/{repository}/.github/workflows/cd.yml@{ref}",
    }


def _docker(args: Sequence[str], *, env: Mapping[str, str], binary: bool = False) -> subprocess.CompletedProcess:
    return subprocess.run(["docker", *args], env=dict(env), capture_output=True, text=not binary, check=False)


def _emit(values: Mapping[str, str], env: Mapping[str, str]) -> None:
    text = "".join(f"{k}={v}\n" for k, v in values.items())
    gh_out = env.get("GITHUB_OUTPUT")
    if gh_out:
        with Path(gh_out).open("a", encoding="utf-8") as handle:
            handle.write(text)
    else:
        sys.stdout.write(text)


def identify_image(candidate_image: str, metadata_path: Path, *, env: Mapping[str, str]) -> dict[str, str]:
    inspected = _docker(["image", "inspect", "--format", "{{.Id}}", candidate_image], env=env)
    if inspected.returncode != 0:
        raise _fail(inspected.stderr.strip() or f"failed to inspect {candidate_image}")
    image_id = inspected.stdout.strip()
    meta = json.loads(metadata_path.read_bytes())
    descriptor = meta.get("containerimage.descriptor")
    top = meta.get("containerimage.digest")
    if not isinstance(descriptor, dict) or not isinstance(top, str) or not _DIGEST_RE.fullmatch(top) or descriptor.get("digest") != top:
        raise _fail("missing or inconsistent BuildKit descriptor")
    if image_id == top:
        kind = "descriptor"
    elif descriptor.get("mediaType") in _SINGLE_MANIFEST_MEDIA_TYPES and meta.get("containerimage.config.digest") == image_id:
        kind = "config"
    else:
        raise _fail("loaded image does not match BuildKit export")
    return {"image_id": image_id, "id_kind": kind}


def push_candidate(candidate_image: str, image_id: str, id_kind: str, *, env: Mapping[str, str]) -> str:
    if not _DIGEST_RE.fullmatch(image_id) or id_kind not in ("descriptor", "config"):
        raise _fail("expected identified build output")
    inspected = _docker(["image", "inspect", "--format", "{{.Id}}", candidate_image], env=env)
    if inspected.returncode != 0 or inspected.stdout.strip() != image_id:
        raise _fail("run tag moved before push")

    pushed = _docker(["push", candidate_image], env=env)
    sys.stderr.write((pushed.stdout or "") + (pushed.stderr or ""))
    if pushed.returncode != 0:
        raise _fail(f"push failed for {candidate_image}")

    expected_tag = f"{candidate_image.rsplit(':', 1)[-1]}:"
    digests = [
        f[2]
        for line in pushed.stdout.splitlines()
        if len(f := line.split()) == 5 and f[0] == expected_tag and f[1] == "digest:" and f[3] == "size:" and f[4].isdigit()
    ]
    if len(digests) != 1 or not _DIGEST_RE.fullmatch(digests[0]):
        raise _fail(f"push did not confirm exactly one digest for {candidate_image}")
    digest = digests[0]

    readback = _docker(["buildx", "imagetools", "inspect", "--raw", candidate_image], env=env, binary=True)
    if readback.returncode != 0:
        raise _fail("failed to read back pushed manifest")
    raw_bytes: bytes = readback.stdout
    if "sha256:" + hashlib.sha256(raw_bytes).hexdigest() != digest:
        raise _fail("registry run tag does not match push digest")
    if id_kind == "descriptor":
        if image_id != digest:
            raise _fail("pushed descriptor differs from tested descriptor")
    else:
        manifest = json.loads(raw_bytes)
        if manifest.get("config", {}).get("digest") != image_id:
            raise _fail("registry manifest config does not match tested image ID")
    return digest


def promote_tags(image_repo: str, image_id: str, digest: str, tags_spec: str, summary_path: Path, *, env: Mapping[str, str]) -> None:
    if not _DIGEST_RE.fullmatch(image_id) or not _DIGEST_RE.fullmatch(digest):
        raise _fail("promotion requires immutable image ID and verified digest")
    tags = tags_spec.split()
    if not tags or any(not _DOCKER_TAG_RE.fullmatch(t) for t in tags):
        raise _fail("invalid promotion tags")

    promoted: list[str] = []
    for tag in tags:
        target = f"{image_repo}:{tag}"
        tagged = _docker(["tag", image_id, target], env=env)
        pushed = _docker(["push", target], env=env) if tagged.returncode == 0 else None
        confirmed = _docker(["buildx", "imagetools", "inspect", "--format", "{{.Manifest.Digest}}", target], env=env) if (pushed and pushed.returncode == 0) else None
        if not confirmed or confirmed.returncode != 0 or confirmed.stdout.strip() != digest:
            summary_path.parent.mkdir(parents=True, exist_ok=True)
            with summary_path.open("a", encoding="utf-8") as handle:
                handle.write(f"### Partial image promotion\n\nDigest: {image_repo}@{digest}\nPromoted: {' '.join(promoted) or 'none'}\nFailed: {tag}\n")
            raise _fail(f"failed to promote and confirm {target}")
        promoted.append(tag)
        summary_path.parent.mkdir(parents=True, exist_ok=True)
        with summary_path.open("a", encoding="utf-8") as handle:
            handle.write(f"Confirmed tag: {target} at {digest}\n")


def main(argv: Sequence[str] | None = None, *, environ: Mapping[str, str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    env = os.environ if environ is None else environ
    if len(args) != 1 or args[0] not in ("prepare", "identify", "push", "promote"):
        print("usage: publish.py prepare | identify | push | promote", file=sys.stderr)
        return 2
    cmd = args[0]
    try:
        if cmd == "prepare":
            repo = Path(env.get("PUBLISH_REPO_ROOT", ".")).resolve()
            meta = compute_metadata(
                mode=env.get("MODE", ""),
                candidate_sha=env.get("CANDIDATE_SHA", ""),
                repository=env.get("GITHUB_REPOSITORY", ""),
                default_branch=env.get("DEFAULT_BRANCH", ""),
                ref_name=env.get("GITHUB_REF_NAME", ""),
                ref=env.get("GITHUB_REF", ""),
                run_id=env.get("GITHUB_RUN_ID", ""),
                run_attempt=env.get("GITHUB_RUN_ATTEMPT", ""),
                crate_version=env.get("CRATE_VERSION", "").strip() or resolve_crate_version(repo),
            )
            _emit(meta, env)
        elif cmd == "identify":
            _emit(identify_image(env["PRIMARY_IMAGE"], Path(env["RUNTIME_IMAGE_METADATA_FILE"]), env=env), env)
        elif cmd == "push":
            digest = push_candidate(env["PRIMARY_IMAGE"], env["IMAGE_ID"], env["ID_KIND"], env=env)
            _emit({"digest": digest}, env)
        elif cmd == "promote":
            promote_tags(env["IMAGE_REPO"], env["IMAGE_ID"], env["DIGEST"], env["PROMOTE_TAGS"], Path(env["GITHUB_STEP_SUMMARY"]), env=env)
        return 0
    except (PublishError, KeyError, OSError, ValueError) as exc:
        print(str(exc), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
