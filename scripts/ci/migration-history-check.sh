#!/usr/bin/env bash
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
	printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$@" >>"%s"\n' "${linter_log}" >"${linter}"
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
	local pair old new refusal
	for pair in '20260918000001_create.sql:former migration.txt' 'README.md:20260918000004_from notes.sql'; do
		old=migrations/${pair%:*}
		new=migrations/${pair#*:}
		git -C "${fixture}" mv "${old}" "${new}"
		if refusal=$(run 2>&1) || [[ ${refusal} != *'applied migration files are append-only'* ]]; then
			echo "self-test: rename ${old} to ${new} did not refuse the history change" >&2
			exit 1
		fi
		git -C "${fixture}" mv "${new}" "${old}"
	done
	local copy='migrations/20260918000004_copy with space.sql'
	cp "${fixture}/migrations/README.md" "${fixture}/${copy}"
	git -C "${fixture}" add "${copy}"
	run >/dev/null || { echo 'self-test: copy classified as an addition failed' >&2; exit 1; }
	[[ $(cat "${linter_log}") == *$'--assume-in-transaction\n'"${copy}"* ]] || {
		echo 'self-test: copy classified as an addition missed lint' >&2
		exit 1
	}
	printf '# more notes\n' >>"${fixture}/migrations/README.md"
	# With its source modified, Git now reports the same destination as C100.
	if refusal=$(run 2>&1) || [[ ${refusal} != *'applied migration files are append-only'* || ${refusal} != *C100* ]]; then
		echo 'self-test: a detected copy into the migration history escaped the copy policy' >&2
		exit 1
	fi
	git -C "${fixture}" restore --staged -- "${copy}"
	rm "${fixture}/${copy}"
	run >/dev/null || {
		echo "self-test: editing a non-migration file failed" >&2
		exit 1
	}
	printf 'SELECT 4;\n' >"${fixture}/migrations/20260918000004_add.sql"
	run >/dev/null || {
		echo "self-test: untracked newer addition failed" >&2
		exit 1
	}
	[[ $(cat "${linter_log}") == *$'--assume-in-transaction\nmigrations/20260918000004_add.sql' ]] || {
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
	[[ $(cat "${linter_log}") == *$'--assume-in-transaction\nmigrations/20260918000004_add.sql'*$'--no-assume-in-transaction\nmigrations/20260918000005_online.sql' ]] || {
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
	printf 'SELECT 6;\n' >"${fixture}/migrations/20260918000006_duplicate_a.sql"
	printf 'SELECT 6;\n' >"${fixture}/migrations/20260918000006_duplicate_b.sql"
	if run >/dev/null 2>&1; then
		echo "self-test: duplicate new versions passed" >&2
		exit 1
	fi
	rm "${fixture}/migrations/20260918000006_duplicate_a.sql" "${fixture}/migrations/20260918000006_duplicate_b.sql"
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

	local bounds="${fixture}/bounds-repo"
	git -C "${fixture}" init -q "${bounds}"
	git -C "${bounds}" config user.name migration-history
	git -C "${bounds}" config user.email migration-history@example.invalid
	mkdir -p "${bounds}/migrations"
	printf '# empty baseline\n' >"${bounds}/migrations/README.md"
	git -C "${bounds}" add .
	git -C "${bounds}" commit -qm empty-baseline
	run_bounds() { env -u BASE_REF -u MIGRATION_DIR "MIGRATION_REPO_ROOT=${bounds}" "MIGRATION_LINTER=${linter}" bash "${script}"; }

	printf 'SELECT 0;\n' >"${bounds}/migrations/0_init.sql"
	run_bounds >/dev/null || {
		echo "self-test: initial 0_init.sql on an empty base failed" >&2
		exit 1
	}
	printf 'SELECT 1;\n' >"${bounds}/migrations/01_a.sql"
	printf 'SELECT 1;\n' >"${bounds}/migrations/1_b.sql"
	if run_bounds >/dev/null 2>&1; then
		echo "self-test: 01_a.sql vs 1_b.sql integer duplicate passed" >&2
		exit 1
	fi
	rm "${bounds}/migrations/01_a.sql" "${bounds}/migrations/1_b.sql"
	printf 'SELECT 1;\n' >"${bounds}/migrations/9223372036854775806_near_max.sql"
	git -C "${bounds}" add migrations/0_init.sql migrations/9223372036854775806_near_max.sql
	git -C "${bounds}" commit -qm near-max
	printf 'SELECT 2;\n' >"${bounds}/migrations/9223372036854775807_i64_max.sql"
	run_bounds >/dev/null || {
		echo "self-test: 19-digit i64::MAX migration after i64::MAX-1 failed" >&2
		exit 1
	}
	rm "${bounds}/migrations/9223372036854775807_i64_max.sql"
	printf 'SELECT 3;\n' >"${bounds}/migrations/9223372036854775808_overflow.sql"
	if run_bounds >/dev/null 2>&1; then
		echo "self-test: > i64::MAX migration version passed" >&2
		exit 1
	fi
	rm "${bounds}/migrations/9223372036854775808_overflow.sql"
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

is_migration() {
	[[ ${1##*/} =~ ^[0-9]+_.+\.sql$ ]]
}

temporary=$(mktemp -d)
trap 'rm -rf -- "${temporary}"' EXIT
git diff --name-status -z --find-renames --find-copies "${base}" -- "${migration_dir}/" >"${temporary}/status"
incomplete_record() {
	echo 'migration history: incomplete Git name-status record' >&2
	exit 1
}
while IFS= read -r -d '' status || [[ -n ${status} ]]; do
	IFS= read -r -d '' path || incomplete_record
	case ${status} in
	R* | C*)
		IFS= read -r -d '' destination || incomplete_record
		if { [[ ${status} == R* ]] && is_migration "${path}"; } || is_migration "${destination}"; then
			printf '%s\t%s\t%s\n' "${status}" "${path}" "${destination}" >>"${temporary}/refused"
		fi
		;;
	A) printf '%s\0' "${path}" ;;
	*)
		if is_migration "${path}"; then printf '%s\t%s\n' "${status}" "${path}" >>"${temporary}/refused"; fi
		;;
	esac
done <"${temporary}/status" >"${temporary}/added"
if [[ -s ${temporary}/refused ]]; then
	echo "migration history: applied migration files are append-only; only additions are allowed (${scope})" >&2
	cat "${temporary}/refused" >&2
	exit 1
fi

git ls-tree -r --name-only -z "${base}" -- "${migration_dir}/" >"${temporary}/base"
if [[ ${include_untracked} == true ]]; then
	git ls-files -z --others --exclude-standard -- "${migration_dir}/" >>"${temporary}/added"
fi
env -u PYTHONPATH -u PYTHONHOME PYTHONDONTWRITEBYTECODE=1 python3 -B - "${temporary}/base" "${temporary}/added" "${scope}" <<'PY'
import os
import re
import shlex
import sys
from pathlib import Path

MAX_I64 = (1 << 63) - 1
MIGRATION_RE = re.compile(r"^([0-9]+)_.+\.sql$")

base_path, added_path, scope = sys.argv[1], sys.argv[2], sys.argv[3]


def read_entries(path: str) -> list[tuple[int, str]]:
    raw = Path(path).read_bytes()
    seen_paths: set[bytes] = set()
    entries: list[tuple[int, str]] = []
    for item in raw.split(b"\0"):
        if not item or item in seen_paths:
            continue
        seen_paths.add(item)
        text = os.fsdecode(item)
        name = text.rsplit("/", 1)[-1]
        match = MIGRATION_RE.match(name)
        if not match:
            continue
        entries.append((int(match.group(1), 10), text))
    return entries


base_entries = read_entries(base_path)
added_entries = read_entries(added_path)

overflow = [path for ver, path in (*base_entries, *added_entries) if not (0 <= ver <= MAX_I64)]
if overflow:
    print(
        f"migration history: migration version must fit in signed 64-bit integer 0..={MAX_I64} ({scope})",
        file=sys.stderr,
    )
    for path in overflow:
        print(shlex.quote(path), file=sys.stderr)
    sys.exit(1)

prior_max = max((ver for ver, _ in base_entries), default=None)
if prior_max is not None:
    out_of_order = [path for ver, path in added_entries if ver <= prior_max]
    if out_of_order:
        print(
            f"migration history: new migrations must be newer than prior version {prior_max} ({scope})",
            file=sys.stderr,
        )
        for path in out_of_order:
            print(shlex.quote(path), file=sys.stderr)
        sys.exit(1)

by_version: dict[int, list[str]] = {}
for ver, path in added_entries:
    by_version.setdefault(ver, []).append(path)
duplicates = {ver: paths for ver, paths in by_version.items() if len(paths) > 1}
if duplicates:
    print(f"migration history: new migration versions must be unique ({scope})", file=sys.stderr)
    for ver in sorted(duplicates):
        print(f"{ver}\t" + " ".join(shlex.quote(p) for p in duplicates[ver]), file=sys.stderr)
    sys.exit(1)
PY

echo "migration history is append-only (${scope})"

# sqlx runs a file outside a transaction when it starts with this marker.
transactional=()
outside_transaction=()
LC_ALL=C sort -z "${temporary}/added" >"${temporary}/sorted"
while IFS= read -r -d '' file; do
	is_migration "${file}" || continue
	[[ -f ${file} ]] || continue
	if [[ $(head -c 17 "${file}") == '-- no-transaction' ]]; then
		outside_transaction+=("${file}")
	else
		transactional+=("${file}")
	fi
done <"${temporary}/sorted"
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
	npx_cooldown_days=7
	npx_before=$(env -u PYTHONPATH -u PYTHONHOME PYTHONDONTWRITEBYTECODE=1 python3 -B -c "import datetime as d; print((d.datetime.now(d.timezone.utc)-d.timedelta(days=${npx_cooldown_days})).strftime('%Y-%m-%dT%H:%M:%SZ'))")
	linter=(
		env
		npm_config_prefer_offline=true
		npm_config_ignore_scripts=true
		npm_config_audit=false
		npm_config_fund=false
		npm_config_update_notifier=false
		"npm_config_before=${npx_before}"
		npx --yes "squawk-cli@${SQUAWK_CLI_VERSION}"
	)
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
