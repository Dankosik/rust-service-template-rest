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
history_name="service-migration-history-$(date +%s)-$$-${RANDOM}"
history_ticket=
migrate_name=
migrate_ticket=
migrate_cid_dir=
migrate_cidfile=
cleanup_container() {
	local name=$1 ticket=$2 id=$3 remaining
	[[ -n ${ticket} ]] || return 0
	remaining=$(docker ps --all --quiet --filter "name=^/${name}$") || return 1
	if [[ -n ${remaining} ]]; then
		docker rm -f "${name}" >/dev/null 2>&1 || return 1
	fi
	remaining=$(docker ps --all --quiet --filter "name=^/${name}$") || return 1
	[[ -z ${remaining} && ${id} =~ ^[0-9a-f]{64}$ ]] || return 1
	bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-complete migration-container "${ticket}" container-absent "${name}"
}

cleanup_migrate() {
	local id=
	if [[ -n ${migrate_cidfile} && -f ${migrate_cidfile} ]]; then
		id=$(<"${migrate_cidfile}")
	fi
	cleanup_container "${migrate_name}" "${migrate_ticket}" "${id}" || return 1
	if [[ -n ${migrate_cidfile} ]]; then rm -f "${migrate_cidfile}" || return 1; fi
	migrate_ticket=
}

cleanup() {
	local status=$? cleanup_failed=false
	trap - EXIT INT TERM HUP
	cleanup_container "${history_name}" "${history_ticket}" "${history_container}" || cleanup_failed=true
	cleanup_migrate || cleanup_failed=true
	compose_postgres_down || cleanup_failed=true
	if [[ -n ${migrate_cid_dir} && -z ${migrate_ticket} ]]; then
		rmdir "${migrate_cid_dir}" || cleanup_failed=true
	fi
	if [[ ${cleanup_failed} == true ]]; then
		echo "migration rehearsal cleanup incomplete" >&2
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
	docker run --rm --cidfile "${migrate_cidfile}" --name "${migrate_name}" --network "${COMPOSE_NETWORK}" \
		--read-only --cap-drop=ALL --security-opt=no-new-privileges \
		-e APP__POSTGRES__ENABLED=true \
		-e "APP__POSTGRES__DSN=${dsn}" \
		--entrypoint /migrate "${image}"
}

# A nonempty embedded set cannot admit an absent history: startup checks it
# read-only and must refuse before the migrator creates bookkeeping. An empty
# source set intentionally admits an empty database, so preserve that profile's
# contract by skipping this nonempty-source scenario.
if compgen -G 'migrations/*.sql' >/dev/null; then
	history_ticket=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-begin migration-container "${history_name}")
	history_container=$(docker run -d --name "${history_name}" --network "${COMPOSE_NETWORK}" \
		--read-only --cap-drop=ALL --security-opt=no-new-privileges \
		-e APP__POSTGRES__ENABLED=true \
		-e "APP__POSTGRES__DSN=${dsn}" \
		"${image}")
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
	cleanup_container "${history_name}" "${history_ticket}" "${history_container}"
	history_ticket=
	history_container=''
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

migrate_cid_dir=$(mktemp -d "${TMPDIR:-/tmp}/service-migrate.XXXXXX")
migrate_cidfile="${migrate_cid_dir}/first.cid"
migrate_name="service-migrate-first-$(date +%s)-$$-${RANDOM}"
migrate_ticket=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-begin migration-container "${migrate_name}")
first=$(run_migrate)
cleanup_migrate
migrate_ticket=
printf '%s\n' "${first}"
grep -Fq '"message":"migration_run"' <<<"${first}" || {
	echo "first migration run logged no migration_run record" >&2
	exit 1
}
grep -Eq '"outcome":"(success|no_change)"' <<<"${first}" || {
	echo "first migration run did not succeed" >&2
	exit 1
}

migrate_cidfile="${migrate_cid_dir}/second.cid"
migrate_name="service-migrate-second-$(date +%s)-$$-${RANDOM}"
migrate_ticket=$(bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --ticket-begin migration-container "${migrate_name}")
second=$(run_migrate)
cleanup_migrate
migrate_ticket=
grep -Fq '"outcome":"no_change"' <<<"${second}" || {
	echo "second migration run was not a no_change" >&2
	printf '%s\n' "${second}" >&2
	exit 1
}
echo "migration rehearsal: embedded set applied, replay is no_change"

RUNTIME_IMAGE_NETWORK="${COMPOSE_NETWORK}" \
	RUNTIME_IMAGE_POSTGRES_DSN="${dsn}" \
	bash ./scripts/ci/runtime-image-check.sh "${image}" "${expected_commit}"
