#!/usr/bin/env bash
# Shared by the database-backed proof scripts: one throwaway compose project
# per run from env/docker-compose.yml, an ephemeral host port, and a clean
# environment for the DSN policy.
#
#   source scripts/lib/compose-postgres.sh
#   require_docker            # exit 1 under REQUIRE_DOCKER=1, else refuse with 2
#   compose_postgres_up       # sets COMPOSE_PROJECT, COMPOSE_NETWORK, POSTGRES_HOST_PORT
#   compose_pgbouncer_up      # optional; sets PGBOUNCER_HOST_PORT
#   trap compose_postgres_exit EXIT; trap 'exit 130' INT; trap 'exit 143' TERM
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
# Only a successful ticket admission grants this shell cleanup ownership.
COMPOSE_POSTGRES_TICKET=
COMPOSE_POSTGRES_SUBMISSIONS_CONFIRMED=true

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
	local confirmed=${COMPOSE_POSTGRES_SUBMISSIONS_CONFIRMED} status
	# A later successful up cannot settle an earlier interrupted submission.
	if [[ ${1:-} == up ]]; then COMPOSE_POSTGRES_SUBMISSIONS_CONFIRMED=false; fi
	if POSTGRES_PORT=0 PGBOUNCER_PORT=0 docker compose -p "${COMPOSE_PROJECT}" -f env/docker-compose.yml "$@"; then
		if [[ ${1:-} == up ]]; then COMPOSE_POSTGRES_SUBMISSIONS_CONFIRMED=${confirmed}; fi
	else
		status=$?
		return "${status}"
	fi
}

compose_postgres_up() {
	COMPOSE_PROJECT="${1:-service-postgres}-$(date +%s)-$$-${RANDOM}"
	COMPOSE_POSTGRES_TICKET=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-begin compose-postgres "${COMPOSE_PROJECT}") || return $?
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
	local remaining
	if [[ -n ${COMPOSE_POSTGRES_TICKET} ]]; then
		compose_postgres down -v --remove-orphans >/dev/null 2>&1 || return 1
		remaining=$(docker ps --all --quiet --filter "label=com.docker.compose.project=${COMPOSE_PROJECT}") || return 1
		[[ -z ${remaining} ]] || return 1
		# Present absence does not settle an unknown daemon admission.
		[[ ${COMPOSE_POSTGRES_SUBMISSIONS_CONFIRMED} == true ]] || return 1
		bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-complete compose-postgres "${COMPOSE_POSTGRES_TICKET}" compose-absent "${COMPOSE_PROJECT}" || return 1
		COMPOSE_POSTGRES_TICKET=
	fi
}

compose_postgres_exit() {
	local status=$?
	trap - EXIT INT TERM HUP
	if ! compose_postgres_down; then
		echo "PostgreSQL Compose cleanup incomplete" >&2
		if [[ ${status} == 0 ]]; then status=1; fi
	fi
	exit "${status}"
}

# DSN as seen from the host (ephemeral published port).
compose_postgres_host_dsn() {
	printf 'postgres://%s:%s@127.0.0.1:%s/%s?sslmode=disable' \
		"${COMPOSE_POSTGRES_USER}" "${COMPOSE_POSTGRES_PASSWORD}" "${POSTGRES_HOST_PORT}" "${COMPOSE_POSTGRES_DB}"
}

# PgBouncer in front of the running PostgreSQL; sets PGBOUNCER_HOST_PORT.
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

# DSN of the same database through PgBouncer, as seen from the host.
compose_pgbouncer_host_dsn() {
	printf 'postgres://%s:%s@127.0.0.1:%s/%s?sslmode=disable' \
		"${COMPOSE_POSTGRES_USER}" "${COMPOSE_POSTGRES_PASSWORD}" "${PGBOUNCER_HOST_PORT}" "${COMPOSE_POSTGRES_DB}"
}

# DSN as seen from a container on the compose network.
compose_postgres_network_dsn() {
	printf 'postgres://%s:%s@postgres:5432/%s?sslmode=disable' \
		"${COMPOSE_POSTGRES_USER}" "${COMPOSE_POSTGRES_PASSWORD}" "${COMPOSE_POSTGRES_DB}"
}
