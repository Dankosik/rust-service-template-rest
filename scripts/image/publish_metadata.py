#!/usr/bin/env python3
"""Prepare, identify, push, and promote verified runtime image publication metadata."""

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

sys.dont_write_bytecode = True

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


class PublishMetadataError(ValueError):
    pass


def _fail(message: str) -> PublishMetadataError:
    return PublishMetadataError(f"publish-image: {message}")


def resolve_crate_version(repo: Path) -> str:
    service_manifest = repo / "crates/service/Cargo.toml"
    if not service_manifest.is_file():
        raise _fail("missing crates/service/Cargo.toml")
    with service_manifest.open("rb") as handle:
        version = tomllib.load(handle).get("package", {}).get("version")
    if isinstance(version, dict) and version.get("workspace") is True:
        workspace_manifest = repo / "Cargo.toml"
        if not workspace_manifest.is_file():
            raise _fail("missing Cargo.toml for workspace package.version")
        with workspace_manifest.open("rb") as handle:
            version = tomllib.load(handle).get("workspace", {}).get("package", {}).get("version")
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
            raise _fail("main publication requires the default branch ref")
        promote_tags = f"sha-{short_sha} main"
        artifact_name = f"sbom-main-{short_sha}-{run_id}-{run_attempt}"
    elif mode == "release":
        if (
            not ref.startswith("refs/tags/v")
            or not _DOCKER_TAG_RE.fullmatch(ref_name)
            or ref != f"refs/tags/{ref_name}"
        ):
            raise _fail("release ref must be a Docker-compatible v* tag")
        if ref_name != f"v{crate_version}":
            raise _fail(f"release tag {ref_name} does not match the crate version {crate_version}")
        promote_tags = f"{ref_name} latest"
        artifact_name = f"sbom-{ref_name}-{run_id}-{run_attempt}"
    else:
        raise _fail(f"unsupported mode '{mode}'")

    return {
        "image_repo": image_repo,
        "primary_image": f"{image_repo}:build-{run_id}-{run_attempt}",
        "version": crate_version,
        "promote_tags": promote_tags,
        "artifact_name": artifact_name,
        "identity": f"https://github.com/{repository}/.github/workflows/cd.yml@{ref}",
    }


def format_kv_lines(values: Mapping[str, str]) -> str:
    return "".join(f"{key}={value}\n" for key, value in values.items())


def _write_output(text: str, *, github_output: str | None, prefer_github_output: bool) -> None:
    if prefer_github_output and github_output:
        out_path = Path(github_output)
        same_as_stdout = False
        try:
            if out_path.exists():
                stdout_stat = os.fstat(sys.stdout.fileno())
                file_stat = out_path.stat()
                same_as_stdout = (
                    stdout_stat.st_dev == file_stat.st_dev
                    and stdout_stat.st_ino == file_stat.st_ino
                )
        except OSError:
            same_as_stdout = False
        if not same_as_stdout:
            with out_path.open("a", encoding="utf-8") as handle:
                handle.write(text)
            return
    sys.stdout.write(text)


def _run_docker(args: Sequence[str], *, env: Mapping[str, str], binary_output: bool = False) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(
            ["docker", *args],
            env=dict(env),
            capture_output=True,
            text=not binary_output,
            check=False,
        )
    except OSError as error:
        raise _fail(f"cannot execute docker: {error}")


def identify_image(
    candidate_image: str,
    export_metadata_path: Path,
    *,
    env: Mapping[str, str],
) -> dict[str, str]:
    inspected = _run_docker(["image", "inspect", "--format", "{{.Id}}", candidate_image], env=env)
    if inspected.returncode != 0:
        detail = inspected.stderr.strip()
        raise _fail(detail or f"failed to inspect loaded image {candidate_image}")
    image_id = inspected.stdout.strip()
    try:
        metadata = json.loads(export_metadata_path.read_bytes())
    except (OSError, ValueError) as error:
        raise _fail(f"unable to read BuildKit export metadata: {error}")
    if not isinstance(metadata, dict):
        raise _fail("missing or inconsistent BuildKit descriptor")
    descriptor = metadata.get("containerimage.descriptor")
    top = metadata.get("containerimage.digest")
    if (
        not isinstance(descriptor, dict)
        or not isinstance(top, str)
        or not _DIGEST_RE.fullmatch(top)
        or descriptor.get("digest") != top
    ):
        raise _fail("missing or inconsistent BuildKit descriptor")
    if image_id == top:
        kind = "descriptor"
    elif (
        descriptor.get("mediaType") in _SINGLE_MANIFEST_MEDIA_TYPES
        and _DIGEST_RE.fullmatch(image_id)
        and metadata.get("containerimage.config.digest") == image_id
    ):
        kind = "config"
    else:
        raise _fail("loaded image does not match the final BuildKit export")
    return {"image_id": image_id, "id_kind": kind}


def push_candidate(
    candidate_image: str,
    image_id: str,
    id_kind: str,
    *,
    env: Mapping[str, str],
) -> str:
    if not _DIGEST_RE.fullmatch(image_id) or id_kind not in ("descriptor", "config"):
        raise _fail("expected an identified build output")
    inspected = _run_docker(["image", "inspect", "--format", "{{.Id}}", candidate_image], env=env)
    if inspected.returncode != 0:
        raise _fail(inspected.stderr.strip() or f"failed to inspect {candidate_image}")
    if inspected.stdout.strip() != image_id:
        raise _fail("run tag moved before push")

    pushed = _run_docker(["push", candidate_image], env=env)
    if pushed.stdout:
        sys.stderr.write(pushed.stdout)
    if pushed.stderr:
        sys.stderr.write(pushed.stderr)
    if pushed.returncode != 0:
        raise _fail(f"push did not confirm exactly one digest for {candidate_image}")

    expected_tag = f"{candidate_image.rsplit(':', 1)[-1]}:"
    digests: list[str] = []
    invalid_digest_line = False
    for raw_line in pushed.stdout.splitlines():
        line = raw_line.rstrip("\r")
        fields = line.split()
        if len(fields) >= 2 and fields[1] == "digest:":
            if len(fields) != 5 or fields[0] != expected_tag or fields[3] != "size:" or not fields[4].isdigit():
                invalid_digest_line = True
            else:
                digests.append(fields[2])
    if invalid_digest_line or len(digests) != 1:
        raise _fail(f"push did not confirm exactly one digest for {candidate_image}")
    digest = digests[0]
    if not _DIGEST_RE.fullmatch(digest):
        raise _fail("invalid push digest")

    # Buildx imagetools Printer.Print(raw) writes the manifest verbatim.
    # Preserve exact registry bytes when checking the push receipt.
    readback = _run_docker(
        ["buildx", "imagetools", "inspect", "--raw", candidate_image],
        env=env,
        binary_output=True,
    )
    if readback.returncode != 0:
        raise _fail("failed to read back pushed manifest from registry")
    raw_bytes: bytes = readback.stdout
    if "sha256:" + hashlib.sha256(raw_bytes).hexdigest() != digest:
        raise _fail("registry run tag does not match the push digest")
    if id_kind == "descriptor":
        if image_id != digest:
            raise _fail("pushed descriptor differs from the tested descriptor")
    else:
        try:
            manifest = json.loads(raw_bytes)
        except ValueError:
            raise _fail("registry single manifest is not valid JSON")
        if (
            not isinstance(manifest, dict)
            or manifest.get("schemaVersion") != 2
            or "manifests" in manifest
            or manifest.get("mediaType") not in (None, *_SINGLE_MANIFEST_MEDIA_TYPES)
            or not isinstance(manifest.get("layers"), list)
            or not isinstance(manifest.get("config"), dict)
            or manifest["config"].get("digest") != image_id
        ):
            raise _fail("registry single manifest does not identify the tested config")
    return digest


def promote_tags(
    image_repo: str,
    image_id: str,
    digest: str,
    tags_spec: str,
    summary_path: Path,
    *,
    env: Mapping[str, str],
) -> None:
    if not _DIGEST_RE.fullmatch(image_id) or not _DIGEST_RE.fullmatch(digest):
        raise _fail("promotion requires an immutable image ID and verified digest")
    tags = tags_spec.split()
    if not tags:
        raise _fail("at least one promotion tag is required")
    if any(not _DOCKER_TAG_RE.fullmatch(tag) for tag in tags):
        raise _fail("promotion tags must be valid Docker tags")

    promoted: list[str] = []
    for tag in tags:
        target = f"{image_repo}:{tag}"
        tagged = _run_docker(["tag", image_id, target], env=env)
        pushed = _run_docker(["push", target], env=env) if tagged.returncode == 0 else None
        confirmed = (
            _run_docker(
                ["buildx", "imagetools", "inspect", "--format", "{{.Manifest.Digest}}", target],
                env=env,
            )
            if pushed is not None and pushed.returncode == 0
            else None
        )
        if (
            tagged.returncode != 0
            or pushed is None
            or pushed.returncode != 0
            or confirmed is None
            or confirmed.returncode != 0
            or confirmed.stdout.strip() != digest
        ):
            partial = (
                "### Partial image promotion\n\n"
                f"Digest: {image_repo}@{digest}\n"
                f"Promoted tags before failure: {' '.join(promoted) if promoted else 'none'}\n"
                f"Failed tag: {tag}\n"
            )
            try:
                summary_path.parent.mkdir(parents=True, exist_ok=True)
                with summary_path.open("a", encoding="utf-8") as handle:
                    handle.write(partial)
            except OSError:
                pass
            raise _fail(f"failed to promote and confirm {target}")
        promoted.append(tag)
        summary_path.parent.mkdir(parents=True, exist_ok=True)
        with summary_path.open("a", encoding="utf-8") as handle:
            handle.write(f"Confirmed tag: {image_repo}:{tag} at {digest}\n")


def _usage() -> int:
    print(
        "usage: publish_metadata.py "
        "prepare [<mode> <candidate-sha> <repository> <default-branch> <ref-name> <ref> <run-id> <run-attempt> <crate-version>] | "
        "identify [<candidate-image> <export-metadata>] | "
        "push [<candidate-image> <image-id> <id-kind>] | "
        "promote-summary [<image-repo> <image-id> <digest> <tags> <summary>]",
        file=sys.stderr,
    )
    return 2


def main(argv: Sequence[str] | None = None, *, environ: Mapping[str, str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    env = os.environ if environ is None else environ
    if not args:
        return _usage()

    subcommand, rest = args[0], args[1:]
    try:
        if subcommand == "prepare":
            if len(rest) == 9:
                values = compute_metadata(
                    mode=rest[0],
                    candidate_sha=rest[1],
                    repository=rest[2],
                    default_branch=rest[3],
                    ref_name=rest[4],
                    ref=rest[5],
                    run_id=rest[6],
                    run_attempt=rest[7],
                    crate_version=rest[8],
                )
                _write_output(format_kv_lines(values), github_output=None, prefer_github_output=False)
                return 0
            if len(rest) == 0:
                repo = Path(env.get("PUBLISH_REPO_ROOT", ".")).resolve()
                crate_version = env.get("CRATE_VERSION", "").strip() or resolve_crate_version(repo)
                values = compute_metadata(
                    mode=env.get("MODE", ""),
                    candidate_sha=env.get("CANDIDATE_SHA", ""),
                    repository=env.get("GITHUB_REPOSITORY", ""),
                    default_branch=env.get("DEFAULT_BRANCH", ""),
                    ref_name=env.get("GITHUB_REF_NAME", ""),
                    ref=env.get("GITHUB_REF", ""),
                    run_id=env.get("GITHUB_RUN_ID", ""),
                    run_attempt=env.get("GITHUB_RUN_ATTEMPT", ""),
                    crate_version=crate_version,
                )
                _write_output(format_kv_lines(values), github_output=env.get("GITHUB_OUTPUT"), prefer_github_output=True)
                return 0
            return _usage()

        if subcommand == "identify":
            if len(rest) == 2:
                values = identify_image(rest[0], Path(rest[1]), env=env)
                _write_output(format_kv_lines(values), github_output=None, prefer_github_output=False)
                return 0
            if len(rest) == 0:
                primary_image = env.get("PRIMARY_IMAGE", "")
                metadata_file = env.get("RUNTIME_IMAGE_METADATA_FILE", "")
                if not primary_image or not metadata_file:
                    raise _fail("PRIMARY_IMAGE and RUNTIME_IMAGE_METADATA_FILE are required")
                values = identify_image(primary_image, Path(metadata_file), env=env)
                _write_output(
                    format_kv_lines(values),
                    github_output=env.get("GITHUB_OUTPUT"),
                    prefer_github_output=True,
                )
                return 0
            return _usage()

        if subcommand == "push":
            if len(rest) == 3:
                digest = push_candidate(rest[0], rest[1], rest[2], env=env)
                sys.stdout.write(f"{digest}\n")
                return 0
            if len(rest) == 0:
                primary_image = env.get("PRIMARY_IMAGE", "")
                image_id = env.get("IMAGE_ID", "")
                id_kind = env.get("ID_KIND", "")
                if not primary_image or not image_id or not id_kind:
                    raise _fail("PRIMARY_IMAGE, IMAGE_ID, and ID_KIND are required")
                digest = push_candidate(primary_image, image_id, id_kind, env=env)
                _write_output(
                    f"digest={digest}\n",
                    github_output=env.get("GITHUB_OUTPUT"),
                    prefer_github_output=True,
                )
                return 0
            return _usage()

        if subcommand == "promote-summary":
            if len(rest) == 5:
                promote_tags(rest[0], rest[1], rest[2], rest[3], Path(rest[4]), env=env)
                return 0
            if len(rest) == 0:
                image_repo = env.get("IMAGE_REPO", "")
                image_id = env.get("IMAGE_ID", "")
                digest = env.get("DIGEST", "")
                tags_spec = env.get("PROMOTE_TAGS", "")
                summary = env.get("GITHUB_STEP_SUMMARY", "")
                if not image_repo or not summary:
                    raise _fail("IMAGE_REPO and GITHUB_STEP_SUMMARY are required")
                promote_tags(image_repo, image_id, digest, tags_spec, Path(summary), env=env)
                return 0
            return _usage()

        return _usage()
    except PublishMetadataError as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
