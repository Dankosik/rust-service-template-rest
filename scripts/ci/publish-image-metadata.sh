#!/usr/bin/env bash
set -euo pipefail

usage() {
	echo "usage: $0 metadata <mode> <candidate-sha> <repository> <default-branch> <ref-name> <ref> <run-id> <run-attempt> <crate-version> | identify <candidate-image> <export-metadata> | push <candidate-image> <image-id> <id-kind> | promote <image-repo> <image-id> <digest> <tags> <summary> | self-test" >&2
	exit 2
}

identify() {
	local image_id
	image_id=$(docker image inspect --format '{{.Id}}' "$1") || return 1
	python3 - "${image_id}" "$2" <<'PY'
import json
import re
import sys

image_id, path = sys.argv[1:]
with open(path, "rb") as source:
    metadata = json.load(source)
descriptor = metadata["containerimage.descriptor"]
top = metadata["containerimage.digest"]
if not re.fullmatch(r"sha256:[0-9a-f]{64}", top) or descriptor["digest"] != top:
    sys.exit("publish-image: missing or inconsistent BuildKit descriptor")
if image_id == top:
    kind = "descriptor"
elif (descriptor["mediaType"] in ("application/vnd.oci.image.manifest.v1+json",
                                  "application/vnd.docker.distribution.manifest.v2+json")
      and re.fullmatch(r"sha256:[0-9a-f]{64}", image_id)
      and metadata.get("containerimage.config.digest") == image_id):
    kind = "config"
else:
    sys.exit("publish-image: loaded image does not match the final BuildKit export")
print(f"image_id={image_id}")
print(f"id_kind={kind}")
PY
}

push_candidate() {
	local image="$1" image_id="$2" kind="$3" current digest
	[[ ${image_id} =~ ^sha256:[0-9a-f]{64}$ && ( ${kind} == descriptor || ${kind} == config ) ]] || {
		echo "publish-image: expected an identified build output" >&2; return 1
	}
	current=$(docker image inspect --format '{{.Id}}' "${image}") || return 1
	[[ ${current} == "${image_id}" ]] || { echo "publish-image: run tag moved before push" >&2; return 1; }
	digest=$(docker push "${image}" | awk -v tag="${image##*:}:" '
		{ print > "/dev/stderr" }
		$2 == "digest:" {
			sub(/\r$/, "")
			if ($1 != tag || NF != 5 || $4 != "size:" || $5 !~ /^[0-9]+$/) invalid = 1
			digest = $3
			count++
		}
		END { if (count != 1 || invalid) exit 1; print digest }
	') || { echo "publish-image: push did not confirm exactly one digest for ${image}" >&2; return 1; }
	[[ ${digest} =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "publish-image: invalid push digest" >&2; return 1; }
	# Buildx imagetools Printer.Print(raw) writes the manifest verbatim, without a newline.
	# Keep any newline belonging to the registry bytes when checking the push receipt.
	docker buildx imagetools inspect --raw "${image}" | python3 -c '
import hashlib
import json
import sys

digest, image_id, kind = sys.argv[1:]
raw = sys.stdin.buffer.read()
if "sha256:" + hashlib.sha256(raw).hexdigest() != digest:
    sys.exit("publish-image: registry run tag does not match the push digest")
if kind == "descriptor":
    if image_id != digest:
        sys.exit("publish-image: pushed descriptor differs from the tested descriptor")
else:
    manifest = json.loads(raw)
    if (manifest.get("schemaVersion") != 2 or "manifests" in manifest
            or manifest.get("mediaType") not in (None, "application/vnd.oci.image.manifest.v1+json",
                                                 "application/vnd.docker.distribution.manifest.v2+json")
            or not isinstance(manifest.get("layers"), list)
            or manifest.get("config", {}).get("digest") != image_id):
        sys.exit("publish-image: registry single manifest does not identify the tested config")
' "${digest}" "${image_id}" "${kind}" || return 1
	printf '%s\n' "${digest}"
}

# Each promoted tag is re-resolved after its push, so a tag that landed on
# another digest fails the publication instead of being deployed.
promote() {
	local image_repo="$1" image_id="$2" digest="$3" promote_tags="$4" summary="$5" tag confirmed
	local -a tags promoted=()

	[[ ${image_id} =~ ^sha256:[0-9a-f]{64}$ && ${digest} =~ ^sha256:[0-9a-f]{64}$ ]] || {
		echo "publish-image: promotion requires an immutable image ID and verified digest" >&2; return 1
	}
	read -ra tags <<<"${promote_tags}"
	[[ ${#tags[@]} -gt 0 ]] || {
		echo "publish-image: at least one promotion tag is required" >&2
		return 1
	}
	for tag in "${tags[@]}"; do
		if ! docker tag "${image_id}" "${image_repo}:${tag}" || ! docker push "${image_repo}:${tag}" ||
			! confirmed=$(docker buildx imagetools inspect --format '{{.Manifest.Digest}}' "${image_repo}:${tag}") ||
			[[ ${confirmed} != "${digest}" ]]; then
			{
				echo "### Partial image promotion"
				echo
				echo "Digest: ${image_repo}@${digest}"
				echo "Promoted tags before failure: ${promoted[*]:-none}"
				echo "Failed tag: ${tag}"
			} >>"${summary}" || true
			return 1
		fi
		promoted+=("${tag}")
		printf 'Confirmed tag: %s:%s at %s\n' "${image_repo}" "${tag}" "${digest}" >>"${summary}" || return 1
	done
}

metadata() {
	local mode="$1" candidate_sha="$2" repository="$3" default_branch="$4"
	local ref_name="$5" ref="$6" run_id="$7" run_attempt="$8" crate_version="$9"
	local short_sha promote_tags artifact_name identity image_repo

	[[ "${candidate_sha}" =~ ^[0-9a-f]{40}$ ]] || {
		echo "publish-image: candidate SHA must be 40 lowercase hex characters" >&2
		return 1
	}
	[[ "${repository}" == */* && -n "${default_branch}" ]] || {
		echo "publish-image: repository and default branch are required" >&2
		return 1
	}
	[[ "${run_id}" =~ ^[1-9][0-9]*$ && "${run_attempt}" =~ ^[1-9][0-9]*$ ]] || {
		echo "publish-image: run identity must be positive integers" >&2
		return 1
	}
	[[ "${crate_version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || {
		echo "publish-image: crate version must be a semantic version" >&2
		return 1
	}

	short_sha="${candidate_sha:0:12}"
	image_repo="ghcr.io/$(printf '%s' "${repository}" | tr '[:upper:]' '[:lower:]')"
	case "${mode}" in
	main)
		promote_tags="sha-${short_sha} main"
		artifact_name="sbom-main-${short_sha}-${run_id}-${run_attempt}"
		identity="https://github.com/${repository}/.github/workflows/cd.yml@refs/heads/${default_branch}"
		;;
	release)
		[[ "${ref}" == refs/tags/v* && "${ref_name}" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]] || {
			echo "publish-image: release ref must be a Docker-compatible v* tag" >&2
			return 1
		}
		[[ "${ref_name}" == "v${crate_version}" ]] || {
			echo "publish-image: release tag ${ref_name} does not match the crate version ${crate_version}" >&2
			return 1
		}
		promote_tags="${ref_name} latest"
		artifact_name="sbom-${ref_name}-${run_id}-${run_attempt}"
		identity="https://github.com/${repository}/.github/workflows/cd.yml@${ref}"
		;;
	*)
		echo "publish-image: unsupported mode '${mode}'" >&2
		return 1
		;;
	esac

	printf 'image_repo=%s\n' "${image_repo}"
	printf 'primary_image=%s:build-%s-%s\n' "${image_repo}" "${run_id}" "${run_attempt}"
	printf 'version=%s\n' "${crate_version}"
	printf 'promote_tags=%s\n' "${promote_tags}"
	printf 'artifact_name=%s\n' "${artifact_name}"
	printf 'identity=%s\n' "${identity}"
}

self_test() {
	local sha="0123456789abcdef0123456789abcdef01234567"
	local config_id="sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	local other_id="sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
	local run_image=ghcr.io/owner/service:build-42-1
	local got expected summary build_receipt manifest digest loaded_id push_line kind

	got="$(metadata main "${sha}" Owner/Service main '' '' 42 3 0.1.0)"
	expected=$'image_repo=ghcr.io/owner/service\nprimary_image=ghcr.io/owner/service:build-42-3\nversion=0.1.0\npromote_tags=sha-0123456789ab main\nartifact_name=sbom-main-0123456789ab-42-3\nidentity=https://github.com/Owner/Service/.github/workflows/cd.yml@refs/heads/main'
	[[ "${got}" == "${expected}" ]] || {
		echo "publish-image metadata self-test: main mismatch" >&2
		return 1
	}

	got="$(metadata release "${sha}" Owner/Service main v1.2.3 refs/tags/v1.2.3 42 4 1.2.3)"
	expected=$'image_repo=ghcr.io/owner/service\nprimary_image=ghcr.io/owner/service:build-42-4\nversion=1.2.3\npromote_tags=v1.2.3 latest\nartifact_name=sbom-v1.2.3-42-4\nidentity=https://github.com/Owner/Service/.github/workflows/cd.yml@refs/tags/v1.2.3'
	[[ "${got}" == "${expected}" ]] || {
		echo "publish-image metadata self-test: release mismatch" >&2
		return 1
	}

	if metadata main invalid Owner/Service main '' '' 42 1 0.1.0 >/dev/null 2>&1; then
		echo "publish-image metadata self-test: invalid SHA passed" >&2
		return 1
	fi
	if metadata release "${sha}" Owner/Service main v1/bad refs/tags/v1/bad 42 1 1.0.0 >/dev/null 2>&1; then
		echo "publish-image metadata self-test: invalid release tag passed" >&2
		return 1
	fi
	if metadata release "${sha}" Owner/Service main v1.2.4 refs/tags/v1.2.4 42 1 1.2.3 >/dev/null 2>&1; then
		echo "publish-image metadata self-test: release tag disagreeing with the crate version passed" >&2
		return 1
	fi
	if metadata main "${sha}" Owner/Service main '' '' 42 1 not-a-version >/dev/null 2>&1; then
		echo "publish-image metadata self-test: invalid crate version passed" >&2
		return 1
	fi

	PUBLISH_IMAGE_TEST_DIR=$(mktemp -d "${TMPDIR:-/tmp}/publish-image.XXXXXXXX")
	trap 'rm -rf -- "${PUBLISH_IMAGE_TEST_DIR}"' EXIT
	summary="${PUBLISH_IMAGE_TEST_DIR}/summary"
	build_receipt="${PUBLISH_IMAGE_TEST_DIR}/export.json"
	manifest="${PUBLISH_IMAGE_TEST_DIR}/manifest"
	printf '{"schemaVersion":2,"config":{"digest":"%s"},"layers":[]}\n\n' "${config_id}" >"${manifest}"
	digest="sha256:$(shasum -a 256 <"${manifest}" | cut -d' ' -f1)"
	printf '{"containerimage.digest":"%s","containerimage.descriptor":{"digest":"%s","mediaType":"application/vnd.oci.image.manifest.v1+json"},"containerimage.config.digest":"%s"}\n' \
		"${digest}" "${digest}" "${config_id}" >"${build_receipt}"
	push_line="build-42-1: digest: ${digest} size: 123"
	loaded_id=${config_id}
	docker() {
		case "$1" in
		image) printf '%s\n' "${loaded_id}" ;;
		tag) [[ $2 == "${config_id}" ]] ;;
		push)
			if [[ $2 == "${run_image}" ]]; then printf '%s\n' "${push_line}"; fi
			[[ "$2" != "${PUBLISH_IMAGE_TEST_FAIL_TAG:-}" ]]
			;;
		buildx)
			if [[ $4 == --raw ]]; then
				[[ $5 == "${run_image}" ]] || return 1
				cat "${manifest}"
			else
				printf '%s\n' "${PUBLISH_IMAGE_TEST_DIGEST:-${digest}}"
			fi
			[[ ${PUBLISH_IMAGE_TEST_READBACK_FAILED:-} != 1 ]]
			;;
		*) return 1 ;;
		esac
	}
	for kind in config descriptor; do
		if [[ ${kind} == descriptor ]]; then loaded_id=${digest}; fi
		got=$(identify "${run_image}" "${build_receipt}")
		[[ ${got} == "image_id=${loaded_id}"$'\n'"id_kind=${kind}" ]]
		got=$(push_candidate "${run_image}" "${loaded_id}" "${kind}")
		[[ ${got} == "${digest}" ]]
	done
	loaded_id=${other_id}
	if identify "${run_image}" "${build_receipt}" || push_candidate "${run_image}" "${config_id}" config; then
		echo "publish-image metadata self-test: replaced run tag passed" >&2
		return 1
	fi
	if push_candidate "${run_image}" "${other_id}" config || push_candidate "${run_image}" "${other_id}" descriptor; then
		echo "publish-image metadata self-test: mismatched registry subject passed" >&2
		return 1
	fi
	loaded_id=${config_id}
	if PUBLISH_IMAGE_TEST_FAIL_TAG="${run_image}" push_candidate "${run_image}" "${config_id}" config ||
		PUBLISH_IMAGE_TEST_READBACK_FAILED=1 push_candidate "${run_image}" "${config_id}" config; then
		echo "publish-image metadata self-test: failed producer confirmed a push" >&2
		return 1
	fi
	if push_line="${push_line}"$'\n'"${push_line}" push_candidate "${run_image}" "${config_id}" config ||
		push_line="another-tag: digest: ${digest} size: 123" push_candidate "${run_image}" "${config_id}" config; then
		echo "publish-image metadata self-test: ambiguous or wrong-tag push passed" >&2
		return 1
	fi
	printf '\n' >>"${manifest}"
	if push_candidate "${run_image}" "${config_id}" config; then
		echo "publish-image metadata self-test: altered registry bytes passed" >&2
		return 1
	fi
	printf '{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"digest":"%s"}]}' "${digest}" >"${manifest}"
	digest="sha256:$(shasum -a 256 <"${manifest}" | cut -d' ' -f1)"
	push_line="build-42-1: digest: ${digest} size: 155"
	printf '{"containerimage.digest":"%s","containerimage.descriptor":{"digest":"%s","mediaType":"application/vnd.oci.image.index.v1+json"},"containerimage.config.digest":"%s"}\n' \
		"${digest}" "${digest}" "${config_id}" >"${build_receipt}"
	if identify "${run_image}" "${build_receipt}" || push_candidate "${run_image}" "${config_id}" config; then
		echo "publish-image metadata self-test: index admitted from a config ID" >&2
		return 1
	fi
	loaded_id=${digest}
	got=$(identify "${run_image}" "${build_receipt}")
	[[ ${got} == "image_id=${digest}"$'\n'"id_kind=descriptor" ]]
	got=$(push_candidate "${run_image}" "${loaded_id}" descriptor)
	[[ ${got} == "${digest}" ]]

	promote ghcr.io/owner/service "${config_id}" "${digest}" 'sha-0123456789ab main' "${summary}"
	PUBLISH_IMAGE_TEST_FAIL_TAG=ghcr.io/owner/service:main
	if promote ghcr.io/owner/service "${config_id}" "${digest}" 'sha-0123456789ab main' "${summary}"; then
		echo "publish-image metadata self-test: partial promotion passed" >&2
		return 1
	fi
	grep -Fq 'Promoted tags before failure: sha-0123456789ab' "${summary}" || {
		echo "publish-image metadata self-test: partial receipt is incomplete" >&2
		return 1
	}
	: >"${summary}"
	PUBLISH_IMAGE_TEST_FAIL_TAG=ghcr.io/owner/service:sha-0123456789ab
	if promote ghcr.io/owner/service "${config_id}" "${digest}" 'sha-0123456789ab main' "${summary}"; then
		echo "publish-image metadata self-test: first-tag failure passed" >&2
		return 1
	fi
	grep -Fq 'Promoted tags before failure: none' "${summary}" || {
		echo "publish-image metadata self-test: empty partial receipt is incomplete" >&2
		return 1
	}
	unset PUBLISH_IMAGE_TEST_FAIL_TAG
	PUBLISH_IMAGE_TEST_DIGEST="${other_id}"
	if promote ghcr.io/owner/service "${config_id}" "${digest}" 'sha-0123456789ab main' "${summary}"; then
		echo "publish-image metadata self-test: a tag resolving to another digest passed" >&2
		return 1
	fi
	echo "publish-image metadata self-test passed"
}

case "${1:-}" in
metadata)
	[[ $# -eq 10 ]] || usage
	metadata "$2" "$3" "$4" "$5" "$6" "$7" "$8" "$9" "${10}"
	;;
identify)
	[[ $# -eq 3 ]] || usage
	identify "$2" "$3"
	;;
push)
	[[ $# -eq 4 ]] || usage
	push_candidate "$2" "$3" "$4"
	;;
promote)
	[[ $# -eq 6 ]] || usage
	promote "$2" "$3" "$4" "$5" "$6"
	;;
self-test)
	[[ $# -eq 1 ]] || usage
	self_test
	;;
*) usage ;;
esac
