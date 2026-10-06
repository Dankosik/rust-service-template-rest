#!/usr/bin/env bash
# JetStream adapter proof against a throwaway Compose NATS. The CI integration
# job can supply a shared, already-running broker through NATS_URL.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

if ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --assert-held; then
	exec bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" -- bash "${ROOT_DIR}/scripts/ci/test-integration-messaging.sh" "$@"
fi

compose_project="service-messaging-${VALIDATION_LOCK_TOKEN:0:12}-$(date +%s)-$$"
resource=

require_docker() {
	if docker info >/dev/null 2>&1; then
		return 0
	fi
	if [[ ${REQUIRE_DOCKER:-} == 1 ]]; then
		echo "docker is required (REQUIRE_DOCKER=1) but not available" >&2
		exit 1
	fi
	echo "refusing: docker is not available; set REQUIRE_DOCKER=1 to fail instead of refusing" >&2
	exit 2
}

cleanup() {
	local status=$?
	trap - EXIT INT TERM
	if [[ -n ${resource} ]]; then
		if ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-cleanup "${resource}"; then
			echo "validation resource cleanup incomplete: ${resource}" >&2
			if [[ ${status} == 0 ]]; then status=1; fi
		fi
	fi
	exit "${status}"
}

if [[ -z ${NATS_URL:-} ]]; then
	require_docker
	trap cleanup EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	resource=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-register compose "${compose_project}" \
		--file "${ROOT_DIR}/env/docker-compose.yml")
	if ! NATS_PORT=0 bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-run "${resource}" -- \
		docker compose -p "${compose_project}" -f env/docker-compose.yml up -d --wait nats; then
		NATS_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml ps --all || true
		NATS_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml logs --no-color --tail 100 nats || true
		container_id=$(NATS_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml ps --all --quiet nats) || true
		if [[ -n ${container_id:-} ]]; then
			docker inspect --format '{{json .State}}' "${container_id}" || true
		fi
		exit 1
	fi
	address=$(NATS_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml port nats 4222)
	port=${address##*:}
	if [[ -z ${port} ]]; then
		echo "failed to resolve the compose NATS port" >&2
		exit 1
	fi
	NATS_URL="nats://127.0.0.1:${port}"
fi
export NATS_URL
cargo test --locked -p infra-messaging --features integration --test jetstream --test idle_pull "$@"
