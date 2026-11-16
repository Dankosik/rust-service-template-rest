#!/usr/bin/env bash

# The DSN policy refuses ambient libpq variables; a developer shell may carry
# them from other tooling, and a proof must not depend on that.
unset PGHOSTADDR PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE PGSSLMODE \
	PGSSLROOTCERT PGSSLCERT PGSSLKEY PGAPPNAME PGOPTIONS PGPASSFILE

# Local compose credentials; the compose file owns the same three values.
COMPOSE_POSTGRES_USER=app
COMPOSE_POSTGRES_PASSWORD=app
COMPOSE_POSTGRES_DB=app
COMPOSE_PROJECT=${COMPOSE_PROJECT:-}
COMPOSE_NETWORK=${COMPOSE_NETWORK:-}
COMPOSE_LAST_PORT=

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

compose_cmd() {
	POSTGRES_PORT=${POSTGRES_PORT:-0} \
	PGBOUNCER_PORT=${PGBOUNCER_PORT:-0} \
	NATS_PORT=${NATS_PORT:-0} \
	VALKEY_PORT=${VALKEY_PORT:-0} \
	VERSITYGW_PORT=${VERSITYGW_PORT:-0} \
		docker compose -p "${COMPOSE_PROJECT}" -f "${ROOT_DIR:-.}/env/docker-compose.yml" "$@"
}

compose_init_project() {
	local prefix=${1:-service} lock_token=${VALIDATION_LOCK_TOKEN:-nolock}
	COMPOSE_PROJECT="${prefix}-${lock_token:0:12}-$(date +%s)-$$"
	COMPOSE_NETWORK="${COMPOSE_PROJECT}_default"
	export COMPOSE_PROJECT COMPOSE_NETWORK
}

compose_up_service() {
	local service=$1 container_port=$2 wait_timeout=${3:-}
	local up_args=(up -d --wait)
	if [[ -n ${wait_timeout} ]]; then
		up_args+=(--wait-timeout "${wait_timeout}")
	fi
	up_args+=("${service}")
	if ! compose_cmd "${up_args[@]}"; then
		compose_cmd ps --all >&2 || true
		compose_cmd logs --no-color --tail 100 "${service}" >&2 || true
		local container_id
		container_id=$(compose_cmd ps --all --quiet "${service}") || true
		if [[ -n ${container_id:-} ]]; then
			docker inspect --format '{{json .State}}' "${container_id}" >&2 || true
		fi
		return 1
	fi
	local address port
	address=$(compose_cmd port "${service}" "${container_port}")
	port=${address##*:}
	if [[ ! ${port} =~ ^[0-9]+$ ]]; then
		echo "failed to resolve the compose ${service} port" >&2
		return 1
	fi
	COMPOSE_LAST_PORT=${port}
}

compose_postgres_up() {
	local prefix=${1:-service-postgres}
	compose_init_project "${prefix}"
	compose_up_service postgres 5432
	POSTGRES_HOST_PORT=${COMPOSE_LAST_PORT}
	export POSTGRES_HOST_PORT
}

compose_pgbouncer_up() {
	compose_up_service pgbouncer 6432
	PGBOUNCER_HOST_PORT=${COMPOSE_LAST_PORT}
	export PGBOUNCER_HOST_PORT
}

compose_down() {
	if [[ -n ${COMPOSE_PROJECT:-} ]]; then
		compose_cmd down -v --remove-orphans >/dev/null 2>&1 || return
		COMPOSE_PROJECT=
	fi
}

compose_cleanup() {
	local status=$?
	trap - EXIT INT TERM HUP
	if ! compose_down; then
		echo "validation Compose cleanup incomplete" >&2
		if [[ ${status} == 0 ]]; then status=1; fi
	fi
	exit "${status}"
}

compose_postgres_host_dsn() {
	printf 'postgres://%s:%s@127.0.0.1:%s/%s?sslmode=disable' \
		"${COMPOSE_POSTGRES_USER}" "${COMPOSE_POSTGRES_PASSWORD}" "${POSTGRES_HOST_PORT}" "${COMPOSE_POSTGRES_DB}"
}

compose_pgbouncer_host_dsn() {
	printf 'postgres://%s:%s@127.0.0.1:%s/%s?sslmode=disable' \
		"${COMPOSE_POSTGRES_USER}" "${COMPOSE_POSTGRES_PASSWORD}" "${PGBOUNCER_HOST_PORT}" "${COMPOSE_POSTGRES_DB}"
}

compose_postgres_network_dsn() {
	printf 'postgres://%s:%s@postgres:5432/%s?sslmode=disable' \
		"${COMPOSE_POSTGRES_USER}" "${COMPOSE_POSTGRES_PASSWORD}" "${COMPOSE_POSTGRES_DB}"
}
