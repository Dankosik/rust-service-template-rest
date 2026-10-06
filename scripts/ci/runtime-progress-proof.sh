#!/usr/bin/env bash
# One Linux release specimen, one immutable image, and the complete frozen
# quota driver. Keep inputs and all adverse observations beside its results.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"
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
workload_ticket=
workload_submissions_confirmed=false

# Invoked by EXIT below; ShellCheck 0.11 loses that edge after the final explicit exit.
# shellcheck disable=SC2329
cleanup() {
	local status=$? container remaining running cleanup_failed=false
	trap - EXIT INT TERM HUP
	set +e
	if [[ -n ${image_id} ]]; then
		# The image has a unique run label. This fallback owns only its containers;
		# the Rust driver normally captures and removes all four itself.
		if ! remaining=$(timeout 10s docker ps --all --quiet --filter "ancestor=${image_id}"); then
			cleanup_failed=true
		fi
		for container in ${remaining}; do
			timeout 10s docker inspect "${container}" >"${inputs}/${container}.cleanup-inspect.json" || cleanup_failed=true
			if ! running=$(timeout 10s docker inspect --format '{{.State.Running}}' "${container}"); then
				cleanup_failed=true
			elif [[ ${running} == true ]]; then
				timeout 10s docker kill "${container}" >/dev/null 2>&1 || cleanup_failed=true
			fi
			timeout 10s docker wait "${container}" >"${inputs}/${container}.cleanup-exit" || cleanup_failed=true
			timeout 15s docker logs "${container}" >"${inputs}/${container}.stdout" 2>"${inputs}/${container}.stderr" || cleanup_failed=true
			timeout 10s docker rm "${container}" >/dev/null || cleanup_failed=true
		done
		if ! remaining=$(timeout 10s docker ps --all --quiet --filter "ancestor=${image_id}"); then
			cleanup_failed=true
		elif [[ -n ${remaining} ]]; then
			cleanup_failed=true
		fi
		timeout 10s docker image rm "${image}" >"${inputs}/image-cleanup.log" 2>&1 || cleanup_failed=true
		if [[ -n ${workload_ticket} ]]; then
			if [[ ${workload_submissions_confirmed} != true ]]; then
				cleanup_failed=true
			elif [[ ${cleanup_failed} == false ]]; then
				bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-complete runtime-progress-workload "${workload_ticket}" workload-absent "${run_id}" || cleanup_failed=true
			fi
		fi
	elif [[ -n ${workload_ticket} ]]; then
		cleanup_failed=true
	fi
	if [[ ${cleanup_failed} == true ]]; then
		echo "runtime progress cleanup incomplete" >&2
		if [[ ${status} == 0 ]]; then status=1; fi
	fi
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
build_ticket=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-begin runtime-progress-build "${run_id}")
docker build --file build/docker/runtime-progress.Dockerfile \
	--build-arg "RUNTIME_PROGRESS_BASE_IMAGE=${base_image}" \
	--build-arg "RUNTIME_PROGRESS_SOURCE=${source_revision}" \
	--build-arg "RUNTIME_PROGRESS_RUN=${run_id}" \
	--tag "${image}" "${inputs}/image" 2>&1 | tee "${inputs}/image-build.log"
bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-complete runtime-progress-build "${build_ticket}" build-completed "${run_id}"
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
export RUNTIME_PROGRESS_IMAGE="${image_id}" RUNTIME_PROGRESS_SOURCE="${source_revision}" RUNTIME_PROGRESS_RESULTS="${results}"
workload_ticket=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-begin runtime-progress-workload "${run_id}")
if timeout --signal=TERM --kill-after=15s 12m "${driver}" --ignored --exact "${test_name}" --nocapture \
	2>&1 | tee "${inputs}/driver.log"; then
	driver_statuses=("${PIPESTATUS[@]}")
else
	driver_statuses=("${PIPESTATUS[@]}")
fi
# Libtest's 101 also covers inner Docker timeouts, so it alone cannot settle
# admission. The frozen driver launches exactly ordinary, blocked, primary and
# negative. Require all four native IDs plus its final disposition: an early
# failure without that complete evidence conservatively retains custody even
# after scoped cleanup. This reads existing evidence without changing oracles.
if [[ ${driver_statuses[0]} == 0 || ${driver_statuses[0]} == 101 ]]; then
	if python3 - "${results}" "${image_id}" "${driver_statuses[0]}" <<'PY'
import json
import pathlib
import re
import sys

try:
    directory = pathlib.Path(sys.argv[1])
    records = [json.loads(line) for path in directory.glob("*.jsonl")
               for line in path.read_text().splitlines()]
    final = [json.loads(line) for line in (directory / "final.jsonl").read_text().splitlines()]
    verdicts = [event for event in final if event.get("event") == "verdict"]
    expected_outcome = "passed" if sys.argv[3] == "0" else "failed"
    if len(verdicts) != 1 or verdicts[0].get("outcome") != expected_outcome:
        raise ValueError("missing terminal disposition")
    commands = []
    started = []
    identities = []
    prefixes = set()
    suffixes = set()
    for event in records:
        if event.get("event") == "container_command":
            arguments = event["arguments"]
            if arguments[:3] != ["run", "--detach", "--name"] or arguments[-3] != sys.argv[2]:
                raise ValueError("unexpected launch identity")
            commands.append(arguments[3])
        elif event.get("event") == "container_started":
            name, identity = event["name"], event["id"]
            match = re.fullmatch(r"(runtime-progress-[0-9]+-[0-9]+)-(ordinary|blocked|primary|negative)", name)
            if match is None or re.fullmatch(r"[0-9a-f]{64}", identity) is None:
                raise ValueError("missing native identity")
            prefixes.add(match[1])
            suffixes.add(match[2])
            started.append(name)
            identities.append(identity)
    if (len(commands) != 4 or len(started) != 4 or sorted(commands) != sorted(started)
            or len(prefixes) != 1 or len(set(identities)) != 4
            or suffixes != {"ordinary", "blocked", "primary", "negative"}):
        raise ValueError("incomplete launch inventory")
except (OSError, ValueError, KeyError, IndexError, TypeError, AttributeError):
    raise SystemExit("runtime progress submission completion remains unknown") from None
PY
	then
		workload_submissions_confirmed=true
	fi
fi
if [[ ${driver_statuses[0]} != 0 ]]; then exit "${driver_statuses[0]}"; fi
exit "${driver_statuses[1]}"
