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

trap compose_postgres_down EXIT INT TERM
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
