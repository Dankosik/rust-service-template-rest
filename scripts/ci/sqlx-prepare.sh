#!/usr/bin/env bash
# Query metadata for `sqlx::query!`: describes every checked statement in the
# selected compilation groups against their independent migration sets.
# Groups write workspace or native package-local caches.
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

primary_server=${DATABASE_URL:-}
if [[ ${INTEGRATION_COMPOSE_MANAGED:-} == 1 && -z ${primary_server} ]]; then
	echo "INTEGRATION_COMPOSE_MANAGED=1 requires DATABASE_URL" >&2
	exit 2
fi

# The default group prepares the workspace against migrations/. A service
# extension may declare further groups, each with its own server and migration
# set, either the workspace or package directories with package-local .sqlx/
# caches, and extra `cargo check` arguments (features, targets, --workspace
# --exclude). Groups never share a schema or merge generated query files.
sqlx_groups=(default)
service_sqlx_init() { :; }
service_sqlx_group() {
	server_url=${primary_server}
	migration_source=migrations
	package_dirs=()
	cargo_check_args=()
}
if [[ -f scripts/ci/service-database.sh ]]; then
	# shellcheck source=/dev/null
	source scripts/ci/service-database.sh
fi
service_sqlx_init

for group in "${sqlx_groups[@]}"; do
	service_sqlx_group "${group}"
	if [[ ${INTEGRATION_COMPOSE_MANAGED:-} != 1 || -z ${server_url} ]]; then
		require_docker
		compose_postgres_up "sqlx-prepare-${group}"
		server_url=$(compose_postgres_host_dsn)
	fi

	# The same server, another database: the suite's own database stays as the
	# tests expect it, and a rerun never meets an earlier run's schema.
	[[ ${server_url} =~ ^(.*/)[^/?]+(\?.*)?$ ]] || {
		echo "DATABASE_URL names no database (value redacted)" >&2
		exit 2
	}
	DATABASE_URL="${BASH_REMATCH[1]}sqlx_prepare_${group}_$$${BASH_REMATCH[2]}"
	export DATABASE_URL

	own_database=true
	sqlx database create
	sqlx migrate run --source "${migration_source}"
	# The repository builds offline (.cargo/config.toml); this run is the one
	# that asks the database.
	if ((${#package_dirs[@]} == 0)); then
		SQLX_OFFLINE=false cargo sqlx prepare --workspace ${check[@]+"${check[@]}"} -- \
			--locked ${cargo_check_args[@]+"${cargo_check_args[@]}"}
	else
		for package_dir in "${package_dirs[@]}"; do
			(
				cd "${ROOT_DIR}/${package_dir}"
				SQLX_OFFLINE=false cargo sqlx prepare ${check[@]+"${check[@]}"} -- \
					--locked ${cargo_check_args[@]+"${cargo_check_args[@]}"}
			)
		done
	fi
	sqlx database drop -y
	own_database=false
	compose_postgres_down
done
