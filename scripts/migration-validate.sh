#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${ROOT_DIR}"

unset PGHOSTADDR PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE PGSSLMODE \
	PGSSLROOTCERT PGSSLCERT PGSSLKEY PGAPPNAME PGOPTIONS PGPASSFILE

requested_image=${1:-}
expected_commit=${2:-}

docker info >/dev/null 2>&1 || { echo "docker is required for migration-validate" >&2; exit 2; }
command -v jq >/dev/null 2>&1 || { echo "jq is required for migration-validate" >&2; exit 2; }

image=${requested_image:-service:migration}
[[ -n ${requested_image} ]] || bash "${ROOT_DIR}/scripts/image.sh" build "${image}"

project="service-migration-$$-$(date +%s)"
network="${project}_default"
history_container=""

cleanup() {
	local rc=$?
	trap - EXIT INT TERM HUP
	[[ -z ${history_container} ]] || docker rm -f -v "${history_container}" >/dev/null 2>&1 || true
	POSTGRES_PORT=0 docker compose -p "${project}" -f "${ROOT_DIR}/env/docker-compose.yml" down -v --remove-orphans >/dev/null 2>&1 || {
		echo "validation PostgreSQL cleanup incomplete" >&2
		[[ ${rc} != 0 ]] || rc=1
	}
	exit "${rc}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

POSTGRES_PORT=0 docker compose -p "${project}" -f "${ROOT_DIR}/env/docker-compose.yml" up -d --wait postgres
dsn="postgres://app:app@postgres:5432/app?sslmode=disable"

run_migrate() {
	docker run --rm --name "service-migrate-$$-$1" \
		--network "${network}" \
		--read-only --cap-drop=ALL --security-opt=no-new-privileges \
		-e APP__APP__ENV=local \
		-e APP__APP__ALLOW_INSECURE_DEVELOPMENT=true \
		-e APP__POSTGRES__ENABLED=true \
		-e "APP__POSTGRES__DSN=${dsn}" \
		--entrypoint /migrate "${image}"
}

if compgen -G 'migrations/*.sql' >/dev/null; then
	history_container=$(docker run -d --name "service-migration-history-$$" \
		--network "${network}" \
		--read-only --cap-drop=ALL --security-opt=no-new-privileges \
		-e APP__APP__ENV=local \
		-e APP__APP__ALLOW_INSECURE_DEVELOPMENT=true \
		-e APP__POSTGRES__ENABLED=true \
		-e "APP__POSTGRES__DSN=${dsn}" \
		"${image}")
	deadline=$((SECONDS + 30))
	while [[ $(docker inspect -f '{{.State.Running}}' "${history_container}") == true ]]; do
		((SECONDS < deadline)) || { docker logs "${history_container}" >&2; echo "service did not refuse pending migrations" >&2; exit 1; }
		sleep 0.1
	done
	history_exit=$(docker inspect -f '{{.State.ExitCode}}' "${history_container}")
	history_logs=$(docker logs "${history_container}" 2>&1)
	docker rm -f -v "${history_container}" >/dev/null 2>&1 || true
	history_container=""
	if [[ ${history_exit} == 0 ]] || ! grep -Fq 'postgres migration history: embedded migrations are pending' <<<"${history_logs}"; then
		printf '%s\n' "${history_logs}" >&2
		echo "service did not refuse missing migration history (exit ${history_exit})" >&2
		exit 1
	fi
	echo "service refused missing migration history before migration"
fi

first=$(run_migrate first)
printf '%s\n' "${first}"
printf '%s\n' "${first}" | jq -e 'fromjson? | select(type == "object" and .message == "migration_run" and (.outcome == "success" or .outcome == "no_change"))' >/dev/null || {
	echo "first migration run did not succeed" >&2; exit 1
}

second=$(run_migrate second)
printf '%s\n' "${second}" | jq -e 'fromjson? | select(type == "object" and .message == "migration_run" and .outcome == "no_change")' >/dev/null || {
	printf '%s\n' "${second}" >&2
	echo "second migration run was not no_change" >&2
	exit 1
}
echo "migration rehearsal: embedded set applied, replay is no_change"

RUNTIME_IMAGE_NETWORK="${network}" \
	RUNTIME_IMAGE_APP_ENV=local \
	RUNTIME_IMAGE_ALLOW_INSECURE_DEVELOPMENT=true \
	RUNTIME_IMAGE_POSTGRES_DSN="${dsn}" \
	bash "${ROOT_DIR}/scripts/image.sh" check "${image}" "${expected_commit}"
