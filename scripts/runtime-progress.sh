#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${ROOT_DIR}"

[[ $(uname -s) == Linux ]] || { echo "runtime progress proof requires Linux" >&2; exit 2; }
for cmd in cargo rustc jq docker sha256sum timeout; do
	command -v "${cmd}" >/dev/null || { echo "runtime progress proof requires ${cmd}" >&2; exit 2; }
done

git diff --quiet HEAD -- || { echo "freeze tracked changes before quota proof" >&2; exit 2; }
untracked=$(git ls-files --others --exclude-standard -- Cargo.toml Cargo.lock rust-toolchain.toml crates vendor test .cargo .sqlx migrations)
[[ -z ${untracked} ]] || { echo "freeze untracked build inputs before quota proof" >&2; exit 2; }

source_revision=$(git rev-parse HEAD)
run_id="${source_revision:0:12}-$(date +%s)-$$"
results=${RUNTIME_PROGRESS_RESULTS:-${ROOT_DIR}/.artifacts/runtime-progress/${run_id}}
[[ ${results} == /* && ! -e ${results} && ! -e ${results}.inputs ]] || {
	echo "RUNTIME_PROGRESS_RESULTS must be a new absolute path" >&2
	exit 2
}
inputs=${results}.inputs
mkdir -p "${inputs}/image"
image="runtime-progress-proof:${run_id}"
image_id=""
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
[[ -n ${base_image} ]] || { echo "Dockerfile has no pinned Rust base" >&2; exit 2; }
printf '%s\n' "${base_image}" >"${inputs}/base-image"
package=$(awk -F'"' '/^name[[:space:]]*=/ { print $2; exit }' crates/service/Cargo.toml)

export SQLX_OFFLINE=true VERGEN_GIT_SHA="${source_revision}"
cargo test --locked --package "${package}" --example runtime_progress 2>&1 | tee "${inputs}/specimen-tests.log"
cargo build --locked --release --package "${package}" --example runtime_progress --test runtime_progress \
	--message-format=json >"${inputs}/release-build.jsonl" 2> >(tee "${inputs}/release-build.stderr" >&2)

manifest_abs=$(realpath crates/service/Cargo.toml)
specimen=$(jq -r --arg m "${manifest_abs}" 'select(.reason == "compiler-artifact" and .manifest_path == $m and .target.name == "runtime_progress" and (.target.kind | index("example")) and .executable != null) | .executable' "${inputs}/release-build.jsonl" | tail -n 1)
driver=$(jq -r --arg m "${manifest_abs}" 'select(.reason == "compiler-artifact" and .manifest_path == $m and .target.name == "runtime_progress" and (.target.kind | index("test")) and .executable != null) | .executable' "${inputs}/release-build.jsonl" | tail -n 1)
[[ -x ${specimen} && -x ${driver} ]] || { echo "failed to locate runtime_progress executables" >&2; exit 1; }

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

test_name=bounded_sources_preserve_process_progress_under_one_cpu_quota
"${driver}" --list --ignored >"${inputs}/driver-tests"
grep -qx "${test_name}: test" "${inputs}/driver-tests"
export RUNTIME_PROGRESS_CONTAINER_PREFIX="runtime-progress-${run_id}"
for suffix in ordinary blocked primary negative; do
	container_names+=("${RUNTIME_PROGRESS_CONTAINER_PREFIX}-${suffix}")
done
export RUNTIME_PROGRESS_IMAGE="${image_id}" RUNTIME_PROGRESS_SOURCE="${source_revision}" RUNTIME_PROGRESS_RESULTS="${results}"
timeout --signal=TERM --kill-after=15s 12m "${driver}" --ignored --exact "${test_name}" --no-capture \
	2>&1 | tee "${inputs}/driver.log"
