#!/usr/bin/env bash
# Build the runtime image from the repository context with BuildKit. One
# command for make, CI, and publication.
#
#   runtime-image-build.sh [IMAGE]          default service:ci
#   runtime-image-build.sh --check          Dockerfile checks only
#
# Environment:
#   VCS_REF                  commit baked into the binary (default: HEAD)
#   APP_VERSION              OCI version label (default: the service crate version)
#   SOURCE_URL               OCI source label
#   SOURCE_DATE_EPOCH        binary mtime (default: HEAD commit time, else 0)
#   RUNTIME_IMAGE_CACHE_FROM / RUNTIME_IMAGE_CACHE_TO
#                            buildx cache specs, e.g. type=gha,scope=runtime-image
#
# Tool versions inside the image come from tools/versions.env as build
# arguments; the Dockerfile carries the same values as defaults for builds
# that pass none (Railway), and `make tools-check` asserts they agree.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"
# A direct script call has the same custody as its Make/CI entry point.
if [[ ${VALIDATION_LOCK_HELD:-} != 1 ]]; then
	exec bash scripts/ci/validation-lock.sh -- bash "$0" "$@"
fi
bash scripts/ci/validation-lock.sh --assert-held

image=${1:-service:ci}
builder_resource=
cleanup() {
	local status=$?
	trap - EXIT INT TERM HUP
	if [[ -n ${builder_resource} ]]; then
		if ! bash scripts/ci/validation-lock.sh --resource-cleanup "${builder_resource}"; then
			[[ ${status} != 0 ]] || status=1
		elif ! bash scripts/ci/validation-lock.sh --resource-complete "${builder_resource}"; then
			[[ ${status} != 0 ]] || status=1
		fi
	fi
	exit "${status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
builder_arguments=()
[[ -z ${VALIDATION_BUILDER_NAME:-} ]] || builder_arguments+=("${VALIDATION_BUILDER_NAME}")
builder_resource=$(bash scripts/ci/validation-lock.sh --builder-prepare "${builder_arguments[@]}")
builder_name=$(bash scripts/ci/validation-lock.sh --builder-name "${builder_resource}")
if [[ ${image} == --check ]]; then
	bash scripts/ci/validation-lock.sh --resource-run "${builder_resource}" -- \
		docker buildx build --builder "${builder_name}" --check -f build/docker/Dockerfile .
	exit
fi

# shellcheck source=tools/versions.env
. tools/versions.env

vcs_ref=${VCS_REF:-$(git rev-parse HEAD 2>/dev/null || echo unknown)}
source_date_epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || echo 0)}
package_id=
if command -v cargo >/dev/null 2>&1; then
	package_id=$(cargo pkgid --locked --manifest-path crates/service/Cargo.toml 2>/dev/null || true)
fi
service_package=${SERVICE_PACKAGE:-$(printf '%s' "${package_id}" | sed -n 's/.*#\([^@]*\)@.*/\1/p')}
service_package=${service_package:-service}
app_version=${APP_VERSION:-$(printf '%s' "${package_id}" | sed -E 's/.*#([^@]+@)?//')}
app_version=${app_version:-unknown}

arguments=(
	--build-arg "CARGO_CHEF_VERSION=${CARGO_CHEF_VERSION}"
	--build-arg "CARGO_AUDITABLE_VERSION=${CARGO_AUDITABLE_VERSION}"
	--build-arg "SERVICE_PACKAGE=${service_package}"
	--build-arg "SERVICE_BIN=${SERVICE_BIN:-${service_package}}"
	--build-arg "APP_VERSION=${app_version}"
	--build-arg "VCS_REF=${vcs_ref}"
	--build-arg "SOURCE_URL=${SOURCE_URL:-}"
	--build-arg "SOURCE_DATE_EPOCH=${source_date_epoch}"
	-f build/docker/Dockerfile
)
[[ -z ${RUNTIME_IMAGE_CACHE_FROM:-} ]] || arguments+=(--cache-from "${RUNTIME_IMAGE_CACHE_FROM}")

# Only the cooked stage is worth exporting: every later layer follows a source
# copy, so a new commit never reuses it, and exporting the three binary
# stages would spend the repository's cache budget on dead layers.
if [[ -n ${RUNTIME_IMAGE_CACHE_TO:-} ]]; then
	bash scripts/ci/validation-lock.sh --resource-run "${builder_resource}" -- \
		docker buildx build --builder "${builder_name}" "${arguments[@]}" --target cooked --cache-to "${RUNTIME_IMAGE_CACHE_TO}" .
fi
bash scripts/ci/validation-lock.sh --resource-run "${builder_resource}" -- \
	docker buildx build --builder "${builder_name}" --load "${arguments[@]}" -t "${image}" .
