#!/usr/bin/env bash
# Database-backed proof: PostgreSQL from env/docker-compose.yml on an
# ephemeral port with PgBouncer in front of it, plus NATS when the selected
# profile needs outbox publication. The integration CI job can instead own a
# shared Compose lifecycle and provide DATABASE_URL, PGBOUNCER_DATABASE_URL
# and NATS_URL itself. Extra arguments go to `cargo test`.
#
#   test-integration-db.sh [cargo test args]
#   test-integration-db.sh --jobs-reliability-reference
#   REQUIRE_DOCKER=1 makes a missing Docker a failure instead of a refusal.
#   --jobs-reliability-reference runs, in place of the suite, the source-only
#   exact-commit recovery and initialized-service rehearsal against the same
#   Compose services (make jobs-reliability-reference). No test filter.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

if ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --assert-held; then
	exec bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" -- bash "${ROOT_DIR}/scripts/ci/test-integration-db.sh" "$@"
fi
reference=false
if [[ ${1:-} == --jobs-reliability-reference ]]; then
	shift
	reference=true
	if (($#)) || [[ ! -f make/source.mk ]]; then
		echo "the jobs reliability reference is source-only and takes no test filter" >&2
		exit 2
	fi
fi
# shellcheck source=scripts/lib/compose-postgres.sh
source scripts/lib/compose-postgres.sh

if [[ ${INTEGRATION_COMPOSE_MANAGED:-} != 1 ]]; then
	require_docker
	trap compose_postgres_cleanup EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	trap 'exit 129' HUP
	compose_postgres_up service-db
	compose_pgbouncer_up
	if [[ $(python3 scripts/lib/template_state.py profile --repo . --field messaging) == nats-jetstream ]]; then
		if ! NATS_PORT=0 compose_postgres up -d --wait nats; then
			NATS_PORT=0 compose_postgres ps --all || true
			NATS_PORT=0 compose_postgres logs --no-color --tail 100 nats || true
			container_id=$(NATS_PORT=0 compose_postgres ps --all --quiet nats) || true
			if [[ -n ${container_id:-} ]]; then
				docker inspect --format '{{json .State}}' "${container_id}" || true
			fi
			exit 1
		fi
		address=$(NATS_PORT=0 compose_postgres port nats 4222)
		port=${address##*:}
		[[ -n ${port} ]] || { echo "failed to resolve the compose NATS port" >&2; exit 1; }
		NATS_URL="nats://127.0.0.1:${port}"
		export NATS_URL
	fi
	DATABASE_URL=$(compose_postgres_host_dsn)
	PGBOUNCER_DATABASE_URL=$(compose_pgbouncer_host_dsn)
	export DATABASE_URL PGBOUNCER_DATABASE_URL
elif [[ -z ${DATABASE_URL:-} || -z ${PGBOUNCER_DATABASE_URL:-} ]]; then
	echo "INTEGRATION_COMPOSE_MANAGED=1 requires DATABASE_URL and PGBOUNCER_DATABASE_URL" >&2
	exit 2
fi

if [[ ${reference} == true ]]; then
	python3 scripts/tests/jobs-reliability-reference.py --candidate "${JOBS_REFERENCE_CANDIDATE:-$(git rev-parse HEAD)}"
else
	cargo test --locked -p integration-tests --features integration "$@"
fi
