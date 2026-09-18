#!/usr/bin/env bash
# Lifecycle check of a built runtime image, observed from outside the
# container because the distroless base has no shell or curl: the service
# starts under the hardened run flags, /health/ready answers, the startup
# record carries the expected commit, and SIGTERM ends the process with exit
# code 0 inside the 45 s grace budget (docs/configuration-source-policy.md).
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
	docker rm -f "${container}" >/dev/null 2>&1 || true
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
