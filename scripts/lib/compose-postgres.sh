#!/usr/bin/env bash

# The DSN policy refuses ambient libpq variables; a developer shell may carry
# them from other tooling, and a proof must not depend on that.
unset PGHOSTADDR PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE PGSSLMODE \
	PGSSLROOTCERT PGSSLCERT PGSSLKEY PGAPPNAME PGOPTIONS PGPASSFILE

# Local compose credentials; the compose file owns the same three values.
COMPOSE_POSTGRES_USER=app
COMPOSE_POSTGRES_PASSWORD=app
COMPOSE_POSTGRES_DB=app
COMPOSE_POSTGRES_OVERRIDE=
COMPOSE_PROJECT=

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
	local files=(-f env/docker-compose.yml)
	if [[ -n ${COMPOSE_POSTGRES_OVERRIDE} ]]; then
		files+=(-f "${COMPOSE_POSTGRES_OVERRIDE}")
	fi
	POSTGRES_PORT=0 PGBOUNCER_PORT=0 docker compose -p "${COMPOSE_PROJECT}" "${files[@]}" "$@"
}

compose_postgres_up() {
	local prefix=${1:-service-postgres}
	if (($#)); then shift; fi
	COMPOSE_POSTGRES_OVERRIDE=
	if (($#)); then
		if [[ $# != 2 || $1 != --override-file || $2 != /* || ! -f $2 ]]; then
			echo "compose PostgreSQL override requires --override-file ABSOLUTE_FILE" >&2
			return 2
		fi
		COMPOSE_POSTGRES_OVERRIDE=$2
	fi
	COMPOSE_PROJECT="${prefix}-${VALIDATION_LOCK_TOKEN:0:12}-$(date +%s)-$$"
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
		compose_postgres down -v --remove-orphans >/dev/null 2>&1 || return
		COMPOSE_PROJECT=
	fi
}

compose_postgres_cleanup() {
	local status=$?
	trap - EXIT INT TERM HUP
	if ! compose_postgres_down; then
		echo "validation PostgreSQL cleanup incomplete" >&2
		if [[ ${status} == 0 ]]; then status=1; fi
	fi
	exit "${status}"
}

compose_postgres_host_dsn() {
	printf 'postgres://%s:%s@127.0.0.1:%s/%s?sslmode=disable' \
		"${COMPOSE_POSTGRES_USER}" "${COMPOSE_POSTGRES_PASSWORD}" "${POSTGRES_HOST_PORT}" "${COMPOSE_POSTGRES_DB}"
}

compose_pgbouncer_up() {
	compose_postgres up -d --wait pgbouncer
	local address
	address=$(compose_postgres port pgbouncer 6432)
	PGBOUNCER_HOST_PORT=${address##*:}
	if [[ -z ${PGBOUNCER_HOST_PORT} ]]; then
		echo "failed to resolve the compose PgBouncer port" >&2
		exit 1
	fi
	export PGBOUNCER_HOST_PORT
}

compose_pgbouncer_host_dsn() {
	printf 'postgres://%s:%s@127.0.0.1:%s/%s?sslmode=disable' \
		"${COMPOSE_POSTGRES_USER}" "${COMPOSE_POSTGRES_PASSWORD}" "${PGBOUNCER_HOST_PORT}" "${COMPOSE_POSTGRES_DB}"
}

compose_postgres_network_dsn() {
	printf 'postgres://%s:%s@postgres:5432/%s?sslmode=disable' \
		"${COMPOSE_POSTGRES_USER}" "${COMPOSE_POSTGRES_PASSWORD}" "${COMPOSE_POSTGRES_DB}"
}
