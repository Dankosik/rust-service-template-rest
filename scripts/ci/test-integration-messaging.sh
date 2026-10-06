#!/usr/bin/env bash
# JetStream adapter proof against a throwaway Compose NATS. The CI integration
# job can supply a shared, already-running broker through NATS_URL.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

compose_project="service-messaging-$(date +%s)-$$-${RANDOM}"
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
		if ! NATS_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml down -v --remove-orphans >/dev/null 2>&1; then
			cleanup_failed=true
		elif ! remaining=$(docker ps --all --quiet --filter "label=com.docker.compose.project=${compose_project}"); then
			cleanup_failed=true
		elif [[ -n ${remaining} || ${submission_confirmed} != true ]]; then
			cleanup_failed=true
		elif ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-complete integration-messaging "${ticket}" compose-absent "${compose_project}"; then
			cleanup_failed=true
		fi
	fi
	if [[ ${cleanup_failed} == true ]]; then
		echo "messaging Compose cleanup incomplete" >&2
		if [[ ${status} == 0 ]]; then status=1; fi
	fi
	exit "${status}"
}

if [[ -z ${NATS_URL:-} ]]; then
	require_docker
	trap cleanup EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	trap 'exit 129' HUP
	ticket=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-begin integration-messaging "${compose_project}")
	if ! NATS_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml up -d --wait nats; then
		NATS_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml ps --all || true
		NATS_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml logs --no-color --tail 100 nats || true
		container_id=$(NATS_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml ps --all --quiet nats) || true
		if [[ -n ${container_id:-} ]]; then
			docker inspect --format '{{json .State}}' "${container_id}" || true
		fi
		exit 1
	fi
	submission_confirmed=true
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
