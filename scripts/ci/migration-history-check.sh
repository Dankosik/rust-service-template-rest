#!/usr/bin/env bash
# Static append-only check over migrations/: a change may add migration
# files, never modify, delete, or rename one, and every added version must be
# newer than the newest version the base already had. The runtime check in
# crates/migrate compares checksums against the database; this one fails the
# pull request before a rewritten file reaches any database.
#
# The files the change adds are then linted with Squawk for DDL that blocks
# or breaks a running service. Only additions are linted: a file the base
# already had may be applied somewhere and can no longer be corrected.
#
#   migration-history-check.sh              worktree vs HEAD, untracked included
#   BASE_REF=<ref> migration-history-check.sh   merge-base of BASE_REF and HEAD
#   migration-history-check.sh --self-test
#   MIGRATION_LINTER=<command> replaces the pinned `npx squawk-cli`.
set -euo pipefail

ROOT_DIR="${MIGRATION_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
migration_dir="${MIGRATION_DIR:-migrations}"

self_test() {
	local script
	script=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")
	fixture=$(mktemp -d -t migration-history.XXXXXX)
	trap 'rm -rf -- "${fixture}"' EXIT
	git -C "${fixture}" init -q
	git -C "${fixture}" config user.name migration-history
	git -C "${fixture}" config user.email migration-history@example.invalid
	mkdir -p "${fixture}/migrations"
	printf 'SELECT 1;\n' >"${fixture}/migrations/20260918000001_create.sql"
	printf 'SELECT 3;\n' >"${fixture}/migrations/20260918000003_extend.sql"
	printf '# notes\n' >"${fixture}/migrations/README.md"
	git -C "${fixture}" add .
	git -C "${fixture}" commit -qm baseline

	# The linter is replaced by a recorder, so the self-test needs no network
	# and still proves which files reach it and under which transaction mode.
	linter_log=$(mktemp -t migration-history-linter.XXXXXX)
	linter="${fixture}/.git/linter"
	printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >>"%s"\n' "${linter_log}" >"${linter}"
	chmod +x "${linter}"
	trap 'rm -rf -- "${fixture}" "${linter_log}"' EXIT

	# The fixture has no remote, so the caller's BASE_REF (make exports
	# origin/main) must not leak in; arguments are KEY=VALUE overrides.
	run() { env -u BASE_REF -u MIGRATION_DIR "MIGRATION_REPO_ROOT=${fixture}" "MIGRATION_LINTER=${linter}" "$@" bash "${script}"; }

	run >/dev/null || {
		echo "self-test: clean worktree failed" >&2
		exit 1
	}
	printf -- '-- rewritten\n' >>"${fixture}/migrations/20260918000001_create.sql"
	if run >/dev/null 2>&1; then
		echo "self-test: rewrite passed" >&2
		exit 1
	fi
	git -C "${fixture}" restore migrations/20260918000001_create.sql
	printf '# more notes\n' >>"${fixture}/migrations/README.md"
	run >/dev/null || {
		echo "self-test: editing a non-migration file failed" >&2
		exit 1
	}
	printf 'SELECT 4;\n' >"${fixture}/migrations/20260918000004_add.sql"
	run >/dev/null || {
		echo "self-test: untracked newer addition failed" >&2
		exit 1
	}
	[[ $(cat "${linter_log}") == *"--assume-in-transaction migrations/20260918000004_add.sql" ]] || {
		echo "self-test: the added migration did not reach the linter as transactional" >&2
		exit 1
	}
	if run MIGRATION_LINTER=false >/dev/null 2>&1; then
		echo "self-test: a linter finding passed" >&2
		exit 1
	fi
	printf -- '-- no-transaction\nSELECT 5;\n' >"${fixture}/migrations/20260918000005_online.sql"
	: >"${linter_log}"
	run >/dev/null || {
		echo "self-test: untracked no-transaction addition failed" >&2
		exit 1
	}
	[[ $(cat "${linter_log}") == *"--assume-in-transaction migrations/20260918000004_add.sql"*"--no-assume-in-transaction migrations/20260918000005_online.sql" ]] || {
		echo "self-test: migrations were not linted under their own transaction mode" >&2
		exit 1
	}
	rm "${fixture}/migrations/20260918000005_online.sql"
	git -C "${fixture}" add migrations/20260918000004_add.sql
	run >/dev/null || {
		echo "self-test: staged newer addition failed" >&2
		exit 1
	}
	printf 'SELECT 2;\n' >"${fixture}/migrations/20260918000002_late.sql"
	if run >/dev/null 2>&1; then
		echo "self-test: out-of-order addition passed" >&2
		exit 1
	fi
	rm "${fixture}/migrations/20260918000002_late.sql"
	git -C "${fixture}" rm -q migrations/20260918000003_extend.sql
	if run >/dev/null 2>&1; then
		echo "self-test: deletion passed" >&2
		exit 1
	fi
	git -C "${fixture}" restore --staged --worktree migrations/20260918000003_extend.sql
	git -C "${fixture}" commit -qam addition
	run BASE_REF=HEAD~1 >/dev/null || {
		echo "self-test: merge-base addition failed" >&2
		exit 1
	}
	# A file added and then amended inside the same range is still an
	# addition relative to the base; a file the base already had is not.
	printf -- '-- amended before merge\n' >>"${fixture}/migrations/20260918000004_add.sql"
	git -C "${fixture}" commit -qam amend
	run BASE_REF=HEAD~2 >/dev/null || {
		echo "self-test: amending a migration added in the same range failed" >&2
		exit 1
	}
	printf -- '-- rewritten\n' >>"${fixture}/migrations/20260918000001_create.sql"
	git -C "${fixture}" commit -qam rewrite
	if run BASE_REF=HEAD~1 >/dev/null 2>&1; then
		echo "self-test: merge-base rewrite passed" >&2
		exit 1
	fi
	if run MIGRATION_DIR=missing >/dev/null 2>&1; then
		echo "self-test: missing directory passed" >&2
		exit 1
	fi

	# A configured second history must get the same append-only and lint checks;
	# a clean primary history cannot hide its rewrite or new SQL.
	mkdir -p "${fixture}/migrations-secondary"
	mkdir -p "${fixture}/scripts/ci"
	cat >"${fixture}/scripts/ci/service-database.sh" <<'HOOK'
service_migration_directories() {
	migration_directories=(migrations migrations-secondary)
}
HOOK
	printf 'SELECT 1;\n' >"${fixture}/migrations-secondary/20261020000000_create.sql"
	git -C "${fixture}" add migrations-secondary
	git -C "${fixture}" commit -qm secondary-baseline
	run >/dev/null || { echo "self-test: clean independent histories failed" >&2; exit 1; }
	printf 'SELECT 2;\n' >>"${fixture}/migrations-secondary/20261020000000_create.sql"
	if run >/dev/null 2>&1; then
		echo "self-test: default entry admitted a secondary history rewrite" >&2
		exit 1
	fi
	git -C "${fixture}" restore migrations-secondary
	printf 'SELECT 3;\n' >"${fixture}/migrations-secondary/20261021000000_extend.sql"
	: >"${linter_log}"
	run >/dev/null || { echo "self-test: new secondary migration failed" >&2; exit 1; }
	[[ $(cat "${linter_log}") == *"migrations-secondary/20261021000000_extend.sql"* ]] || {
		echo "self-test: secondary migration did not reach the linter" >&2
		exit 1
	}
	echo "migration history self-test passed"
}

if [[ ${1:-} == --self-test ]]; then
	self_test
	exit 0
fi

# Preserve the per-directory history algorithm and historical exceptions.
# Service configuration adds independent sets without weakening either check.
if [[ -z ${MIGRATION_DIR:-} ]]; then
	migration_directories=(migrations)
	service_migration_directories() { :; }
	if [[ -f ${ROOT_DIR}/scripts/ci/service-database.sh ]]; then
		# shellcheck source=/dev/null
		source "${ROOT_DIR}/scripts/ci/service-database.sh"
	fi
	service_migration_directories
	for directory in "${migration_directories[@]}"; do
		MIGRATION_DIR=${directory} bash "${BASH_SOURCE[0]}"
	done
	exit 0
fi

cd "${ROOT_DIR}"
if [[ ! -d ${migration_dir} ]]; then
	echo "migration history: configured directory does not exist: ${migration_dir}" >&2
	exit 2
fi

if [[ -n ${BASE_REF:-} ]]; then
	git cat-file -e "${BASE_REF}^{commit}" 2>/dev/null || {
		echo "migration history: BASE_REF ${BASE_REF} is not a readable commit" >&2
		exit 1
	}
	# A shallow CI checkout has no merge base; there HEAD is the pull
	# request's merge commit, so the base tip itself is the right comparison.
	if base=$(git merge-base "${BASE_REF}" HEAD 2>/dev/null); then
		scope="merge-base:${base}"
	else
		base=$(git rev-parse "${BASE_REF}^{commit}")
		scope="base:${base}"
	fi
	include_untracked=false
else
	base=HEAD
	scope=worktree
	include_untracked=true
fi

# An awk pattern, expanded by awk and not by the shell.
# shellcheck disable=SC2016
is_migration='$NF ~ /^[0-9]+_.+\.sql$/'
diff_status=$(git diff --name-status --find-renames --find-copies "${base}" -- "${migration_dir}/")

changes=$(printf '%s\n' "${diff_status}" | awk -F'\t' '$1 != "A" && $1 != ""' | awk -F/ "${is_migration}" || true)

if [[ -n ${changes} ]]; then
	echo "migration history: applied migration files are append-only; only additions are allowed (${scope})" >&2
	printf '%s\n' "${changes}" >&2
	exit 1
fi

prior_max=$(
	git ls-tree -r --name-only "${base}" -- "${migration_dir}/" |
		LC_ALL=C awk -F/ "${is_migration}"' {
			name = $NF
			sub(/_.*/, "", name)
			if (name + 0 > max) max = name + 0
		}
		END { print max + 0 }'
)
added=$(printf '%s\n' "${diff_status}" | awk -F'\t' '$1 == "A" { print $2 }')
if [[ ${include_untracked} == true ]]; then
	untracked=$(git ls-files --others --exclude-standard -- "${migration_dir}/")
	added="${added}${added:+$'\n'}${untracked}"
fi
out_of_order=$(
	printf '%s\n' "${added}" |
		LC_ALL=C awk -F/ -v max="${prior_max}" "${is_migration}"' {
			name = $NF
			sub(/_.*/, "", name)
			if (name + 0 <= max) print
		}'
)
if [[ -n ${out_of_order} ]]; then
	echo "migration history: new migrations must be newer than prior version ${prior_max} (${scope})" >&2
	printf '%s\n' "${out_of_order}" >&2
	exit 1
fi

echo "migration history is append-only (${scope})"

# sqlx runs a file outside a transaction when it starts with this marker.
transactional=()
outside_transaction=()
while IFS= read -r file; do
	[[ -f ${file} ]] || continue
	if [[ $(head -c 17 "${file}") == '-- no-transaction' ]]; then
		outside_transaction+=("${file}")
	else
		transactional+=("${file}")
	fi
done < <(printf '%s\n' "${added}" | LC_ALL=C awk -F/ "${is_migration}"' { print }' | LC_ALL=C sort)
if ((${#transactional[@]} + ${#outside_transaction[@]} == 0)); then
	exit 0
fi

if [[ -n ${MIGRATION_LINTER:-} ]]; then
	read -r -a linter <<<"${MIGRATION_LINTER}"
else
	# shellcheck source=tools/versions.env
	. "$(dirname "${BASH_SOURCE[0]}")/../../tools/versions.env"
	command -v npx >/dev/null 2>&1 || {
		echo "migration lint requires Node.js (npx) for squawk-cli@${SQUAWK_CLI_VERSION}" >&2
		exit 2
	}
	linter=(env npm_config_prefer_offline=true npx --yes "squawk-cli@${SQUAWK_CLI_VERSION}")
fi
# The runner publishes lock_timeout and statement_timeout for the session, so
# a file does not set them; smallint is this template's type for a bounded
# count. PostgreSQL 14 is the oldest supported server.
linter+=(--pg-version 14 --exclude "require-lock-timeout,require-statement-timeout,prefer-bigint-over-smallint")
lint() {
	"${linter[@]}" "$@" || {
		echo "migration lint: correct the migration, or put \`-- squawk-ignore <rule>\` with the reason above the statement" >&2
		exit 1
	}
}
if ((${#transactional[@]} > 0)); then lint --assume-in-transaction "${transactional[@]}"; fi
if ((${#outside_transaction[@]} > 0)); then lint --no-assume-in-transaction "${outside_transaction[@]}"; fi
echo "migration lint passed (${#transactional[@]} transactional, ${#outside_transaction[@]} outside a transaction)"
