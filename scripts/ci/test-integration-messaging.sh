#!/usr/bin/env bash
# Ordinary JetStream proof may use a shared NATS_URL. Each authenticated phase
# owns a private instance of the same Compose service and pinned image.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

if ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --assert-held; then
	exec bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" -- bash "${ROOT_DIR}/scripts/ci/test-integration-messaging.sh" "$@"
fi

auth_only=false
if [[ ${1:-} == --auth-only ]]; then
	auth_only=true
	shift
	[[ $# == 0 ]] || { echo "--auth-only accepts no ordinary-suite filters" >&2; exit 2; }
fi

# CI warms the existing test executables without starting their fixtures.
for argument in "$@"; do
	[[ ${argument} != -- ]] || break
	if [[ ${argument} == --no-run ]]; then
		echo "messaging mode=compile-only auth_proof=not-run"
		exec cargo test --locked -p infra-messaging --features integration --test jetstream --test idle_pull --test credential_rotation --example dlq_recovery "$@"
	fi
done
if [[ ${NATS_URL:-} == compile-only ]]; then
	echo "NATS_URL=compile-only requires --no-run" >&2
	exit 2
fi

compose_project_base="service-messaging-${VALIDATION_LOCK_TOKEN:0:12}-$(date +%s)-$$"
compose_project=${compose_project_base}
resource=
compose_config="${ROOT_DIR}/env/nats/nats-server.conf"
cleanup_pending=false
receipt_dir=${MESSAGING_RECEIPT_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/service-messaging-receipt.XXXXXX")}
mkdir -p "${receipt_dir}"
receipt="${receipt_dir}/lifecycle.txt"
printf 'project=%s\nauth_only=%s\n' "${compose_project}" "${auth_only}" >"${receipt}"
printf 'messaging receipt directory: %s\n' "${receipt_dir}"

compose() {
	NATS_PORT=127.0.0.1:0 NATS_CONFIG_FILE="${compose_config}" \
		docker compose -p "${compose_project}" -f "${ROOT_DIR}/env/docker-compose.yml" "$@"
}

require_docker() {
	if docker info >/dev/null 2>&1; then
		return 0
	fi
	echo "Docker is required for the owned authentication phases" >&2
	[[ ${REQUIRE_DOCKER:-} != 1 ]] || return 1
	return 2
}

cleanup_owned() {
	[[ ${cleanup_pending} == true ]] || return 0
	cleanup_pending=false
	local down_status=0 observed=true containers networks volumes
	if NATS_PORT=127.0.0.1:0 NATS_CONFIG_FILE="${compose_config}" \
		bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-cleanup "${resource}" >>"${receipt_dir}/compose.log" 2>&1; then
		:
	else
		down_status=$?
	fi
	if ! containers=$(docker ps -aq --filter "label=com.docker.compose.project=${compose_project}"); then observed=false; fi
	if ! networks=$(docker network ls -q --filter "label=com.docker.compose.project=${compose_project}"); then observed=false; fi
	if ! volumes=$(docker volume ls -q --filter "label=com.docker.compose.project=${compose_project}"); then observed=false; fi
	if [[ ${down_status} == 0 && ${observed} == true && -z ${containers}${networks}${volumes} ]]; then
		printf 'cleanup=observed project=%s config=%s remaining_containers=0 remaining_networks=0 remaining_volumes=0\n' \
			"${compose_project}" "${compose_config}" | tee -a "${receipt}"
		return 0
	fi
	printf 'cleanup=unobserved project=%s config=%s down_exit=%s readback=%s\n' \
		"${compose_project}" "${compose_config}" "${down_status}" "${observed}" | tee -a "${receipt}" >&2
	echo "owned messaging teardown was not established; inspect the project and compose.log" >&2
	return 1
}

# The EXIT trap invokes this callback; keep its exit-status and cleanup custody.
# shellcheck disable=SC2329
finish() {
	local status=$? cleanup_status=0
	trap - EXIT INT TERM HUP
	if [[ ${primary_status:-0} != 0 ]]; then status=${primary_status}; fi
	cleanup_owned || cleanup_status=$?
	if [[ ${status} == 0 && ${cleanup_status} != 0 ]]; then status=${cleanup_status}; fi
	printf 'runner_exit=%s\n' "${status}" >>"${receipt}"
	exit "${status}"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

start_owned() {
	compose_config=$1
	# A completed resource identity cannot be reused by a later phase.
	compose_project="${compose_project_base}-$2"
	resource=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-register compose "${compose_project}" \
		--file "${ROOT_DIR}/env/docker-compose.yml")
	cleanup_pending=true
	printf 'start project=%s config=%s\n' "${compose_project}" "${compose_config}" >>"${receipt}"
	if ! NATS_PORT=127.0.0.1:0 NATS_CONFIG_FILE="${compose_config}" \
		bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-run "${resource}" -- \
		docker compose -p "${compose_project}" -f "${ROOT_DIR}/env/docker-compose.yml" \
		up -d --wait --wait-timeout 30 nats >>"${receipt_dir}/compose.log" 2>&1; then
		compose ps --all >&2 || true
		compose logs --no-color --tail 100 nats >&2 || true
		echo "owned messaging startup failed; see compose.log" >&2
		return 1
	fi
	local address port
	address=$(compose port nats 4222) || return
	port=${address##*:}
	[[ ${port} =~ ^[0-9]+$ ]] || { echo "failed to resolve the owned NATS port" >&2; return 1; }
	owned_url="nats://127.0.0.1:${port}"
	printf 'ready project=%s endpoint=%s\n' "${compose_project}" "${owned_url}" >>"${receipt}"
}

# Full mode needs Docker even when ordinary tests use a supplied shared broker.
require_docker
primary_status=0
if [[ ${auth_only} == false ]]; then
	ordinary_url=${NATS_URL:-}
	if [[ -z ${ordinary_url} ]]; then
		start_owned "${ROOT_DIR}/env/nats/nats-server.conf" ordinary
		ordinary_url=${owned_url}
	fi
	if NATS_URL="${ordinary_url}" cargo test --locked -p infra-messaging --features integration --test jetstream --test idle_pull --example dlq_recovery "$@"; then
		:
	else
		primary_status=$?
	fi
	printf 'ordinary_exit=%s\n' "${primary_status}" >>"${receipt}"
	# The auth configuration never replaces a live ordinary or shared broker.
	if ! cleanup_owned; then
		[[ ${primary_status} != 0 ]] || primary_status=1
		exit "${primary_status}"
	fi
fi

fixture_dir="${ROOT_DIR}/env/nats/auth-rotation"
start_owned "${fixture_dir}/nats-server.conf" auth
auth_log="${receipt_dir}/auth-test.log"
auth_status=0
if NATS_AUTH_URL="${owned_url}" NATS_AUTH_CREDS_A="${fixture_dir}/user-a.creds" NATS_AUTH_CREDS_B="${fixture_dir}/user-b.creds" \
	cargo test --locked -p infra-messaging --features integration --test jetstream \
	credentials_file_rotation_is_authenticated_by_the_broker -- --ignored --exact --nocapture >"${auth_log}" 2>&1; then
	if ! grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;' "${auth_log}"; then
		echo "JWT rotation proof did not execute exactly one passing, nonignored test" >&2
		auth_status=1
	fi
else
	auth_status=$?
fi
cat "${auth_log}"
printf 'auth_exit=%s auth_log=%s\n' "${auth_status}" "${auth_log}" >>"${receipt}"
[[ ${primary_status} != 0 ]] || primary_status=${auth_status}
if ! cleanup_owned; then
	[[ ${primary_status} != 0 ]] || primary_status=1
	exit "${primary_status}"
fi

# The expiry/reread proof has a distinct synthetic account. It never replaces
# the fixed-JWT fixture or a live ordinary/shared broker.
start_owned "${ROOT_DIR}/env/nats/credential-rotation.conf" credential-rotation
rotation_log="${receipt_dir}/credential-rotation-test.log"
rotation_status=0
if NATS_AUTH_URL="${owned_url}" cargo test --locked -p infra-messaging --features integration \
	--test credential_rotation expired_old_credentials_are_refused_and_file_replacement_recovers_the_client \
	-- --exact --nocapture >"${rotation_log}" 2>&1; then
	if ! grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;' "${rotation_log}"; then
		echo "credential expiry/reread proof did not execute exactly one passing test" >&2
		rotation_status=1
	fi
else
	rotation_status=$?
fi
cat "${rotation_log}"
printf 'credential_rotation_exit=%s credential_rotation_log=%s\n' "${rotation_status}" "${rotation_log}" >>"${receipt}"
[[ ${primary_status} != 0 ]] || primary_status=${rotation_status}
exit "${primary_status}"
