#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${ROOT_DIR}"

unset PGHOSTADDR PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE PGSSLMODE \
	PGSSLROOTCERT PGSSLCERT PGSSLKEY PGAPPNAME PGOPTIONS PGPASSFILE

check=()
case "${1:-}" in
'') ;;
--check) check=(--check) ;;
*) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

command -v cargo-sqlx >/dev/null 2>&1 || { echo "cargo-sqlx is required; run through make sqlx-prepare or make sqlx-check" >&2; exit 2; }
docker info >/dev/null 2>&1 || { echo "docker is required for sqlx prepare" >&2; exit 2; }

# shellcheck source=tools/versions.env
. tools/versions.env
driver=$(python3 -B -c '
import tomllib
with open("Cargo.toml", "rb") as f:
    s = tomllib.load(f).get("workspace", {}).get("dependencies", {}).get("sqlx")
print(s if isinstance(s, str) else (s.get("version", "") if isinstance(s, dict) else ""))
')
[[ ${driver} == "${SQLX_CLI_VERSION}" ]] || {
	echo "Cargo.toml pins sqlx ${driver:-<none>}, tools/versions.env pins sqlx-cli ${SQLX_CLI_VERSION}" >&2
	exit 2
}

project="sqlx-prepare-$$-$(date +%s)"
cleanup() {
	local rc=$?
	trap - EXIT INT TERM HUP
	POSTGRES_PORT=0 docker compose -p "${project}" -f "${ROOT_DIR}/env/docker-compose.yml" down -v --remove-orphans >/dev/null 2>&1 || true
	exit "${rc}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

POSTGRES_PORT=0 docker compose -p "${project}" -f "${ROOT_DIR}/env/docker-compose.yml" up -d --wait postgres
addr=$(POSTGRES_PORT=0 docker compose -p "${project}" -f "${ROOT_DIR}/env/docker-compose.yml" port postgres 5432)
port=${addr##*:}

export DATABASE_URL="postgres://app:app@127.0.0.1:${port}/sqlx_prepare_$$?sslmode=disable"
sqlx database create
sqlx migrate run --source migrations
SQLX_OFFLINE=false cargo sqlx prepare --workspace ${check[@]+"${check[@]}"} -- --locked
