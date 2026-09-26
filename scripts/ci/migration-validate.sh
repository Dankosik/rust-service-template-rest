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
cleanup() {
	if [[ -n ${history_container} ]]; then
		docker rm -f "${history_container}" >/dev/null 2>&1 || true
	fi
	compose_postgres_down
}
trap cleanup EXIT INT TERM
compose_postgres_up service-migration
dsn=$(compose_postgres_network_dsn)

# The same hardened flags as the service; the migration job runs under them
# in production too.
run_migrate() {
	docker run --rm --network "${COMPOSE_NETWORK}" \
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
	history_container=$(docker run -d --network "${COMPOSE_NETWORK}" \
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
	docker rm "${history_container}" >/dev/null
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

first=$(run_migrate)
printf '%s\n' "${first}"
grep -Fq '"message":"migration_run"' <<<"${first}" || {
	echo "first migration run logged no migration_run record" >&2
	exit 1
}
grep -Eq '"outcome":"(success|no_change)"' <<<"${first}" || {
	echo "first migration run did not succeed" >&2
	exit 1
}

second=$(run_migrate)
grep -Fq '"outcome":"no_change"' <<<"${second}" || {
	echo "second migration run was not a no_change" >&2
	printf '%s\n' "${second}" >&2
	exit 1
}
echo "migration rehearsal: embedded set applied, replay is no_change"

RUNTIME_IMAGE_NETWORK="${COMPOSE_NETWORK}" \
	RUNTIME_IMAGE_POSTGRES_DSN="${dsn}" \
	bash ./scripts/ci/runtime-image-check.sh "${image}" "${expected_commit}"
