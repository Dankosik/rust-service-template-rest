#!/usr/bin/env bash
# Build the runtime image from the repository context with BuildKit. One
# command for make, CI, and publication.
#
#   runtime-image-build.sh [IMAGE]          default service:ci
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
image=${1:-service:ci}

# shellcheck source=tools/versions.env
. tools/versions.env

vcs_ref=${VCS_REF:-$(git rev-parse HEAD 2>/dev/null || echo unknown)}
source_date_epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || echo 0)}
app_version=${APP_VERSION:-}
if [[ -z ${app_version} ]] && command -v cargo >/dev/null 2>&1; then
	# cargo pkgid prints path+file:///…/crates/service#0.1.0
	app_version=$(cargo pkgid --locked -p service 2>/dev/null | sed -n 's/.*#//p')
fi
app_version=${app_version:-unknown}

build=(docker buildx build --load)
[[ -z ${RUNTIME_IMAGE_CACHE_FROM:-} ]] || build+=(--cache-from "${RUNTIME_IMAGE_CACHE_FROM}")
[[ -z ${RUNTIME_IMAGE_CACHE_TO:-} ]] || build+=(--cache-to "${RUNTIME_IMAGE_CACHE_TO}")

"${build[@]}" \
	--build-arg "CARGO_CHEF_VERSION=${CARGO_CHEF_VERSION}" \
	--build-arg "CARGO_AUDITABLE_VERSION=${CARGO_AUDITABLE_VERSION}" \
	--build-arg "APP_VERSION=${app_version}" \
	--build-arg "VCS_REF=${vcs_ref}" \
	--build-arg "SOURCE_URL=${SOURCE_URL:-}" \
	--build-arg "SOURCE_DATE_EPOCH=${source_date_epoch}" \
	-f build/docker/Dockerfile \
	-t "${image}" \
	.
