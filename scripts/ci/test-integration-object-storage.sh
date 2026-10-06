#!/usr/bin/env bash
# Object storage adapter proof against a throwaway Compose versitygw. The CI
# integration job can supply a shared, already-running emulator through
# OBJECT_STORAGE_TEST_ENDPOINT; it must hold the bucket `template-bucket`,
# which the Compose service creates.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

compose_project="service-object-storage-$(date +%s)-$$-${RANDOM}"
ticket=
submission_confirmed=false

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
	local status=$? remaining cleanup_failed=false
	trap - EXIT INT TERM HUP
	if [[ -n ${ticket} ]]; then
		if ! VERSITYGW_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml down -v --remove-orphans >/dev/null 2>&1; then
			cleanup_failed=true
		elif ! remaining=$(docker ps --all --quiet --filter "label=com.docker.compose.project=${compose_project}"); then
			cleanup_failed=true
		elif [[ -n ${remaining} || ${submission_confirmed} != true ]]; then
			cleanup_failed=true
		elif ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-complete integration-object-storage "${ticket}" compose-absent "${compose_project}"; then
			cleanup_failed=true
		fi
	fi
	if [[ ${cleanup_failed} == true ]]; then
		echo "object-storage Compose cleanup incomplete" >&2
		if [[ ${status} == 0 ]]; then status=1; fi
	fi
	exit "${status}"
}

if [[ -z ${OBJECT_STORAGE_TEST_ENDPOINT:-} ]]; then
	require_docker
	trap cleanup EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	trap 'exit 129' HUP
	ticket=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-begin integration-object-storage "${compose_project}")
	if ! VERSITYGW_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml up -d --wait versitygw; then
		VERSITYGW_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml ps --all || true
		VERSITYGW_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml logs --no-color --tail 100 versitygw || true
		container_id=$(VERSITYGW_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml ps --all --quiet versitygw) || true
		if [[ -n ${container_id:-} ]]; then
			docker inspect --format '{{json .State}}' "${container_id}" || true
		fi
		exit 1
	fi
	submission_confirmed=true
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
