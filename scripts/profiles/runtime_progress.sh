#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"
if ! python3 "${ROOT_DIR}/scripts/verify/lock.py" --assert-held; then
	exec python3 "${ROOT_DIR}/scripts/verify/lock.py" -- bash "${ROOT_DIR}/scripts/profiles/runtime_progress.sh" "$@"
fi
[[ $(uname -s) == Linux ]] || { echo "runtime progress proof requires Linux" >&2; exit 2; }
for command in cargo rustc python3 docker sha256sum timeout; do
	command -v "${command}" >/dev/null || { echo "runtime progress proof requires ${command}" >&2; exit 2; }
done

# This runner consumes a frozen checkout. Untracked coordination documents do
# not enter Cargo; untracked build inputs must not escape the source identity.
git diff --quiet HEAD -- || { echo "freeze tracked changes before quota proof" >&2; exit 2; }
untracked=$(git ls-files --others --exclude-standard -- Cargo.toml Cargo.lock rust-toolchain.toml crates vendor test .cargo .sqlx migrations)
[[ -z ${untracked} ]] || { echo "freeze untracked build inputs before quota proof" >&2; exit 2; }
source_revision=$(git rev-parse HEAD)
run_id="${source_revision:0:12}-$(date +%s)-$$"
results=${RUNTIME_PROGRESS_RESULTS:-${ROOT_DIR}/.artifacts/runtime-progress/${run_id}}
[[ ${results} == /* && ! -e ${results} && ! -e ${results}.inputs ]] || {
	echo "RUNTIME_PROGRESS_RESULTS must be a new absolute path; preserve earlier runs" >&2
	exit 2
}
inputs=${results}.inputs
mkdir -p "${inputs}/image"
image="runtime-progress-proof:${run_id}"
image_id=
container_names=()

cleanup() {
	local status=$? container cleanup_failed=false
	trap - EXIT INT TERM HUP
	set +e
	for container in "${container_names[@]}"; do
		if timeout 10s docker inspect "${container}" >"${inputs}/${container}.cleanup-inspect.json" 2>&1; then
			timeout 15s docker logs "${container}" >"${inputs}/${container}.stdout" 2>"${inputs}/${container}.stderr" || cleanup_failed=true
		fi
		docker rm -f -v "${container}" >/dev/null 2>&1 || true
	done
	if [[ -n ${image_id} && ${cleanup_failed} == false ]]; then
		timeout 10s docker image rm "${image}" >"${inputs}/image-cleanup.log" 2>&1 || cleanup_failed=true
	fi
	if [[ ${cleanup_failed} == true && ${status} == 0 ]]; then status=1; fi
	printf '%s\n' "${status}" >"${inputs}/runner-exit-code"
	exit "${status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

timeout 10s docker info --format '{{.CgroupVersion}}' >"${inputs}/cgroup-version"
[[ $(<"${inputs}/cgroup-version") == 2 ]] || { echo "Docker cgroup v2 is required" >&2; exit 2; }
printf '%s\n' "${source_revision}" >"${inputs}/source-commit"
git archive --format=tar HEAD | sha256sum >"${inputs}/source-archive.sha256"
sha256sum Cargo.lock rust-toolchain.toml >"${inputs}/build-inputs.sha256"
rustc --version --verbose >"${inputs}/rustc-version"
cargo --version >"${inputs}/cargo-version"
uname -a >"${inputs}/kernel"
base_image=$(awk '$1 == "FROM" && $2 ~ /^rust:.*@sha256:/ { print $2; exit }' build/docker/Dockerfile)
[[ -n ${base_image} ]] || { echo "production Dockerfile has no pinned Rust base" >&2; exit 2; }
printf '%s\n' "${base_image}" >"${inputs}/base-image"
package=$(python3 -c 'import pathlib,tomllib; print(tomllib.loads(pathlib.Path("crates/service/Cargo.toml").read_text())["package"]["name"])')

# The build runs outside the measured quota, with the pinned toolchain and the
# workspace's unchanged release profile/features/allocator. Reuse Cargo's cache.
export SQLX_OFFLINE=true VERGEN_GIT_SHA="${source_revision}"
# Cargo's ordinary workspace suite builds examples without necessarily running
# their unit cases. Exercise the specimen's occupancy oracle once before the
# release workload; the driver's nonignored cases belong to workspace tests.
cargo test --locked --package "${package}" --example runtime_progress \
	2>&1 | tee "${inputs}/specimen-tests.log"
# One release invocation builds the specimen and the external driver before
# anything is measured, so their single-threaded fat-LTO links run side by
# side instead of one after the other; an example and a test resolve the same
# dev-dependency features.
cargo build --locked --release --package "${package}" --example runtime_progress --test runtime_progress \
	--message-format=json >"${inputs}/release-build.jsonl" 2> >(tee "${inputs}/release-build.stderr" >&2)
specimen=$(python3 scripts/image/cargo_artifact.py --manifest crates/service/Cargo.toml --name runtime_progress --kind example <"${inputs}/release-build.jsonl")
driver=$(python3 scripts/image/cargo_artifact.py --manifest crates/service/Cargo.toml --name runtime_progress --kind test <"${inputs}/release-build.jsonl")
cp "${specimen}" "${inputs}/image/runtime_progress"
sha256sum "${inputs}/image/runtime_progress" >"${inputs}/binary.sha256"
builder_args=()
[[ -z ${VALIDATION_BUILDER_NAME:-} ]] || builder_args=(--builder "${VALIDATION_BUILDER_NAME}")
docker buildx build ${builder_args[@]+"${builder_args[@]}"} --load --file build/docker/runtime-progress.Dockerfile \
	--build-arg "RUNTIME_PROGRESS_BASE_IMAGE=${base_image}" \
	--build-arg "RUNTIME_PROGRESS_SOURCE=${source_revision}" \
	--build-arg "RUNTIME_PROGRESS_RUN=${run_id}" \
	--tag "${image}" "${inputs}/image" 2>&1 | tee "${inputs}/image-build.log"
image_id=$(docker image inspect --format '{{.Id}}' "${image}")
docker image inspect "${image_id}" >"${inputs}/image-inspect.json"

# The driver was built with the specimen; invoke its exact test so Cargo
# compilation cannot compete with the service during the quota window.
test_name=bounded_sources_preserve_process_progress_under_one_cpu_quota
"${driver}" --list --ignored >"${inputs}/driver-tests"
grep -qx "${test_name}: test" "${inputs}/driver-tests"
lock_token=${VALIDATION_LOCK_TOKEN:-nolock}
export RUNTIME_PROGRESS_CONTAINER_PREFIX="runtime-progress-${lock_token:0:12}-${run_id}"
for suffix in ordinary blocked primary negative; do
	container_names+=("${RUNTIME_PROGRESS_CONTAINER_PREFIX}-${suffix}")
done
export RUNTIME_PROGRESS_IMAGE="${image_id}" RUNTIME_PROGRESS_SOURCE="${source_revision}" RUNTIME_PROGRESS_RESULTS="${results}"
timeout --signal=TERM --kill-after=15s 12m "${driver}" --ignored --exact "${test_name}" --no-capture \
	2>&1 | tee "${inputs}/driver.log"
