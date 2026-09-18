#!/usr/bin/env bash
# Database-backed proof: PostgreSQL from env/docker-compose.yml on an
# ephemeral port, the integration-tests crate with its `integration` feature
# and DATABASE_URL set, then teardown. Extra arguments go to `cargo test`.
#
#   test-integration-db.sh [cargo test args]
#   REQUIRE_DOCKER=1 makes a missing Docker a failure instead of a refusal.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"
# shellcheck source=scripts/lib/compose-postgres.sh
source scripts/lib/compose-postgres.sh

require_docker
trap compose_postgres_down EXIT INT TERM
compose_postgres_up service-db

DATABASE_URL=$(compose_postgres_host_dsn)
export DATABASE_URL
cargo test --locked -p integration-tests --features integration "$@"
