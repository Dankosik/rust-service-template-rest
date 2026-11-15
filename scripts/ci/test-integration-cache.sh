#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

if ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --assert-held; then
	exec bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" -- bash "${ROOT_DIR}/scripts/ci/test-integration-cache.sh" "$@"
fi

compose_project="service-cache-${VALIDATION_LOCK_TOKEN:0:12}-$(date +%s)-$$"
started_compose=false

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
	if [[ ${started_compose} == true ]]; then
		VALKEY_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml down -v --remove-orphans >/dev/null 2>&1 || true
	fi
	exit "${status}"
}

if [[ -z ${CACHE_URL:-} ]]; then
	require_docker
	trap cleanup EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	trap 'exit 129' HUP
	started_compose=true
	if ! VALKEY_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml up -d --wait valkey; then
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
