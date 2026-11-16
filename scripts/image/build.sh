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
if [[ ${image} != --check && -e scripts/ci/service-runtime-image.sh ]]; then
	echo "refusing legacy hook scripts/ci/service-runtime-image.sh; move to scripts/service/runtime-image.sh" >&2
	exit 2
fi
if [[ ${image} != --check && -f scripts/service/runtime-image.sh ]]; then
	# shellcheck source=/dev/null
	source scripts/service/runtime-image.sh
	service_runtime_image_build_arguments
fi

if [[ ${VALIDATION_LOCK_HELD:-} != 1 ]]; then
	exec python3 "${ROOT_DIR}/scripts/verify/lock.py" -- bash "$0" "$@"
fi
python3 "${ROOT_DIR}/scripts/verify/lock.py" --assert-held

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
service_package=${SERVICE_PACKAGE:-}
app_version=${APP_VERSION:-}
if [[ -z ${service_package} || -z ${app_version} ]]; then
	metadata=$(
		python3 -B -E - <<'PY'
import sys
import tomllib

with open("crates/service/Cargo.toml", "rb") as handle:
    package = tomllib.load(handle).get("package", {})
name = package.get("name")
version = package.get("version")
if isinstance(version, dict) and version.get("workspace") is True:
    with open("Cargo.toml", "rb") as handle:
        version = tomllib.load(handle).get("workspace", {}).get("package", {}).get("version")
if not isinstance(name, str) or not name:
    sys.exit("runtime-image-build: crates/service/Cargo.toml declares no package.name")
if not isinstance(version, str) or not version:
    sys.exit("runtime-image-build: unable to resolve crates/service/Cargo.toml package.version")
print(name)
print(version)
PY
	)
	manifest_package=${metadata%%$'\n'*}
	manifest_version=${metadata#*$'\n'}
	service_package=${service_package:-${manifest_package}}
	app_version=${app_version:-${manifest_version}}
fi
[[ -n ${service_package} && -n ${app_version} ]] || {
	echo "runtime-image-build: service package and version must be non-empty" >&2
	exit 1
}

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
	--build-arg "PGO=${PGO:-none}"
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
