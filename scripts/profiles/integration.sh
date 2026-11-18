#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

usage() {
	echo "usage: integration.sh <db|cache|object-storage|messaging|oauth|--compile-retained|--compile-services> [cargo-args...]" >&2
	exit 2
}

mode=${1:-}
[[ -n ${mode} ]] || usage
shift

case ${mode} in
--compile-retained | --compile-services)
	[[ $# == 0 ]] || usage
	;;
db)
	[[ -d crates/infra-postgres ]] || { echo "integration: postgres profile is not present" >&2; exit 2; }
	;;
cache)
	[[ -d crates/infra-cache ]] || { echo "integration: cache profile is not present" >&2; exit 2; }
	;;
object-storage)
	[[ -d crates/infra-object-storage ]] || { echo "integration: object-storage profile is not present" >&2; exit 2; }
	;;
messaging)
	[[ -d crates/infra-messaging ]] || { echo "integration: messaging profile is not present" >&2; exit 2; }
	;;
oauth)
	[[ -d crates/infra-oauth2-client-credentials ]] || { echo "integration: oauth profile is not present" >&2; exit 2; }
	;;
*)
	usage
	;;
esac

if ! python3 "${ROOT_DIR}/scripts/verify/lock.py" --assert-held; then
	exec python3 "${ROOT_DIR}/scripts/verify/lock.py" -- bash "${ROOT_DIR}/scripts/profiles/integration.sh" "${mode}" "$@"
fi

# OAuth-only derived repositories prune scripts/profiles/compose.sh alongside
# env/docker-compose.yml while keeping scripts/profiles/integration.sh.
if [[ -f "${ROOT_DIR}/scripts/profiles/compose.sh" ]]; then
	# shellcheck source=/dev/null
	source "${ROOT_DIR}/scripts/profiles/compose.sh"
fi

if ! declare -F require_docker >/dev/null; then
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
fi

if ! declare -F wait_http_ready >/dev/null; then
	wait_http_ready() {
		local url=$1 timeout_seconds=$2 container_id=${3:-}
		python3 - "${timeout_seconds}" "${url}" "${container_id}" <<'PY'
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request

deadline = time.monotonic() + float(sys.argv[1])
url = sys.argv[2]
container = sys.argv[3]

for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
    signal.signal(sig, lambda signum, _frame: sys.exit(128 + signum))

opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

while True:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise SystemExit(1)
    try:
        with opener.open(url, timeout=min(5.0, remaining)) as response:
            if 200 <= response.status < 400:
                break
    except (urllib.error.URLError, TimeoutError, OSError):
        pass
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise SystemExit(1)
    if container:
        probe = subprocess.run(
            ["docker", "inspect", "--format", "{{.State.Running}}", container],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=min(5.0, remaining),
            check=False,
        )
        if probe.returncode != 0 or probe.stdout.strip() != b"true":
            raise SystemExit(1)
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise SystemExit(1)
    time.sleep(min(1.0, remaining))
PY
	}
fi

is_no_run=false
for argument in "$@"; do
	[[ ${argument} != -- ]] || break
	if [[ ${argument} == --no-run ]]; then
		is_no_run=true
		break
	fi
done

run_compile_services() {
	if [[ -d crates/infra-messaging ]]; then
		echo "messaging mode=compile-only auth_proof=not-run"
		cargo test --locked -p infra-messaging --features integration \
			--test jetstream --test idle_pull --test credential_rotation --example dlq_recovery --no-run
		if [[ -f crates/infra-messaging/tests/wire_compat.rs ]]; then
			cargo test --locked -p infra-messaging --test wire_compat --no-run
		fi
	fi
	if [[ -d crates/infra-cache ]]; then
		CACHE_URL=compile-only \
			cargo test --locked -p infra-cache --features integration --test valkey --no-run
	fi
	if [[ -d crates/infra-object-storage ]]; then
		OBJECT_STORAGE_TEST_ENDPOINT=compile-only \
		OBJECT_STORAGE_TEST_ACCESS_KEY_ID=template \
		OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY=template-secret \
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
	if [[ ${is_no_run} == true ]]; then
		DATABASE_URL=${DATABASE_URL:-compile-only}
		PGBOUNCER_DATABASE_URL=${PGBOUNCER_DATABASE_URL:-compile-only}
		export DATABASE_URL PGBOUNCER_DATABASE_URL
		exec cargo test --locked -p integration-tests --features integration "$@"
	fi
	if [[ ${DATABASE_URL:-} == compile-only || ${PGBOUNCER_DATABASE_URL:-} == compile-only ]]; then
		echo "compile-only database URLs require --no-run" >&2
		exit 2
	fi

	# An optional service extension adds proof around the suite on the same fixture.
	if [[ -e "${ROOT_DIR}/scripts/ci/service-database.sh" ]]; then
		echo "refusing legacy hook scripts/ci/service-database.sh; move to scripts/service/database.sh" >&2
		exit 2
	fi
	service_db_before_tests() { :; }
	service_db_after_tests() { :; }
	if [[ -f "${ROOT_DIR}/scripts/service/database.sh" ]]; then
		# shellcheck source=/dev/null
		source "${ROOT_DIR}/scripts/service/database.sh"
	fi

	if [[ ${INTEGRATION_COMPOSE_MANAGED:-} != 1 ]]; then
		require_docker
		trap compose_cleanup EXIT
		trap 'exit 130' INT
		trap 'exit 143' TERM
		trap 'exit 129' HUP
		compose_postgres_up service-db
		compose_pgbouncer_up
		local messaging
		if [[ -f "${ROOT_DIR}/scripts/template/state.py" ]]; then
			messaging=$(python3 "${ROOT_DIR}/scripts/template/state.py" profile --repo . --field messaging)
		elif [[ -d crates/infra-messaging ]]; then
			messaging=nats-jetstream
		else
			messaging=none
		fi
		if [[ ${messaging} == nats-jetstream ]]; then
			compose_up_service nats 4222
			NATS_URL="nats://127.0.0.1:${COMPOSE_LAST_PORT}"
			export NATS_URL
		fi
		DATABASE_URL=$(compose_postgres_host_dsn)
		PGBOUNCER_DATABASE_URL=$(compose_pgbouncer_host_dsn)
		export DATABASE_URL PGBOUNCER_DATABASE_URL
	elif [[ -z ${DATABASE_URL:-} || -z ${PGBOUNCER_DATABASE_URL:-} ]]; then
		echo "INTEGRATION_COMPOSE_MANAGED=1 requires DATABASE_URL and PGBOUNCER_DATABASE_URL" >&2
		exit 2
	fi

	service_db_before_tests
	cargo test --locked -p integration-tests --features integration "$@"
	if [[ $# == 0 && -f crates/service/src/bootstrap/inbound_webhooks.rs ]]; then
		cargo test --locked --manifest-path crates/service/Cargo.toml --test lifecycle \
			restarting_with_an_old_inbound_receipt_acknowledges_redelivery_without_another_job -- --ignored --exact
	fi
	service_db_after_tests "$@"
}

run_cache() {
	if [[ ${is_no_run} == true ]]; then
		CACHE_URL=${CACHE_URL:-compile-only}
		export CACHE_URL
		exec cargo test --locked -p infra-cache --features integration --test valkey "$@"
	fi
	if [[ ${CACHE_URL:-} == compile-only ]]; then
		echo "CACHE_URL=compile-only requires --no-run" >&2
		exit 2
	fi
	if [[ ${INTEGRATION_COMPOSE_MANAGED:-} == 1 && -z ${CACHE_URL:-} ]]; then
		echo "INTEGRATION_COMPOSE_MANAGED=1 requires CACHE_URL" >&2
		exit 2
	fi
	if [[ -z ${CACHE_URL:-} ]]; then
		require_docker
		trap compose_cleanup EXIT
		trap 'exit 130' INT
		trap 'exit 143' TERM
		trap 'exit 129' HUP
		compose_init_project service-cache
		compose_up_service valkey 6379
		CACHE_URL="redis://127.0.0.1:${COMPOSE_LAST_PORT}"
	fi
	export CACHE_URL
	cargo test --locked -p infra-cache --features integration --test valkey "$@"
}

run_object_storage() {
	if [[ ${is_no_run} == true ]]; then
		OBJECT_STORAGE_TEST_ENDPOINT=${OBJECT_STORAGE_TEST_ENDPOINT:-compile-only}
		export OBJECT_STORAGE_TEST_ENDPOINT
		export OBJECT_STORAGE_TEST_ACCESS_KEY_ID=${OBJECT_STORAGE_TEST_ACCESS_KEY_ID:-template}
		export OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY=${OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY:-template-secret}
		exec cargo test --locked -p infra-object-storage --features integration --test emulator "$@"
	fi
	if [[ ${OBJECT_STORAGE_TEST_ENDPOINT:-} == compile-only ]]; then
		echo "OBJECT_STORAGE_TEST_ENDPOINT=compile-only requires --no-run" >&2
		exit 2
	fi
	if [[ ${INTEGRATION_COMPOSE_MANAGED:-} == 1 && -z ${OBJECT_STORAGE_TEST_ENDPOINT:-} ]]; then
		echo "INTEGRATION_COMPOSE_MANAGED=1 requires OBJECT_STORAGE_TEST_ENDPOINT" >&2
		exit 2
	fi
	if [[ -z ${OBJECT_STORAGE_TEST_ENDPOINT:-} ]]; then
		require_docker
		trap compose_cleanup EXIT
		trap 'exit 130' INT
		trap 'exit 143' TERM
		trap 'exit 129' HUP
		compose_init_project service-object-storage
		compose_up_service versitygw 7070
		OBJECT_STORAGE_TEST_ENDPOINT="http://127.0.0.1:${COMPOSE_LAST_PORT}"
	fi
	# The root credentials of the Compose service; a supplied emulator may override them.
	export OBJECT_STORAGE_TEST_ENDPOINT
	export OBJECT_STORAGE_TEST_ACCESS_KEY_ID=${OBJECT_STORAGE_TEST_ACCESS_KEY_ID:-template}
	export OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY=${OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY:-template-secret}
	cargo test --locked -p infra-object-storage --features integration --test emulator "$@"
}

run_messaging() {
	if [[ ${is_no_run} == true ]]; then
		echo "messaging mode=compile-only auth_proof=not-run"
		exec cargo test --locked -p infra-messaging --features integration \
			--test jetstream --test idle_pull --test credential_rotation --example dlq_recovery "$@"
	fi
	if [[ ${NATS_URL:-} == compile-only ]]; then
		echo "NATS_URL=compile-only requires --no-run" >&2
		exit 2
	fi
	if [[ ${INTEGRATION_COMPOSE_MANAGED:-} == 1 && -z ${NATS_URL:-} ]]; then
		echo "INTEGRATION_COMPOSE_MANAGED=1 requires NATS_URL" >&2
		exit 2
	fi

	# Full mode needs Docker for the isolated authentication phases even when
	# ordinary tests use a supplied shared broker.
	require_docker

	local lock_token=${VALIDATION_LOCK_TOKEN:-nolock}
	messaging_project_base="service-messaging-${lock_token:0:12}-$(date +%s)-$$"
	messaging_project=${messaging_project_base}
	messaging_config="${ROOT_DIR}/env/nats/nats-server.conf"
	messaging_cleanup_pending=false
	messaging_primary_status=0
	messaging_work_dir=$(mktemp -d "${TMPDIR:-/tmp}/service-messaging.XXXXXX")
	owned_url=

	messaging_compose() {
		COMPOSE_PROJECT="${messaging_project}" \
		NATS_PORT=127.0.0.1:0 \
		NATS_CONFIG_FILE="${messaging_config}" \
			compose_cmd "$@"
	}

	cleanup_owned_messaging() {
		[[ ${messaging_cleanup_pending} == true ]] || return 0
		messaging_cleanup_pending=false
		local down_status=0 observed=true containers networks volumes
		messaging_compose down -v --remove-orphans >>"${messaging_work_dir}/compose.log" 2>&1 || down_status=$?
		if ! containers=$(docker ps -aq --filter "label=com.docker.compose.project=${messaging_project}"); then observed=false; fi
		if ! networks=$(docker network ls -q --filter "label=com.docker.compose.project=${messaging_project}"); then observed=false; fi
		if ! volumes=$(docker volume ls -q --filter "label=com.docker.compose.project=${messaging_project}"); then observed=false; fi
		if [[ ${down_status} == 0 && ${observed} == true && -z ${containers}${networks}${volumes} ]]; then
			printf 'cleanup=observed project=%s config=%s remaining_containers=0 remaining_networks=0 remaining_volumes=0\n' \
				"${messaging_project}" "${messaging_config}"
			return 0
		fi
		printf 'cleanup=unobserved project=%s config=%s down_exit=%s readback=%s\n' \
			"${messaging_project}" "${messaging_config}" "${down_status}" "${observed}" >&2
		cat "${messaging_work_dir}/compose.log" >&2 || true
		echo "owned messaging teardown was not established" >&2
		return 1
	}

	# shellcheck disable=SC2329
	finish_messaging() {
		local status=$? cleanup_status=0
		trap - EXIT INT TERM HUP
		if [[ ${messaging_primary_status:-0} != 0 ]]; then status=${messaging_primary_status}; fi
		cleanup_owned_messaging || cleanup_status=$?
		if [[ ${status} == 0 && ${cleanup_status} != 0 ]]; then status=${cleanup_status}; fi
		rm -rf -- "${messaging_work_dir}"
		exit "${status}"
	}
	trap finish_messaging EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	trap 'exit 129' HUP

	wait_owned_messaging() {
		if ! messaging_compose up -d --wait --wait-timeout 30 nats >>"${messaging_work_dir}/compose.log" 2>&1; then
			messaging_compose ps --all >&2 || true
			messaging_compose logs --no-color --tail 100 nats >&2 || true
			cat "${messaging_work_dir}/compose.log" >&2 || true
			echo "owned messaging startup failed" >&2
			return 1
		fi
		local address port
		address=$(messaging_compose port nats 4222) || return
		port=${address##*:}
		[[ ${port} =~ ^[0-9]+$ ]] || { echo "failed to resolve the owned NATS port" >&2; return 1; }
		owned_url="nats://127.0.0.1:${port}"
	}

	start_owned_messaging() {
		messaging_config=$1
		messaging_project="${messaging_project_base}-$2"
		messaging_cleanup_pending=true
		wait_owned_messaging
	}

	local ordinary_url=${NATS_URL:-}
	if [[ -z ${ordinary_url} ]]; then
		start_owned_messaging "${ROOT_DIR}/env/nats/nats-server.conf" ordinary
		ordinary_url=${owned_url}
	fi
	# idle_pull waits out pull expiries (33 s in CI); run it beside the jetstream
	# suite after one shared compile step.
	if cargo test --locked -p infra-messaging --features integration --test jetstream --test idle_pull --example dlq_recovery --no-run; then
		local idle_log="${messaging_work_dir}/idle-pull-test.log"
		local idle_pid idle_status=0
		NATS_URL="${ordinary_url}" cargo test --locked -p infra-messaging --features integration --test idle_pull "$@" >"${idle_log}" 2>&1 &
		idle_pid=$!
		if NATS_URL="${ordinary_url}" cargo test --locked -p infra-messaging --features integration --test jetstream --example dlq_recovery "$@"; then
			:
		else
			messaging_primary_status=$?
		fi
		wait "${idle_pid}" || idle_status=$?
		cat "${idle_log}"
		[[ ${messaging_primary_status} != 0 ]] || messaging_primary_status=${idle_status}
	else
		messaging_primary_status=$?
	fi
	if [[ ${messaging_primary_status} == 0 ]]; then
		if [[ -z ${owned_url} ]]; then
			start_owned_messaging "${ROOT_DIR}/env/nats/nats-server.conf" cursor
		fi
		run_cursor_check() {
			local log="${messaging_work_dir}/$1.log"
			NATS_URL="${owned_url}" NATS_CURSOR_RECEIPT="${messaging_work_dir}/cursor.json" \
				cargo test --locked -p infra-messaging --features integration --test jetstream \
				"connection::$1" -- --ignored --exact | tee "${log}" || return
			grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;' "${log}"
		}
		run_cursor_check save_ack_floor_before_restart &&
			messaging_compose restart --timeout 10 nats &&
			wait_owned_messaging &&
			run_cursor_check read_ack_floor_after_restart || messaging_primary_status=$?
	fi
	if ! cleanup_owned_messaging; then
		[[ ${messaging_primary_status} != 0 ]] || messaging_primary_status=1
		exit "${messaging_primary_status}"
	fi

	local fixture_dir="${ROOT_DIR}/env/nats/auth-rotation"
	start_owned_messaging "${fixture_dir}/nats-server.conf" auth
	local auth_log="${messaging_work_dir}/auth-test.log"
	local auth_status=0
	if NATS_AUTH_URL="${owned_url}" NATS_AUTH_CREDS_A="${fixture_dir}/user-a.creds" NATS_AUTH_CREDS_B="${fixture_dir}/user-b.creds" \
		cargo test --locked -p infra-messaging --features integration --test jetstream \
		credentials::credentials_file_rotation_is_authenticated_by_the_broker -- --ignored --exact --no-capture >"${auth_log}" 2>&1; then
		if ! grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;' "${auth_log}"; then
			echo "JWT rotation proof did not execute exactly one passing, nonignored test" >&2
			auth_status=1
		fi
	else
		auth_status=$?
	fi
	cat "${auth_log}"
	[[ ${messaging_primary_status} != 0 ]] || messaging_primary_status=${auth_status}
	if ! cleanup_owned_messaging; then
		[[ ${messaging_primary_status} != 0 ]] || messaging_primary_status=1
		exit "${messaging_primary_status}"
	fi

	# The expiry/reread proof has a distinct synthetic account and never replaces
	# the fixed-JWT fixture or a live ordinary/shared broker.
	start_owned_messaging "${ROOT_DIR}/env/nats/credential-rotation.conf" credential-rotation
	local rotation_log="${messaging_work_dir}/credential-rotation-test.log"
	local rotation_status=0
	if NATS_AUTH_URL="${owned_url}" cargo test --locked -p infra-messaging --features integration \
		--test credential_rotation expired_old_credentials_are_refused_and_file_replacement_recovers_the_client \
		-- --exact --no-capture >"${rotation_log}" 2>&1; then
		if ! grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;' "${rotation_log}"; then
			echo "credential expiry/reread proof did not execute exactly one passing test" >&2
			rotation_status=1
		fi
	else
		rotation_status=$?
	fi
	cat "${rotation_log}"
	[[ ${messaging_primary_status} != 0 ]] || messaging_primary_status=${rotation_status}
	exit "${messaging_primary_status}"
}

run_oauth() {
	if [[ ${is_no_run} == true ]]; then
		OAUTH_TEST_KEYCLOAK_URL=${OAUTH_TEST_KEYCLOAK_URL:-compile-only}
		export OAUTH_TEST_KEYCLOAK_URL
		exec cargo test --locked -p infra-oauth2-client-credentials --features integration tests::keycloak "$@"
	fi
	if [[ ${OAUTH_TEST_KEYCLOAK_URL:-} == compile-only ]]; then
		echo "OAUTH_TEST_KEYCLOAK_URL=compile-only requires --no-run" >&2
		exit 2
	fi
	if [[ ${INTEGRATION_COMPOSE_MANAGED:-} == 1 && -z ${OAUTH_TEST_KEYCLOAK_URL:-} ]]; then
		echo "INTEGRATION_COMPOSE_MANAGED=1 requires OAUTH_TEST_KEYCLOAK_URL" >&2
		exit 2
	fi

	oauth_container=
	# shellcheck disable=SC2329
	cleanup_oauth() {
		local status=$?
		trap - EXIT INT TERM HUP
		if [[ -n ${oauth_container:-} ]]; then
			docker rm -f -v "${oauth_container}" >/dev/null 2>&1 || true
		fi
		exit "${status}"
	}

	if [[ -z ${OAUTH_TEST_KEYCLOAK_URL:-} ]]; then
		# shellcheck source=tools/versions.env
		. "${ROOT_DIR}/tools/versions.env"
		local ready_timeout_seconds=180 container_name lock_token=${VALIDATION_LOCK_TOKEN:-nolock}
		container_name="service-oauth-${lock_token:0:12}-$(date +%s)-$$"
		require_docker
		trap cleanup_oauth EXIT
		trap 'exit 130' INT
		trap 'exit 143' TERM
		trap 'exit 129' HUP
		# Development mode: plain HTTP on loopback and an in-memory database,
		# which is all a throwaway realm needs.
		oauth_container=$(docker run -d --name "${container_name}" -p 127.0.0.1::8080 \
			-e KC_BOOTSTRAP_ADMIN_USERNAME=admin -e KC_BOOTSTRAP_ADMIN_PASSWORD=admin \
			"${KEYCLOAK_IMAGE}" start-dev)
		local address port
		address=$(docker port "${oauth_container}" 8080/tcp)
		port=${address##*:}
		if [[ ! ${port} =~ ^[0-9]+$ ]]; then
			echo "failed to resolve the Keycloak port" >&2
			exit 1
		fi
		OAUTH_TEST_KEYCLOAK_URL="http://127.0.0.1:${port}"
		if ! wait_http_ready "${OAUTH_TEST_KEYCLOAK_URL}/realms/master" "${ready_timeout_seconds}" "${oauth_container}"; then
			echo "Keycloak did not become ready within ${ready_timeout_seconds}s" >&2
			docker logs --tail 100 "${oauth_container}" >&2 || true
			exit 1
		fi
	fi
	export OAUTH_TEST_KEYCLOAK_URL
	cargo test --locked -p infra-oauth2-client-credentials --features integration tests::keycloak "$@"
}

case ${mode} in
--compile-retained) run_compile_retained ;;
--compile-services) run_compile_services ;;
db) run_db "$@" ;;
cache) run_cache "$@" ;;
object-storage) run_object_storage "$@" ;;
messaging) run_messaging "$@" ;;
oauth) run_oauth "$@" ;;
esac
