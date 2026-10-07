#!/usr/bin/env bash
# Rehearse the runtime image against a live database: `/migrate` from the
# image applies the embedded set to a fresh PostgreSQL, a second run reports
# no_change, and the service image then passes the lifecycle check with the
# profile enabled, so readiness is proven with the pool open.
#
#   migration-validate.sh [IMAGE [EXPECTED_COMMIT]]
#   Without IMAGE the runtime image is built as service:migration first.
#   REQUIRE_DOCKER=1 makes a missing Docker a failure instead of a refusal.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

if ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --assert-held; then
	exec bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" -- bash "${ROOT_DIR}/scripts/ci/migration-validate.sh" "$@"
fi
# shellcheck source=scripts/lib/compose-postgres.sh
source scripts/lib/compose-postgres.sh

requested_image=${1:-}
expected_commit=${2:-}

require_docker
image=${requested_image:-service:migration}
if [[ -z ${requested_image} ]]; then
	make runtime-image-build RUNTIME_IMAGE="${image}"
fi

history_container=''
history_resource=''
cleanup() {
	local status=$?
	trap - EXIT INT TERM HUP
	if [[ -n ${history_resource} ]]; then
		if ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-cleanup "${history_resource}"; then
			echo "validation history container cleanup incomplete" >&2
			if [[ ${status} == 0 ]]; then status=1; fi
		fi
	fi
	if ! compose_postgres_down; then
		echo "validation PostgreSQL cleanup incomplete" >&2
		if [[ ${status} == 0 ]]; then status=1; fi
	fi
	exit "${status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
compose_postgres_up service-migration
dsn=$(compose_postgres_network_dsn)

# The same hardened flags as the service; the migration job runs under them
# in production too.
run_migrate() {
	local name="service-migrate-${VALIDATION_LOCK_TOKEN:0:12}-$$-$1"
	local resource status=0
	resource=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-register container "${name}") || return
	bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-run "${resource}" -- \
		docker run --rm --name "${name}" --label "dev.rust-service.validation-owner=${VALIDATION_LOCK_TOKEN}" \
		--network "${COMPOSE_NETWORK}" \
		--read-only --cap-drop=ALL --security-opt=no-new-privileges \
		-e APP__POSTGRES__ENABLED=true \
		-e "APP__POSTGRES__DSN=${dsn}" \
		--entrypoint /migrate "${image}" || status=$?
	if ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-cleanup "${resource}"; then
		echo "validation migrator cleanup incomplete: ${resource}" >&2
		if [[ ${status} == 0 ]]; then status=1; fi
	fi
	return "${status}"
}

# A nonempty embedded set cannot admit an absent history: startup checks it
# read-only and must refuse before the migrator creates bookkeeping. An empty
# source set intentionally admits an empty database, so preserve that profile's
# contract by skipping this nonempty-source scenario.
if compgen -G 'migrations/*.sql' >/dev/null; then
	history_name="service-migration-history-${VALIDATION_LOCK_TOKEN:0:12}-$$"
	history_resource=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-register container "${history_name}")
	history_container=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-run "${history_resource}" -- \
		docker run -d --name "${history_name}" \
		--label "dev.rust-service.validation-owner=${VALIDATION_LOCK_TOKEN}" --network "${COMPOSE_NETWORK}" \
		--read-only --cap-drop=ALL --security-opt=no-new-privileges \
		-e APP__POSTGRES__ENABLED=true \
		-e "APP__POSTGRES__DSN=${dsn}" \
		"${image}")
	bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-bind "${history_resource}" "${history_container}"
	history_deadline=$((SECONDS + 30))
	while [[ $(docker inspect --format '{{.State.Running}}' "${history_container}") == true ]]; do
		if ((SECONDS >= history_deadline)); then
			echo "service did not refuse missing migration history within 30 seconds" >&2
			docker logs "${history_container}" >&2
			exit 1
		fi
		sleep 0.1
	done
	history_exit=$(docker inspect --format '{{.State.ExitCode}}' "${history_container}")
	history_refusal=$(docker logs "${history_container}" 2>&1)
	bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --resource-cleanup "${history_resource}"
	history_container=''
	history_resource=''
	if [[ ${history_exit} == 0 ]]; then
		echo "service admitted missing migration history" >&2
		printf '%s\n' "${history_refusal}" >&2
		exit 1
	fi
	grep -Fq 'postgres migration history: embedded migrations are pending' <<<"${history_refusal}" || {
		echo "service did not refuse missing migration history" >&2
		printf '%s\n' "${history_refusal}" >&2
		exit 1
	}
	echo "service refused missing migration history before migration"
fi

first=$(run_migrate first)
printf '%s\n' "${first}"
grep -Fq '"message":"migration_run"' <<<"${first}" || {
	echo "first migration run logged no migration_run record" >&2
	exit 1
}
grep -Eq '"outcome":"(success|no_change)"' <<<"${first}" || {
	echo "first migration run did not succeed" >&2
	exit 1
}

second=$(run_migrate second)
grep -Fq '"outcome":"no_change"' <<<"${second}" || {
	echo "second migration run was not a no_change" >&2
	printf '%s\n' "${second}" >&2
	exit 1
}
echo "migration rehearsal: embedded set applied, replay is no_change"

RUNTIME_IMAGE_NETWORK="${COMPOSE_NETWORK}" \
	RUNTIME_IMAGE_POSTGRES_DSN="${dsn}" \
	bash ./scripts/ci/runtime-image-check.sh "${image}" "${expected_commit}"
