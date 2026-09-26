#!/usr/bin/env bash
# Static append-only check over migrations/: a change may add migration
# files, never modify, delete, or rename one, and every added version must be
# newer than the newest version the base already had. The runtime check in
# crates/migrate compares checksums against the database; this one fails the
# pull request before a rewritten file reaches any database.
#
#   migration-history-check.sh              worktree vs HEAD, untracked included
#   BASE_REF=<ref> migration-history-check.sh   merge-base of BASE_REF and HEAD
#   migration-history-check.sh --self-test
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

	# The fixture has no remote, so the caller's BASE_REF (make exports
	# origin/main) must not leak in; arguments are KEY=VALUE overrides.
	run() { env -u BASE_REF -u MIGRATION_DIR "MIGRATION_REPO_ROOT=${fixture}" "$@" bash "${script}"; }

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
	echo "migration history self-test passed"
}

if [[ ${1:-} == --self-test ]]; then
	self_test
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

idempotency_create="migrations/20260923000001_create_http_idempotency_records.sql"
jobs_create="migrations/20260924000001_create_background_jobs.sql"
jobs_simplify="migrations/20260925000001_simplify_background_jobs.sql"
idempotency_simplify="migrations/20260926001448_simplify_http_idempotency_records.sql"
transition_status=$(printf '%s\n' "${diff_status}" | awk -F'\t' -v first="${idempotency_create}" -v second="${jobs_create}" -v third="${jobs_simplify}" -v fourth="${idempotency_simplify}" '$2 == first || $2 == second || $2 == third || $2 == fourth')

# The only pre-adoption history rewrite accepted by this gate. It is exact by
# path and old/new blob identity, so a later edit, a partial squash, or a
# different rewrite still falls through to the append-only refusal below.
reviewed_pre_adoption_squash() {
	[[ ${migration_dir} == migrations ]] || return 1
	local expected_status=$'M\t'"${idempotency_create}"$'\nM\t'"${jobs_create}"$'\nD\t'"${jobs_simplify}"$'\nD\t'"${idempotency_simplify}"
	[[ ${transition_status} == "${expected_status}" ]] || return 1
	[[ $(git rev-parse "${base}:${idempotency_create}" 2>/dev/null) == 67545b94cfe4f58f931b24515801f6ab6e157720 ]] || return 1
	[[ $(git rev-parse "${base}:${jobs_create}" 2>/dev/null) == 167ca4d43efd05cd6e521a3818cf07a317005138 ]] || return 1
	[[ $(git rev-parse "${base}:${jobs_simplify}" 2>/dev/null) == 8e947805e66ce535f3853a08704fd70ff8e66a89 ]] || return 1
	[[ $(git rev-parse "${base}:${idempotency_simplify}" 2>/dev/null) == e98208f8aab9b15b6c369a16383fa4d1a549e7d1 ]] || return 1
	[[ -f ${idempotency_create} && -f ${jobs_create} && ! -e ${jobs_simplify} && ! -e ${idempotency_simplify} ]] || return 1
	[[ $(git hash-object "${idempotency_create}") == ca23412adb35422a42235979356b13358723bf85 ]] || return 1
	[[ $(git hash-object "${jobs_create}") == 600b30e77bcc5e6bdb0607b4f3d58fd2779cd874 ]] || return 1
	return 0
}

if reviewed_pre_adoption_squash; then
	# The exception clears only its four known rewrite/delete entries. New
	# migration files still pass through the normal ordering check below.
	changes=$(printf '%s\n' "${diff_status}" | awk -F'\t' -v first="${idempotency_create}" -v second="${jobs_create}" -v third="${jobs_simplify}" -v fourth="${idempotency_simplify}" '$2 != first && $2 != second && $2 != third && $2 != fourth && $1 != "A" && $1 != ""' | awk -F/ "${is_migration}" || true)
else
	changes=$(printf '%s\n' "${diff_status}" | awk -F'\t' '$1 != "A" && $1 != ""' | awk -F/ "${is_migration}" || true)
fi

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
