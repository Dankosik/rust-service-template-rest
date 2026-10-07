#!/usr/bin/env bash
# Run one validation step and append its wall time, CPU time, and peak RSS to
# the GitHub step summary (or stdout), so a route's cost is visible per run.
#
#   measure.sh <route> -- command [args...]
set -euo pipefail

if [[ ${1:-} == --self-test ]]; then
	fixture=$(mktemp -d)
	trap 'rm -rf "${fixture}"' EXIT
	# Exercise the public recorder, including a failed command; it must retain
	# that exact exit code and hash the actual stream, not a supplied receipt.
	for expected in 0 7; do
		actual=0
		MEASURE_OUTPUT_DIR="${fixture}/records" GITHUB_STEP_SUMMARY="${fixture}/summary" \
			GITHUB_RUN_ID=100 GITHUB_RUN_ATTEMPT=2 bash "$0" "fixture-${expected}" -- \
			bash -c 'printf "fixture output\n"; exit "$1"' _ "${expected}" || actual=$?
		[[ ${actual} == "${expected}" ]] || exit 1
	done
	python3 - "${fixture}/records" <<'PYTEST'
import hashlib
import json
from pathlib import Path
import subprocess
import sys

records = [json.loads(path.read_text()) for path in Path(sys.argv[1]).glob("*.json")]
assert len(records) == 2
assert {record["exit_code"] for record in records} == {0, 7}
for record in records:
    assert record["run_id"] == "100" and record["producing_attempt"] == "2"
    assert record["source_revision"] == subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    data = (Path(sys.argv[1]) / record["log"]).read_bytes()
    assert b"fixture output" in data
    assert record["log_sha256"] == hashlib.sha256(data).hexdigest()
    assert record["finished_epoch"] >= record["started_epoch"]
PYTEST
	printf 'measurement recorder self-test: pass\n'
	exit 0
fi

route=${1:-}
[[ -n ${route} && ${2:-} == -- && $# -ge 3 ]] || {
	echo "usage: $0 route -- command [args...]" >&2
	exit 2
}
shift 2

candidate=$(git rev-parse HEAD 2>/dev/null || echo unavailable)
source_tree=$(git rev-parse 'HEAD^{tree}' 2>/dev/null || echo unavailable)
command_text=$(printf '%q ' "$@")
started=$(date +%s)
metrics=$(mktemp)
trap 'rm -f "${metrics}"' EXIT

# Image jobs retain the command stream, including BuildKit cache/import logs.
# Other callers keep the original summary-only behavior.
log_file=
if [[ -n ${MEASURE_OUTPUT_DIR:-} ]]; then
	mkdir -p "${MEASURE_OUTPUT_DIR}"
	log_file=$(mktemp "${MEASURE_OUTPUT_DIR}/command.XXXXXX")
	exec 3>&1 4>&2
	exec >"${log_file}" 2>&1
fi

set +e
if [[ $(uname -s) == Linux && -x /usr/bin/time ]]; then
	/usr/bin/time -f 'user=%U\nsystem=%S\nmax_rss_kb=%M' -o "${metrics}" "$@"
	status=$?
else
	"$@"
	status=$?
	printf 'user=unknown\nsystem=unknown\nmax_rss_kb=unknown\n' >"${metrics}"
fi
set -e
if [[ -n ${log_file} ]]; then
	exec 1>&3 2>&4 3>&- 4>&-
	cat "${log_file}"
fi
finished=$(date +%s)
wall_seconds=$((finished - started))
user_seconds=$(awk -F= '$1 == "user" { print $2 }' "${metrics}")
system_seconds=$(awk -F= '$1 == "system" { print $2 }' "${metrics}")
max_rss_kb=$(awk -F= '$1 == "max_rss_kb" { print $2 }' "${metrics}")
if [[ ${user_seconds} == unknown ]]; then
	cpu_seconds=unknown
	max_rss_mb=unknown
else
	cpu_seconds=$(awk -v user="${user_seconds}" -v sys="${system_seconds}" 'BEGIN { printf "%.2f", user + sys }')
	max_rss_mb=$(awk -v kb="${max_rss_kb}" 'BEGIN { printf "%.1f", kb / 1024 }')
fi
result=pass
((status == 0)) || result=fail

summary=${GITHUB_STEP_SUMMARY:-/dev/stdout}
{
	printf '### validation metric: %s\n\n' "${route}"
	printf -- '- route: %s\n' "${route}"
	printf -- '- candidate: %s\n' "${candidate}"
	printf -- '- command: %s\n' "${command_text% }"
	printf -- '- wall_seconds: %s\n' "${wall_seconds}"
	printf -- '- cpu_seconds: %s\n' "${cpu_seconds}"
	printf -- '- max_rss_mb: %s\n' "${max_rss_mb}"
	printf -- '- result: %s\n\n' "${result}"
} >>"${summary}"

if [[ -n ${log_file} ]]; then
	python3 - "${log_file}" "${route}" "${candidate}" "${source_tree}" "${started}" "${finished}" \
		"${status}" "${cpu_seconds}" "${max_rss_mb}" "${command_text% }" <<'PYMETRIC'
import hashlib
import json
import os
from pathlib import Path
import sys

log, route, revision, tree, started, finished, status, cpu, rss, command = sys.argv[1:]
def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()
metric = {
    "route": route, "source_revision": revision, "source_tree": tree,
    "run_id": os.getenv("GITHUB_RUN_ID"), "producing_attempt": os.getenv("GITHUB_RUN_ATTEMPT"),
    "job": os.getenv("GITHUB_JOB"), "runner_os": os.getenv("RUNNER_OS"),
    "runner_arch": os.getenv("RUNNER_ARCH"), "runner_image": os.getenv("ImageOS"),
    "runner_image_version": os.getenv("ImageVersion"),
    "lane": os.getenv("IMAGE_LANE"), "selected_graphs": os.getenv("ARTIFACT_GRAPHS"),
    "image_id": os.getenv("IMAGE_ID"), "command": command,
    "started_epoch": int(started), "finished_epoch": int(finished),
    "wall_seconds": int(finished) - int(started), "exit_code": int(status),
    "cpu_seconds": cpu, "max_rss_mb": rss,
    "log": Path(log).name, "log_sha256": digest(log),
    "cache_from": os.getenv("RUNTIME_IMAGE_CACHE_FROM"),
    "cache_to": os.getenv("RUNTIME_IMAGE_CACHE_TO"),
}
artifact = os.getenv("MEASURE_ARTIFACT_PATH")
if artifact and (int(status) == 0 or Path(artifact).is_file()):
    metric.update(artifact=artifact, artifact_sha256=digest(artifact))
Path(log + ".json").write_text(json.dumps(metric, indent=2) + "\n")
PYMETRIC
fi

exit "${status}"
