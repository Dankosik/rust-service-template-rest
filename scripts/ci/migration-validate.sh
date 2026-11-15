#!/usr/bin/env bash
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
cleanup() {
	local status=$?
	trap - EXIT INT TERM HUP
	if [[ -n ${history_container} ]]; then
		docker rm -f -v "${history_container}" >/dev/null 2>&1 || true
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
# in production too. An optional second argument names another migrator
# entrypoint in the same image.
run_migrate() {
	local name="service-migrate-${VALIDATION_LOCK_TOKEN:0:12}-$$-$1"
	docker run --rm --name "${name}" \
		--network "${COMPOSE_NETWORK}" \
		--read-only --cap-drop=ALL --security-opt=no-new-privileges \
		-e APP__POSTGRES__ENABLED=true \
		-e "APP__POSTGRES__DSN=${dsn}" \
		--entrypoint "${2:-/migrate}" "${image}"
}

# A nonempty embedded set cannot admit an absent history: startup checks it
# read-only and must refuse before the migrator creates bookkeeping. An empty
# source set intentionally admits an empty database, so preserve that profile's
# contract by skipping this nonempty-source scenario.
if compgen -G 'migrations/*.sql' >/dev/null; then
	history_name="service-migration-history-${VALIDATION_LOCK_TOKEN:0:12}-$$"
	history_container=$(docker run -d --name "${history_name}" \
		--network "${COMPOSE_NETWORK}" \
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
	docker rm -f -v "${history_container}" >/dev/null 2>&1 || true
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

check_migration_log() {
	python3 -c '
import json
import sys

first = sys.argv[1] == "first"
seen = False
for line in sys.stdin:
    try:
        event = json.loads(line)
    except ValueError:
        continue
    if not isinstance(event, dict) or event.get("message") != "migration_run":
        continue
    seen = True
    outcome = event.get("outcome")
    if outcome == "no_change" or (first and outcome == "success"):
        sys.exit(0)
if not first:
    sys.exit("second migration run was not a no_change")
if not seen:
    sys.exit("first migration run logged no migration_run record")
sys.exit("first migration run did not succeed")
' "$1"
}

first=$(run_migrate first)
printf '%s\n' "${first}"
check_migration_log first <<<"${first}"

second=$(run_migrate second)
check_migration_log second <<<"${second}" || {
	printf '%s\n' "${second}" >&2
	exit 1
}
echo "migration rehearsal: embedded set applied, replay is no_change"

RUNTIME_IMAGE_NETWORK="${COMPOSE_NETWORK}" \
	RUNTIME_IMAGE_POSTGRES_DSN="${dsn}" \
	bash ./scripts/ci/runtime-image-check.sh "${image}" "${expected_commit}"

# An optional service extension rehearses further migration sets with the same
# fixture cleanup, image and run_migrate helper.
service_migration_validate() { :; }
if [[ -f scripts/ci/service-database.sh ]]; then
	# shellcheck source=/dev/null
	source scripts/ci/service-database.sh
fi
service_migration_validate
