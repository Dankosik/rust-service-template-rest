#!/usr/bin/env bash
# One Linux release specimen, one immutable image, and the complete frozen
# quota driver. Keep inputs and all adverse observations beside its results.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"
if [[ ${VALIDATION_LOCK_HELD:-} != 1 ]]; then
	exec bash scripts/ci/validation-lock.sh -- bash "$0" "$@"
fi
bash scripts/ci/validation-lock.sh --assert-held
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
builder_resource=
container_resources=()
container_names=()

cleanup() {
	local status=$? resource container cleanup_failed=false
	trap - EXIT INT TERM HUP
	set +e
	# Registered names and IDs are the cancellation boundary, including a run
	# whose Docker CLI returned no ID. The driver normally retains logs first.
	for container in "${container_names[@]}"; do
		if timeout 10s docker inspect "${container}" >"${inputs}/${container}.cleanup-inspect.json" 2>&1; then
			timeout 15s docker logs "${container}" >"${inputs}/${container}.stdout" 2>"${inputs}/${container}.stderr" || cleanup_failed=true
		fi
	done
	for resource in "${container_resources[@]}"; do
		bash scripts/ci/validation-lock.sh --resource-cleanup "${resource}" || cleanup_failed=true
		bash scripts/ci/validation-lock.sh --resource-complete "${resource}" || cleanup_failed=true
	done
	if [[ -n ${image_id} && ${cleanup_failed} == false ]]; then
		timeout 10s docker image rm "${image}" >"${inputs}/image-cleanup.log" 2>&1 || cleanup_failed=true
	fi
	if [[ -n ${builder_resource} ]]; then
		bash scripts/ci/validation-lock.sh --resource-cleanup "${builder_resource}" || cleanup_failed=true
		bash scripts/ci/validation-lock.sh --resource-complete "${builder_resource}" || cleanup_failed=true
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
cargo build --locked --release --package "${package}" --example runtime_progress \
	--message-format=json >"${inputs}/release-build.jsonl" 2> >(tee "${inputs}/release-build.stderr" >&2)
python3 - "${inputs}/release-build.jsonl" "${inputs}/image/runtime_progress" <<'PY'
import json
import pathlib
import shutil
import sys
artifacts = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
executables = [entry["executable"] for entry in artifacts
               if entry.get("reason") == "compiler-artifact"
               and entry["target"]["name"] == "runtime_progress"
               and "example" in entry["target"]["kind"] and entry.get("executable")]
if len(executables) != 1:
    raise SystemExit("release build did not identify exactly one runtime_progress specimen")
shutil.copy2(executables[0], sys.argv[2])
PY
sha256sum "${inputs}/image/runtime_progress" >"${inputs}/binary.sha256"
builder_arguments=()
[[ -z ${VALIDATION_BUILDER_NAME:-} ]] || builder_arguments+=("${VALIDATION_BUILDER_NAME}")
builder_resource=$(bash scripts/ci/validation-lock.sh --builder-prepare "${builder_arguments[@]}")
builder_name=$(bash scripts/ci/validation-lock.sh --builder-name "${builder_resource}")
bash scripts/ci/validation-lock.sh --resource-run "${builder_resource}" -- \
	docker buildx build --builder "${builder_name}" --load --file build/docker/runtime-progress.Dockerfile \
	--build-arg "RUNTIME_PROGRESS_BASE_IMAGE=${base_image}" \
	--build-arg "RUNTIME_PROGRESS_SOURCE=${source_revision}" \
	--build-arg "RUNTIME_PROGRESS_RUN=${run_id}" \
	--tag "${image}" "${inputs}/image" 2>&1 | tee "${inputs}/image-build.log"
bash scripts/ci/validation-lock.sh --resource-cleanup "${builder_resource}"
bash scripts/ci/validation-lock.sh --resource-complete "${builder_resource}"
builder_resource=
image_id=$(docker image inspect --format '{{.Id}}' "${image}")
docker image inspect "${image_id}" >"${inputs}/image-inspect.json"

# Build the external driver before measurement, then invoke its exact test so
# Cargo compilation cannot compete with the service during the quota window.
cargo test --locked --package "${package}" --test runtime_progress --no-run \
	--message-format=json >"${inputs}/driver-build.jsonl" 2> >(tee "${inputs}/driver-build.stderr" >&2)
driver=$(python3 - "${inputs}/driver-build.jsonl" <<'PY'
import json
import pathlib
import sys
artifacts = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
executables = [entry["executable"] for entry in artifacts
               if entry.get("reason") == "compiler-artifact"
               and entry["target"]["name"] == "runtime_progress"
               and "test" in entry["target"]["kind"] and entry.get("executable")]
if len(executables) != 1:
    raise SystemExit("driver build did not identify exactly one runtime_progress test executable")
print(executables[0])
PY
)
test_name=bounded_sources_preserve_process_progress_under_one_cpu_quota
"${driver}" --list --ignored >"${inputs}/driver-tests"
grep -qx "${test_name}: test" "${inputs}/driver-tests"
export RUNTIME_PROGRESS_CONTAINER_PREFIX="runtime-progress-${VALIDATION_LOCK_TOKEN:0:12}-${run_id}"
export RUNTIME_PROGRESS_VALIDATION_HELPER="${ROOT_DIR}/scripts/ci/validation-lock.sh"
for suffix in ordinary blocked primary negative; do
	container_name="${RUNTIME_PROGRESS_CONTAINER_PREFIX}-${suffix}"
	resource=$(bash scripts/ci/validation-lock.sh --resource-register container "${container_name}")
	container_resources+=("${resource}")
	container_names+=("${container_name}")
	export "RUNTIME_PROGRESS_RESOURCE_${suffix}=${resource}"
done
export RUNTIME_PROGRESS_IMAGE="${image_id}" RUNTIME_PROGRESS_SOURCE="${source_revision}" RUNTIME_PROGRESS_RESULTS="${results}"
timeout --signal=TERM --kill-after=15s 12m "${driver}" --ignored --exact "${test_name}" --nocapture \
	2>&1 | tee "${inputs}/driver.log"
