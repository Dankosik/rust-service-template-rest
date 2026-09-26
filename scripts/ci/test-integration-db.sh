#!/usr/bin/env bash
# Database-backed proof: PostgreSQL from env/docker-compose.yml on an
# ephemeral port, plus NATS when the selected profile needs outbox publication.
# The integration CI job can instead own a shared Compose lifecycle and provide
# DATABASE_URL and NATS_URL itself. Extra arguments go to `cargo test`.
#
#   test-integration-db.sh [cargo test args]
#   REQUIRE_DOCKER=1 makes a missing Docker a failure instead of a refusal.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"
# shellcheck source=scripts/lib/compose-postgres.sh
source scripts/lib/compose-postgres.sh

if [[ ${INTEGRATION_COMPOSE_MANAGED:-} != 1 ]]; then
	require_docker
	trap compose_postgres_down EXIT INT TERM
	compose_postgres_up service-db
	if [[ $(python3 scripts/lib/template_state.py profile --repo . --field messaging) == nats-jetstream ]]; then
		if ! NATS_PORT=0 compose_postgres up -d --wait nats; then
			NATS_PORT=0 compose_postgres ps --all || true
			NATS_PORT=0 compose_postgres logs --no-color --tail 100 nats || true
			container_id=$(NATS_PORT=0 compose_postgres ps --all --quiet nats) || true
			if [[ -n ${container_id:-} ]]; then
				docker inspect --format '{{json .State}}' "${container_id}" || true
			fi
			exit 1
		fi
		address=$(NATS_PORT=0 compose_postgres port nats 4222)
		port=${address##*:}
		[[ -n ${port} ]] || { echo "failed to resolve the compose NATS port" >&2; exit 1; }
		NATS_URL="nats://127.0.0.1:${port}"
		export NATS_URL
	fi
	DATABASE_URL=$(compose_postgres_host_dsn)
	export DATABASE_URL
elif [[ -z ${DATABASE_URL:-} ]]; then
	echo "INTEGRATION_COMPOSE_MANAGED=1 requires DATABASE_URL" >&2
	exit 2
fi

cargo test --locked -p integration-tests --features integration "$@"
