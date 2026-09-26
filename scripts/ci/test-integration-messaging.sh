#!/usr/bin/env bash
# JetStream adapter proof against a throwaway Compose NATS. The CI integration
# job can supply a shared, already-running broker through NATS_URL.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

compose_project="service-messaging-$(date +%s)-$$"
started=false

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
	if [[ ${started} == true ]]; then
		NATS_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml down -v --remove-orphans >/dev/null 2>&1 || true
	fi
}

if [[ -z ${NATS_URL:-} ]]; then
	require_docker
	trap cleanup EXIT INT TERM
	NATS_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml up -d --wait nats
	address=$(NATS_PORT=0 docker compose -p "${compose_project}" -f env/docker-compose.yml port nats 4222)
	port=${address##*:}
	if [[ -z ${port} ]]; then
		echo "failed to resolve the compose NATS port" >&2
		exit 1
	fi
	NATS_URL="nats://127.0.0.1:${port}"
	started=true
fi
export NATS_URL
cargo test --locked -p infra-messaging --test jetstream "$@"
