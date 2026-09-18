#!/usr/bin/env bash
# Shared by the database-backed proof scripts: one throwaway compose project
# per run from env/docker-compose.yml, an ephemeral host port, and a clean
# environment for the DSN policy.
#
#   source scripts/lib/compose-postgres.sh
#   require_docker            # exit 1 under REQUIRE_DOCKER=1, else refuse with 2
#   compose_postgres_up       # sets COMPOSE_PROJECT, COMPOSE_NETWORK, POSTGRES_HOST_PORT
#   trap compose_postgres_down EXIT INT TERM
#
# Callers own `set -euo pipefail` and the repository root as working directory.

# The DSN policy refuses ambient libpq variables; a developer shell may carry
# them from other tooling, and a proof must not depend on that.
unset PGHOSTADDR PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE PGSSLMODE \
	PGSSLROOTCERT PGSSLCERT PGSSLKEY PGAPPNAME PGOPTIONS PGPASSFILE

# Local compose credentials; the compose file owns the same three values.
COMPOSE_POSTGRES_USER=app
COMPOSE_POSTGRES_PASSWORD=app
COMPOSE_POSTGRES_DB=app

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

compose_postgres() {
	POSTGRES_PORT=0 docker compose -p "${COMPOSE_PROJECT}" -f env/docker-compose.yml "$@"
}

compose_postgres_up() {
	COMPOSE_PROJECT="${1:-service-postgres}-$(date +%s)-$$"
	COMPOSE_NETWORK="${COMPOSE_PROJECT}_default"
	compose_postgres up -d --wait postgres
	local address
	address=$(compose_postgres port postgres 5432)
	POSTGRES_HOST_PORT=${address##*:}
	if [[ -z ${POSTGRES_HOST_PORT} ]]; then
		echo "failed to resolve the compose PostgreSQL port" >&2
		exit 1
	fi
	export COMPOSE_PROJECT COMPOSE_NETWORK POSTGRES_HOST_PORT
}

compose_postgres_down() {
	if [[ -n ${COMPOSE_PROJECT:-} ]]; then
		compose_postgres down -v --remove-orphans >/dev/null 2>&1 || true
	fi
}

# DSN as seen from the host (ephemeral published port).
compose_postgres_host_dsn() {
	printf 'postgres://%s:%s@127.0.0.1:%s/%s?sslmode=disable' \
		"${COMPOSE_POSTGRES_USER}" "${COMPOSE_POSTGRES_PASSWORD}" "${POSTGRES_HOST_PORT}" "${COMPOSE_POSTGRES_DB}"
}

# DSN as seen from a container on the compose network.
compose_postgres_network_dsn() {
	printf 'postgres://%s:%s@postgres:5432/%s?sslmode=disable' \
		"${COMPOSE_POSTGRES_USER}" "${COMPOSE_POSTGRES_PASSWORD}" "${COMPOSE_POSTGRES_DB}"
}
