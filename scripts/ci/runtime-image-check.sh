#!/usr/bin/env bash
# Lifecycle check of a built runtime image, observed from outside the
# container because the distroless base has no shell or curl: the service
# starts under the hardened run flags, /health/ready answers, the startup
# record carries the expected commit, and SIGTERM ends the process with exit
# code 0 inside the 45 s grace budget (docs/configuration-source-policy.md).
# A final step takes its /jobs-worker expectation from the repository's jobs
# selection: where the pack is retained, the image must carry the entrypoint
# and it must refuse to start before any database I/O; otherwise the image
# must not carry it.
#
#   runtime-image-check.sh IMAGE [EXPECTED_COMMIT]
#   RUNTIME_IMAGE_NETWORK      Docker network to join (a compose project's)
#   RUNTIME_IMAGE_POSTGRES_DSN Enable the PostgreSQL profile with this DSN, so
#                              readiness is observed with the pool open
set -euo pipefail

image=${1:?runtime image is required}
expected_commit=${2:-}
container="service-runtime-check-$$"

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

cleanup() {
	docker rm -f "${container}" "${container}-jobs-worker" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

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
	"${image}" >/dev/null

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
starting=$(grep -F '"message":"service_starting"' <<<"${logs}" | head -n 1 || true)
if [[ -z ${starting} ]]; then
	echo "runtime image logged no service_starting record" >&2
	printf '%s\n' "${logs}" >&2
	exit 1
fi
if [[ -n ${expected_commit} ]] && ! grep -Fq "\"app.commit\":\"${expected_commit}\"" <<<"${starting}"; then
	echo "runtime image did not report app.commit ${expected_commit}" >&2
	printf '%s\n' "${starting}" >&2
	exit 1
fi
echo "runtime image ready: $(grep -oE '"app\.(version|commit)":"[^"]*"' <<<"${starting}" | tr '\n' ' ')"
if [[ -n ${RUNTIME_IMAGE_POSTGRES_DSN:-} ]]; then
	grep -Fq '"message":"postgres_pool_opened"' <<<"${logs}" || {
		echo "runtime image did not open the postgres pool" >&2
		printf '%s\n' "${logs}" >&2
		exit 1
	}
	echo "runtime image opened the postgres pool"
fi

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

# The /jobs-worker entrypoint. The repository decides what the image
# must hold. Where the jobs pack is retained, the image carries /jobs-worker, and
# the binary refuses before any database I/O under the hardened flags, with the
# default configuration and no network. Retained webhook profiles register
# their kinds, so the template then refuses because PostgreSQL is disabled;
# without them the source has no registered kind. A derived service may add its
# own registration and can reach either refusal. Where the jobs pack is not
# retained, the image must not carry the entrypoint.
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
jobs=$(python3 "${root}/scripts/lib/template_state.py" profile --repo "${root}" --field jobs) || {
	echo "cannot resolve the selected jobs profile" >&2
	exit 2
}
worker="${container}-jobs-worker"
docker create --name "${worker}" --label "runtime-check=${container}" \
	--read-only \
	--cap-drop=ALL \
	--security-opt=no-new-privileges \
	--network none \
	--entrypoint /jobs-worker \
	"${image}" >/dev/null
has_worker=false
if docker cp "${worker}:/jobs-worker" - >/dev/null 2>&1; then
	has_worker=true
fi
case "${jobs}:${has_worker}" in
none:false)
	echo "jobs pack not retained; the image has no /jobs-worker entrypoint"
	;;
none:true)
	echo "the jobs pack is not retained, but the image carries /jobs-worker" >&2
	exit 1
	;;
postgres:false)
	echo "the jobs pack is retained, but the image has no /jobs-worker entrypoint" >&2
	exit 1
	;;
postgres:true)
	expected='no job kind is registered'
	if [[ -f "${root}/template.lock" ]]; then
		expected='no job kind is registered|postgres\.enabled must be true to run the jobs worker'
	else
		webhooks=$(python3 "${root}/scripts/lib/template_state.py" profile --repo "${root}" --field webhooks)
		inbound_webhooks=$(python3 "${root}/scripts/lib/template_state.py" profile --repo "${root}" --field inbound_webhooks)
		if [[ ${webhooks} != none || ${inbound_webhooks} != none ]]; then
			expected='postgres\.enabled must be true to run the jobs worker'
		fi
	fi
	worker_output=$(docker start --attach "${worker}" 2>&1 || true)
	worker_exit=$(docker inspect -f '{{.State.ExitCode}}' "${worker}")
	refusal=$(grep -Eo "${expected}" <<<"${worker_output}" | head -n 1 || true)
	if [[ ${worker_exit} != 1 || -z ${refusal} ]]; then
		echo "jobs-worker did not give the expected startup refusal (exit ${worker_exit})" >&2
		printf '%s\n' "${worker_output}" >&2
		exit 1
	fi
	echo "jobs-worker refused before database I/O: ${refusal}"
	;;
*)
	echo "unexpected jobs selection: ${jobs}" >&2
	exit 1
	;;
esac
