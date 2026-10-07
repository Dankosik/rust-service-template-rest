#!/usr/bin/env bash
# Query metadata for `sqlx::query!`: describes every checked statement in the
# workspace against a database that holds exactly the embedded migration set
# and writes the result to .sqlx/, which offline builds read.
#
#   sqlx-prepare.sh           regenerate .sqlx/
#   sqlx-prepare.sh --check   fail when .sqlx/ differs from what the
#                             statements and the migrations produce now
#
# PostgreSQL comes from env/docker-compose.yml on an ephemeral port, or from
# DATABASE_URL when the caller owns the compose lifecycle
# (INTEGRATION_COMPOSE_MANAGED=1). Either way the work happens in a database
# of its own, created here and dropped on exit. `cargo sqlx` (sqlx-cli at the
# version tools/versions.env pins) must be on PATH; `make sqlx-prepare` and
# `make sqlx-check` put it there.
#   REQUIRE_DOCKER=1 makes a missing Docker a failure instead of a refusal.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"
if ! bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" --assert-held; then
	exec bash "${ROOT_DIR}/scripts/ci/validation-lock.sh" -- bash "${ROOT_DIR}/scripts/ci/sqlx-prepare.sh" "$@"
fi
# shellcheck source=scripts/lib/compose-postgres.sh
source scripts/lib/compose-postgres.sh

check=()
case "${1:-}" in
'') ;;
--check) check=(--check) ;;
*)
	echo "usage: sqlx-prepare.sh [--check]" >&2
	exit 2
	;;
esac

command -v cargo-sqlx >/dev/null 2>&1 || {
	echo "cargo-sqlx is not on PATH; run through make sqlx-prepare or make sqlx-check" >&2
	exit 2
}

# The metadata format belongs to the driver and the tool alike.
# shellcheck source=tools/versions.env
. tools/versions.env
driver=$(sed -n 's/^sqlx = { version = "\([^"]*\)".*/\1/p' Cargo.toml)
[[ ${driver} == "${SQLX_CLI_VERSION}" ]] || {
	echo "Cargo.toml pins sqlx ${driver:-<none>}, tools/versions.env pins sqlx-cli ${SQLX_CLI_VERSION}; move them together" >&2
	exit 2
}

# Nothing to drop or stop until the steps below created it.
own_database=false
cleanup() {
	local status=$?
	trap - EXIT INT TERM HUP
	if [[ ${own_database} == true ]]; then
		if ! sqlx database drop -y >/dev/null 2>&1; then
			echo "SQLx temporary database cleanup incomplete" >&2
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

if [[ ${INTEGRATION_COMPOSE_MANAGED:-} != 1 ]]; then
	require_docker
	compose_postgres_up sqlx-prepare
	server_url=$(compose_postgres_host_dsn)
elif [[ -n ${DATABASE_URL:-} ]]; then
	server_url=${DATABASE_URL}
else
	echo "INTEGRATION_COMPOSE_MANAGED=1 requires DATABASE_URL" >&2
	exit 2
fi

# The same server, another database: the suite's own database stays as the
# tests expect it, and a rerun never meets an earlier run's schema.
[[ ${server_url} =~ ^(.*/)[^/?]+(\?.*)?$ ]] || {
	echo "DATABASE_URL names no database (value redacted)" >&2
	exit 2
}
DATABASE_URL="${BASH_REMATCH[1]}sqlx_prepare_$$${BASH_REMATCH[2]}"
export DATABASE_URL

own_database=true
sqlx database create
sqlx migrate run --source migrations
# The repository builds offline (.cargo/config.toml); this run is the one
# that asks the database.
SQLX_OFFLINE=false cargo sqlx prepare --workspace ${check[@]+"${check[@]}"} -- --locked
