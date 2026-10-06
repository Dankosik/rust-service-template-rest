#!/usr/bin/env bash
# Object storage adapter proof against a throwaway Compose versitygw. The CI
# integration job can supply a shared, already-running emulator through
# OBJECT_STORAGE_TEST_ENDPOINT; it must hold the bucket `template-bucket`,
# which the Compose service creates.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

if ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --assert-held; then
	exec bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" -- bash "${ROOT_DIR}/scripts/ci/test-integration-object-storage.sh" "$@"
fi

compose_project="service-object-storage-${VALIDATION_LOCK_TOKEN:0:12}-$(date +%s)-$$"
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

if [[ -z ${OBJECT_STORAGE_TEST_ENDPOINT:-} ]]; then
	require_docker
	trap cleanup EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	resource=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-register compose "${compose_project}" \
		--file "${ROOT_DIR}/env/docker-compose.yml")
	if ! VERSITYGW_PORT=0 bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-run "${resource}" -- \
		docker compose -p "${compose_project}" -f env/docker-compose.yml up -d --wait versitygw; then
		VERSITYGW_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml ps --all || true
		VERSITYGW_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml logs --no-color --tail 100 versitygw || true
		container_id=$(VERSITYGW_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml ps --all --quiet versitygw) || true
		if [[ -n ${container_id:-} ]]; then
			docker inspect --format '{{json .State}}' "${container_id}" || true
		fi
		exit 1
	fi
	address=$(VERSITYGW_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml port versitygw 7070)
	port=${address##*:}
	if [[ -z ${port} ]]; then
		echo "failed to resolve the compose versitygw port" >&2
		exit 1
	fi
	OBJECT_STORAGE_TEST_ENDPOINT="http://127.0.0.1:${port}"
fi
# The root credentials of the Compose service; a supplied emulator may override them.
export OBJECT_STORAGE_TEST_ENDPOINT
export OBJECT_STORAGE_TEST_ACCESS_KEY_ID=${OBJECT_STORAGE_TEST_ACCESS_KEY_ID:-template}
export OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY=${OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY:-template-secret}
cargo test --locked -p infra-object-storage --features integration --test emulator "$@"
