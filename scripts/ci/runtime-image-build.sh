#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

image=${1:-service:ci}
arguments=()
# Resolve service inputs (for example BuildKit secrets) before acquiring
# the validation lock, so a missing input refuses immediately.
# Dockerfile checks consume no service inputs.
service_runtime_image_build_arguments() { :; }
if [[ ${image} != --check && -f scripts/ci/service-runtime-image.sh ]]; then
	# shellcheck source=/dev/null
	source scripts/ci/service-runtime-image.sh
	service_runtime_image_build_arguments
fi

if [[ ${VALIDATION_LOCK_HELD:-} != 1 ]]; then
	exec bash scripts/ci/validation-lock.sh -- bash "$0" "$@"
fi
bash scripts/ci/validation-lock.sh --assert-held

builder_args=()
[[ -z ${VALIDATION_BUILDER_NAME:-} ]] || builder_args=(--builder "${VALIDATION_BUILDER_NAME}")
if [[ ${image} == --check ]]; then
	docker buildx build ${builder_args[@]+"${builder_args[@]}"} --check -f build/docker/Dockerfile .
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

arguments+=(
	--build-arg "CARGO_CHEF_VERSION=${CARGO_CHEF_VERSION}"
	--build-arg "CARGO_CHEF_X86_64_SHA256=${CARGO_CHEF_X86_64_SHA256}"
	--build-arg "CARGO_CHEF_AARCH64_SHA256=${CARGO_CHEF_AARCH64_SHA256}"
	--build-arg "CARGO_AUDITABLE_VERSION=${CARGO_AUDITABLE_VERSION}"
	--build-arg "CARGO_AUDITABLE_X86_64_SHA256=${CARGO_AUDITABLE_X86_64_SHA256}"
	--build-arg "CARGO_AUDITABLE_AARCH64_SHA256=${CARGO_AUDITABLE_AARCH64_SHA256}"
	--build-arg "SERVICE_PACKAGE=${service_package}"
	--build-arg "SERVICE_BIN=${SERVICE_BIN:-${service_package}}"
	--build-arg "APP_VERSION=${app_version}"
	--build-arg "VCS_REF=${vcs_ref}"
	--build-arg "SOURCE_URL=${SOURCE_URL:-}"
	--build-arg "SOURCE_DATE_EPOCH=${source_date_epoch}"
	--build-arg "IMAGE_TESTS=${IMAGE_TESTS:-none}"
	-f build/docker/Dockerfile
)
[[ -z ${RUNTIME_IMAGE_CACHE_FROM:-} ]] || arguments+=(--cache-from "${RUNTIME_IMAGE_CACHE_FROM}")

# Only the cooked stage is worth exporting: every later layer follows a source
# copy, so a new commit never reuses it, and exporting the binary
# stages would spend the repository's cache budget on dead layers.
if [[ -n ${RUNTIME_IMAGE_CACHE_TO:-} ]]; then
	docker buildx build ${builder_args[@]+"${builder_args[@]}"} "${arguments[@]}" --target cooked --cache-to "${RUNTIME_IMAGE_CACHE_TO}" .
fi
[[ -z ${RUNTIME_IMAGE_METADATA_FILE:-} ]] || arguments+=(--metadata-file "${RUNTIME_IMAGE_METADATA_FILE}")
docker buildx build ${builder_args[@]+"${builder_args[@]}"} --load "${arguments[@]}" -t "${image}" .
