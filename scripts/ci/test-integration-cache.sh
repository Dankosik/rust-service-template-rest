#!/usr/bin/env bash
# Cache adapter proof against a throwaway Compose Valkey. The CI integration
# job can supply a shared, already-running server through CACHE_URL.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

if ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --assert-held; then
	exec bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" -- bash "${ROOT_DIR}/scripts/ci/test-integration-cache.sh" "$@"
fi

compose_project="service-cache-${VALIDATION_LOCK_TOKEN:0:12}-$(date +%s)-$$"
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
	trap - EXIT INT TERM HUP
	if [[ -n ${resource} ]]; then
		if ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-cleanup "${resource}"; then
			echo "validation resource cleanup incomplete: ${resource}" >&2
			if [[ ${status} == 0 ]]; then status=1; fi
		fi
	fi
	exit "${status}"
}

if [[ -z ${CACHE_URL:-} ]]; then
	require_docker
	trap cleanup EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	trap 'exit 129' HUP
	resource=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-register compose "${compose_project}" \
		--file "${ROOT_DIR}/env/docker-compose.yml")
	if ! VALKEY_PORT=0 bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-run "${resource}" -- \
		docker compose -p "${compose_project}" -f env/docker-compose.yml up -d --wait valkey; then
		VALKEY_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml ps --all || true
		VALKEY_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml logs --no-color --tail 100 valkey || true
		container_id=$(VALKEY_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml ps --all --quiet valkey) || true
		if [[ -n ${container_id:-} ]]; then
			docker inspect --format '{{json .State}}' "${container_id}" || true
		fi
		exit 1
	fi
	address=$(VALKEY_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml port valkey 6379)
	port=${address##*:}
	if [[ -z ${port} ]]; then
		echo "failed to resolve the compose Valkey port" >&2
		exit 1
	fi
	CACHE_URL="redis://127.0.0.1:${port}"
fi
export CACHE_URL
cargo test --locked -p infra-cache --features integration --test valkey "$@"
