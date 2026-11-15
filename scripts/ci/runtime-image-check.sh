#!/usr/bin/env bash
set -euo pipefail

image=${1:?runtime image is required}
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
expected_commit=${2:-}
role=${RUNTIME_IMAGE_ROLE:-service}
cd "${root}"
if ! bash "${root}/scripts/ci/validation-lock.sh" --assert-held; then
	exec bash "${root}/scripts/ci/validation-lock.sh" -- bash "${root}/scripts/ci/runtime-image-check.sh" "$@"
fi
image=$(docker image inspect --format '{{.Id}}' "${image}")
bash "${root}/scripts/ci/runtime-image-scan.sh" filesystem "${image}"
container="${role}-runtime-check-${VALIDATION_LOCK_TOKEN:0:12}-$$"
worker=

# Never empty, so the expansion below is safe under `set -u` on bash 3.2.
docker_args=(--label "runtime-check=${container}")
if [[ -n ${RUNTIME_IMAGE_NETWORK:-} ]]; then
	docker_args+=(--network "${RUNTIME_IMAGE_NETWORK}")
fi
if [[ -n ${RUNTIME_IMAGE_POSTGRES_DSN:-} ]]; then
	docker_args+=(
		-e APP__POSTGRES__ENABLED=true
		-e "APP__POSTGRES__DSN=${RUNTIME_IMAGE_POSTGRES_DSN}"
	)
fi

run_args=()
starting_event=service_starting
# A service hook may name its own worker refusal; empty keeps the profile default.
worker_refusal=''
check_worker=true
service_runtime_image_configure() {
	[[ ${role} == service ]] || { echo "unknown image role" >&2; return 2; }
}
service_runtime_image_observe() { :; }
if [[ -f scripts/ci/service-runtime-image.sh ]]; then
	# shellcheck source=/dev/null
	source scripts/ci/service-runtime-image.sh
fi
service_runtime_image_configure

cleanup() {
	local status=$?
	trap - EXIT INT TERM HUP
	if [[ -n ${worker} ]]; then
		docker rm -f -v "${worker}" >/dev/null 2>&1 || true
	fi
	docker rm -f -v "${container}" >/dev/null 2>&1 || true
	exit "${status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

command -v curl >/dev/null 2>&1 || {
	echo "curl is required for the runtime image check" >&2
	exit 2
}

# The same flags a hardened deployment uses; a binary that needs a writable
# root, a capability, or privilege escalation fails here first.
docker run -d --name "${container}" \
	-p 127.0.0.1::8080 \
	--read-only \
	--cap-drop=ALL \
	--security-opt=no-new-privileges \
	"${docker_args[@]}" \
	"${image}" ${run_args[@]+"${run_args[@]}"} >/dev/null

address=$(docker port "${container}" 8080/tcp 2>/dev/null | head -n 1 || true)
port=${address##*:}
if [[ -z ${port} ]]; then
	if [[ $(docker inspect -f '{{.State.Running}}' "${container}") != true ]]; then
		echo "runtime image exited before publishing its service port" >&2
	else
		echo "failed to resolve the runtime service port" >&2
	fi
	docker logs "${container}" >&2
	exit 1
fi

ready=false
for _ in {1..45}; do
	if curl -fs --max-time 2 "http://127.0.0.1:${port}/health/ready" >/dev/null; then
		ready=true
		break
	fi
	[[ $(docker inspect -f '{{.State.Running}}' "${container}") == true ]] || break
	sleep 1
done
if [[ ${ready} != true ]]; then
	echo "runtime image did not become ready" >&2
	docker logs "${container}" >&2
	exit 1
fi

logs=$(docker logs "${container}" 2>&1)
python3 -c '
import json
import sys

starting_event, expected_commit, require_pool = sys.argv[1:]
logs = sys.stdin.read()
starting = None
pool_opened = False
for line in logs.split("\n"):
    try:
        event = json.loads(line)
    except ValueError:
        continue
    if not isinstance(event, dict):
        continue
    if event.get("message") == starting_event and starting is None:
        starting = event
        starting_line = line
    if event.get("message") == "postgres_pool_opened":
        pool_opened = True

if starting is None:
    print(f"runtime image logged no {starting_event} record", file=sys.stderr)
    sys.exit(logs.rstrip("\n"))
if expected_commit and starting.get("app.commit") != expected_commit:
    print(f"runtime image did not report app.commit {expected_commit}", file=sys.stderr)
    sys.exit(starting_line)
identity = (f"{json.dumps(key)}:{json.dumps(value)}" for key, value in starting.items()
            if key in ("app.version", "app.commit") and isinstance(value, str))
print("runtime image ready:", " ".join(identity))
if require_pool:
    if not pool_opened:
        print("runtime image did not open the postgres pool", file=sys.stderr)
        sys.exit(logs.rstrip("\n"))
    print("runtime image opened the postgres pool")
' "${starting_event}" "${expected_commit}" "${RUNTIME_IMAGE_POSTGRES_DSN:+true}" <<<"${logs}"

stop_started=$(date +%s)
docker stop --time 45 "${container}" >/dev/null
stop_seconds=$(($(date +%s) - stop_started))
exit_code=$(docker inspect -f '{{.State.ExitCode}}' "${container}")
[[ ${exit_code} == 0 ]] || {
	echo "runtime image exited with code ${exit_code} after SIGTERM (${stop_seconds}s)" >&2
	docker logs "${container}" >&2
	exit 1
}
echo "runtime image stopped cleanly in ${stop_seconds}s (budget 45s)"

service_runtime_image_observe
[[ ${check_worker} == true ]] || exit 0

# The worker entrypoint is retained when either jobs or messaging survives.
# It must refuse before dependency I/O with the default configuration and no
# network, so the image check observes the retained binary without requiring a
# broker or database.
profile_lines=$(env -u PYTHONPATH -u PYTHONHOME PYTHONDONTWRITEBYTECODE=1 python3 -B "${root}/scripts/lib/template_state.py" profile --repo "${root}" --fields jobs messaging webhooks inbound_webhooks outbox) || {
	echo "cannot resolve selected worker profiles" >&2
	exit 2
}
jobs=''
messaging=''
webhooks=''
inbound_webhooks=''
outbox=''
while IFS='=' read -r key value; do
	case "${key}" in
	jobs | messaging | webhooks | inbound_webhooks | outbox)
		printf -v "${key}" '%s' "${value}"
		;;
	*)
		echo "unexpected profile field from template_state.py: ${key}" >&2
		exit 2
		;;
	esac
done <<<"${profile_lines}"
if [[ ${jobs} == postgres || ${messaging} == nats-jetstream ]]; then
	worker="${container}-jobs-worker"
	docker create --name "${worker}" --label "runtime-check=${container}" \
		--read-only \
		--cap-drop=ALL \
		--security-opt=no-new-privileges \
		--network none \
		--entrypoint /jobs-worker \
		"${image}" >/dev/null
	expected="no job kind or typed message handler is registered: register this service's retained capabilities in crates/jobs-worker/src/main.rs"
	if [[ ${webhooks} != none || ${inbound_webhooks} != none || ${outbox} == postgres ]]; then
		expected='postgres.enabled must be true to run the jobs worker'
	fi
	[[ -z ${worker_refusal} ]] || expected=${worker_refusal}
	worker_output=$(docker start --attach "${worker}" 2>&1 || true)
	worker_exit=$(docker inspect -f '{{.State.ExitCode}}' "${worker}")
	refusal=$(grep -Fo "${expected}" <<<"${worker_output}" | head -n 1 || true)
	if [[ ${worker_exit} != 1 || -z ${refusal} ]]; then
		echo "jobs-worker did not give the expected startup refusal (exit ${worker_exit})" >&2
		printf '%s\n' "${worker_output}" >&2
		exit 1
	fi
	echo "jobs-worker refused before dependency I/O: ${refusal}"
else
	echo "no worker profile is retained; the image has no /jobs-worker entrypoint"
fi
