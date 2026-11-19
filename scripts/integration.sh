#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${ROOT_DIR}"

unset PGHOSTADDR PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE PGSSLMODE \
	PGSSLROOTCERT PGSSLCERT PGSSLKEY PGAPPNAME PGOPTIONS PGPASSFILE

usage() {
	echo "usage: $0 <db|cache|object-storage|messaging|oauth|--compile-retained|--compile-services> [cargo-args...]" >&2
	exit 2
}

mode=${1:-}
[[ -n ${mode} ]] || usage
shift

require_docker() {
	if docker info >/dev/null 2>&1; then
		return 0
	fi
	if [[ ${REQUIRE_DOCKER:-} == 1 ]]; then
		echo "docker is required (REQUIRE_DOCKER=1) but unavailable" >&2
		exit 1
	fi
	echo "docker is unavailable; set REQUIRE_DOCKER=1 to fail instead of refusing" >&2
	exit 2
}

COMPOSE_PROJECT=""
COMPOSE_LAST_PORT=""

compose_cmd() {
	POSTGRES_PORT=${POSTGRES_PORT:-0} \
	PGBOUNCER_PORT=${PGBOUNCER_PORT:-0} \
	NATS_PORT=${NATS_PORT:-0} \
	VALKEY_PORT=${VALKEY_PORT:-0} \
	VERSITYGW_PORT=${VERSITYGW_PORT:-0} \
		docker compose -p "${COMPOSE_PROJECT}" -f "${ROOT_DIR}/env/docker-compose.yml" "$@"
}

compose_up_service() {
	local service=$1 container_port=$2 timeout_s=${3:-}
	local up_args=(up -d --wait)
	[[ -z ${timeout_s} ]] || up_args+=(--wait-timeout "${timeout_s}")
	up_args+=("${service}")
	if ! compose_cmd "${up_args[@]}"; then
		compose_cmd ps --all >&2 || true
		compose_cmd logs --no-color --tail 100 "${service}" >&2 || true
		return 1
	fi
	local addr port
	addr=$(compose_cmd port "${service}" "${container_port}")
	port=${addr##*:}
	[[ ${port} =~ ^[0-9]+$ ]] || { echo "failed to resolve compose ${service} port" >&2; return 1; }
	COMPOSE_LAST_PORT=${port}
}

compose_down() {
	if [[ -n ${COMPOSE_PROJECT} ]]; then
		compose_cmd down -v --remove-orphans >/dev/null 2>&1 || return
		COMPOSE_PROJECT=""
	fi
}

compose_cleanup() {
	local status=$?
	trap - EXIT INT TERM HUP
	compose_down || { echo "compose cleanup incomplete" >&2; [[ ${status} != 0 ]] || status=1; }
	exit "${status}"
}

is_no_run=false
for arg in "$@"; do
	[[ ${arg} != -- ]] || break
	[[ ${arg} != --no-run ]] || { is_no_run=true; break; }
done

run_compile_services() {
	if [[ -d crates/infra-messaging ]]; then
		cargo test --locked -p infra-messaging --features integration \
			--test jetstream --test idle_pull --test credential_rotation --example dlq_recovery --no-run
		[[ ! -f crates/infra-messaging/tests/wire_compat.rs ]] || \
			cargo test --locked -p infra-messaging --test wire_compat --no-run
	fi
	if [[ -d crates/infra-cache ]]; then
		CACHE_URL=compile-only cargo test --locked -p infra-cache --features integration --test valkey --no-run
	fi
	if [[ -d crates/infra-object-storage ]]; then
		OBJECT_STORAGE_TEST_ENDPOINT=compile-only OBJECT_STORAGE_TEST_ACCESS_KEY_ID=template OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY=template-secret \
			cargo test --locked -p infra-object-storage --features integration --test emulator --no-run
	fi
}

run_compile_retained() {
	if [[ -d crates/infra-postgres ]]; then
		DATABASE_URL=compile-only PGBOUNCER_DATABASE_URL=compile-only \
			cargo test --locked -p integration-tests --features integration --no-run
	fi
	run_compile_services
	if [[ -d crates/infra-oauth2-client-credentials ]]; then
		OAUTH_TEST_KEYCLOAK_URL=compile-only \
			cargo test --locked -p infra-oauth2-client-credentials --features integration tests::keycloak --no-run
	fi
}

run_db() {
	[[ -d crates/infra-postgres ]] || { echo "postgres profile not present" >&2; exit 2; }
	if [[ ${is_no_run} == true ]]; then
		DATABASE_URL=${DATABASE_URL:-compile-only} PGBOUNCER_DATABASE_URL=${PGBOUNCER_DATABASE_URL:-compile-only} \
			exec cargo test --locked -p integration-tests --features integration "$@"
	fi
	require_docker
	trap compose_cleanup EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	trap 'exit 129' HUP
	COMPOSE_PROJECT="service-db-$$-$(date +%s)"
	compose_up_service postgres 5432
	local pg_port=${COMPOSE_LAST_PORT}
	compose_up_service pgbouncer 6432
	local pgb_port=${COMPOSE_LAST_PORT}
	if [[ -d crates/infra-messaging ]]; then
		compose_up_service nats 4222
		export NATS_URL="nats://127.0.0.1:${COMPOSE_LAST_PORT}"
	fi
	export DATABASE_URL="postgres://app:app@127.0.0.1:${pg_port}/app?sslmode=disable"
	export PGBOUNCER_DATABASE_URL="postgres://app:app@127.0.0.1:${pgb_port}/app?sslmode=disable"

	cargo test --locked -p integration-tests --features integration "$@"
	if [[ $# == 0 && -f crates/service/src/bootstrap/inbound_webhooks.rs ]]; then
		cargo test --locked --manifest-path crates/service/Cargo.toml --test lifecycle \
			restarting_with_an_old_inbound_receipt_acknowledges_redelivery_without_another_job -- --ignored --exact
	fi
}

run_cache() {
	[[ -d crates/infra-cache ]] || { echo "cache profile not present" >&2; exit 2; }
	if [[ ${is_no_run} == true ]]; then
		CACHE_URL=${CACHE_URL:-compile-only} exec cargo test --locked -p infra-cache --features integration --test valkey "$@"
	fi
	if [[ -z ${CACHE_URL:-} ]]; then
		require_docker
		trap compose_cleanup EXIT
		trap 'exit 130' INT
		trap 'exit 143' TERM
		trap 'exit 129' HUP
		COMPOSE_PROJECT="service-cache-$$-$(date +%s)"
		compose_up_service valkey 6379
		export CACHE_URL="redis://127.0.0.1:${COMPOSE_LAST_PORT}"
	fi
	cargo test --locked -p infra-cache --features integration --test valkey "$@"
}

run_object_storage() {
	[[ -d crates/infra-object-storage ]] || { echo "object-storage profile not present" >&2; exit 2; }
	if [[ ${is_no_run} == true ]]; then
		OBJECT_STORAGE_TEST_ENDPOINT=${OBJECT_STORAGE_TEST_ENDPOINT:-compile-only} \
		OBJECT_STORAGE_TEST_ACCESS_KEY_ID=${OBJECT_STORAGE_TEST_ACCESS_KEY_ID:-template} \
		OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY=${OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY:-template-secret} \
			exec cargo test --locked -p infra-object-storage --features integration --test emulator "$@"
	fi
	if [[ -z ${OBJECT_STORAGE_TEST_ENDPOINT:-} ]]; then
		require_docker
		trap compose_cleanup EXIT
		trap 'exit 130' INT
		trap 'exit 143' TERM
		trap 'exit 129' HUP
		COMPOSE_PROJECT="service-s3-$$-$(date +%s)"
		compose_up_service versitygw 7070
		export OBJECT_STORAGE_TEST_ENDPOINT="http://127.0.0.1:${COMPOSE_LAST_PORT}"
	fi
	export OBJECT_STORAGE_TEST_ACCESS_KEY_ID=${OBJECT_STORAGE_TEST_ACCESS_KEY_ID:-template}
	export OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY=${OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY:-template-secret}
	cargo test --locked -p infra-object-storage --features integration --test emulator "$@"
}

run_messaging() {
	[[ -d crates/infra-messaging ]] || { echo "messaging profile not present" >&2; exit 2; }
	if [[ ${is_no_run} == true ]]; then
		exec cargo test --locked -p infra-messaging --features integration \
			--test jetstream --test idle_pull --test credential_rotation --example dlq_recovery "$@"
	fi
	require_docker

	local base
	base="service-nats-$$-$(date +%s)"
	NATS_WORK=$(mktemp -d "${TMPDIR:-/tmp}/service-nats.XXXXXX")
	NATS_STATUS=0
	local work=${NATS_WORK} nats_cfg=""
	# shellcheck disable=SC2329
	cleanup_nats() {
		local rc=$?
		trap - EXIT INT TERM HUP
		[[ ${NATS_STATUS:-0} == 0 ]] || rc=${NATS_STATUS}
		compose_down || [[ ${rc} != 0 ]] || rc=1
		[[ -z ${NATS_WORK:-} ]] || rm -rf -- "${NATS_WORK}"
		exit "${rc}"
	}
	trap cleanup_nats EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	trap 'exit 129' HUP

	start_nats() {
		compose_down
		nats_cfg=$1
		COMPOSE_PROJECT="${base}-$2"
		NATS_PORT=127.0.0.1:0 NATS_CONFIG_FILE="${nats_cfg}" compose_up_service nats 4222 30
		OWNED_NATS_URL="nats://127.0.0.1:${COMPOSE_LAST_PORT}"
	}

	local ordinary_url=${NATS_URL:-}
	if [[ -z ${ordinary_url} ]]; then
		start_nats "${ROOT_DIR}/env/nats/nats-server.conf" ordinary
		ordinary_url=${OWNED_NATS_URL}
	fi

	cargo test --locked -p infra-messaging --features integration --test jetstream --test idle_pull --example dlq_recovery --no-run
	NATS_URL="${ordinary_url}" cargo test --locked -p infra-messaging --features integration --test idle_pull "$@" >"${work}/idle.log" 2>&1 &
	local idle_pid=$! idle_rc=0
	NATS_URL="${ordinary_url}" cargo test --locked -p infra-messaging --features integration --test jetstream --example dlq_recovery "$@" || NATS_STATUS=$?
	wait "${idle_pid}" || idle_rc=$?
	cat "${work}/idle.log"
	[[ ${NATS_STATUS} != 0 ]] || NATS_STATUS=${idle_rc}

	if [[ ${NATS_STATUS} == 0 ]]; then
		[[ -n ${COMPOSE_PROJECT} ]] || start_nats "${ROOT_DIR}/env/nats/nats-server.conf" cursor
		NATS_URL="${OWNED_NATS_URL}" NATS_CURSOR_RECEIPT="${work}/cursor.json" \
			cargo test --locked -p infra-messaging --features integration --test jetstream \
			connection::save_ack_floor_before_restart -- --ignored --exact &&
		NATS_PORT=127.0.0.1:0 NATS_CONFIG_FILE="${nats_cfg}" compose_cmd restart --timeout 10 nats &&
		NATS_PORT=127.0.0.1:0 NATS_CONFIG_FILE="${nats_cfg}" compose_up_service nats 4222 30 &&
		NATS_URL="nats://127.0.0.1:${COMPOSE_LAST_PORT}" NATS_CURSOR_RECEIPT="${work}/cursor.json" \
			cargo test --locked -p infra-messaging --features integration --test jetstream \
			connection::read_ack_floor_after_restart -- --ignored --exact || NATS_STATUS=$?
	fi

	local fixture_dir="${ROOT_DIR}/env/nats/auth-rotation"
	start_nats "${fixture_dir}/nats-server.conf" auth
	NATS_AUTH_URL="${OWNED_NATS_URL}" NATS_AUTH_CREDS_A="${fixture_dir}/user-a.creds" NATS_AUTH_CREDS_B="${fixture_dir}/user-b.creds" \
		cargo test --locked -p infra-messaging --features integration --test jetstream \
		credentials::credentials_file_rotation_is_authenticated_by_the_broker -- --ignored --exact --no-capture || [[ ${NATS_STATUS} != 0 ]] || NATS_STATUS=$?

	start_nats "${ROOT_DIR}/env/nats/credential-rotation.conf" cred-rot
	NATS_AUTH_URL="${OWNED_NATS_URL}" cargo test --locked -p infra-messaging --features integration \
		--test credential_rotation expired_old_credentials_are_refused_and_file_replacement_recovers_the_client \
		-- --exact --no-capture || [[ ${NATS_STATUS} != 0 ]] || NATS_STATUS=$?

	exit "${NATS_STATUS}"
}

run_oauth() {
	[[ -d crates/infra-oauth2-client-credentials ]] || { echo "oauth profile not present" >&2; exit 2; }
	if [[ ${is_no_run} == true ]]; then
		OAUTH_TEST_KEYCLOAK_URL=${OAUTH_TEST_KEYCLOAK_URL:-compile-only} \
			exec cargo test --locked -p infra-oauth2-client-credentials --features integration tests::keycloak "$@"
	fi
	KC_CONTAINER=""
	# shellcheck disable=SC2329
	cleanup_kc() {
		local rc=$?
		trap - EXIT INT TERM HUP
		[[ -z ${KC_CONTAINER:-} ]] || docker rm -f -v "${KC_CONTAINER}" >/dev/null 2>&1 || true
		exit "${rc}"
	}
	if [[ -z ${OAUTH_TEST_KEYCLOAK_URL:-} ]]; then
		# shellcheck source=tools/versions.env
		. tools/versions.env
		require_docker
		trap cleanup_kc EXIT
		trap 'exit 130' INT
		trap 'exit 143' TERM
		trap 'exit 129' HUP
		KC_CONTAINER=$(docker run -d --name "service-oauth-$$-$(date +%s)" -p 127.0.0.1::8080 \
			-e KC_BOOTSTRAP_ADMIN_USERNAME=admin -e KC_BOOTSTRAP_ADMIN_PASSWORD=admin \
			"${KEYCLOAK_IMAGE}" start-dev)
		local addr port ready=false
		addr=$(docker port "${KC_CONTAINER}" 8080/tcp)
		port=${addr##*:}
		export OAUTH_TEST_KEYCLOAK_URL="http://127.0.0.1:${port}"
		for _ in {1..180}; do
			if curl -fs --max-time 5 "${OAUTH_TEST_KEYCLOAK_URL}/realms/master" >/dev/null 2>&1; then
				ready=true
				break
			fi
			[[ $(docker inspect -f '{{.State.Running}}' "${KC_CONTAINER}") == true ]] || break
			sleep 1
		done
		[[ ${ready} == true ]] || { docker logs --tail 100 "${KC_CONTAINER}" >&2; echo "Keycloak not ready" >&2; exit 1; }
	fi
	cargo test --locked -p infra-oauth2-client-credentials --features integration tests::keycloak "$@"
}

case "${mode}" in
--compile-retained) run_compile_retained ;;
--compile-services) run_compile_services ;;
db) run_db "$@" ;;
cache) run_cache "$@" ;;
object-storage) run_object_storage "$@" ;;
messaging) run_messaging "$@" ;;
oauth) run_oauth "$@" ;;
*) usage ;;
esac
