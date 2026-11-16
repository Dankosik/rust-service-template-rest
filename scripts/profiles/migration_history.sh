#!/usr/bin/env bash
set -euo pipefail

SCRIPT_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
ROOT_DIR="${MIGRATION_REPO_ROOT:-${SCRIPT_ROOT}}"
migration_dir="${MIGRATION_DIR:-migrations}"

if [[ ${1:-} == --self-test ]]; then
	exec python3 -m unittest "${SCRIPT_ROOT}/scripts/tests/test_migration_history.py"
fi
if [[ $# -gt 0 ]]; then
	echo "usage: migration_history.sh" >&2
	exit 2
fi

# Preserve the per-directory history algorithm and historical exceptions.
# Service configuration adds independent sets without weakening either check.
if [[ -z ${MIGRATION_DIR:-} ]]; then
	migration_directories=(migrations)
	service_migration_directories() { :; }
	if [[ -e ${ROOT_DIR}/scripts/ci/service-database.sh ]]; then
		echo "refusing legacy hook scripts/ci/service-database.sh; move to scripts/service/database.sh" >&2
		exit 2
	fi
	if [[ -f ${ROOT_DIR}/scripts/service/database.sh ]]; then
		# shellcheck source=/dev/null
		source "${ROOT_DIR}/scripts/service/database.sh"
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
	. "${SCRIPT_ROOT}/tools/versions.env"
	linter=(python3 "${SCRIPT_ROOT}/scripts/checks/npx.py" "squawk-cli@${SQUAWK_CLI_VERSION}")
fi
# The runner publishes lock_timeout and statement_timeout for the session, so
# a file does not set them; smallint is this template's type for a bounded
# count. PostgreSQL 14 is the oldest supported server.
linter+=(--pg-version 14 --exclude "require-lock-timeout,require-statement-timeout,prefer-bigint-over-smallint")
lint() {
	local status=0
	"${linter[@]}" "$@" || status=$?
	if ((status != 0)); then
		if ((status == 2)) && [[ -z ${MIGRATION_LINTER:-} ]]; then
			exit 2
		fi
		echo "migration lint: correct the migration, or put \`-- squawk-ignore <rule>\` with the reason above the statement" >&2
		exit 1
	fi
}
if ((${#transactional[@]} > 0)); then lint --assume-in-transaction "${transactional[@]}"; fi
if ((${#outside_transaction[@]} > 0)); then lint --no-assume-in-transaction "${outside_transaction[@]}"; fi
echo "migration lint passed (${#transactional[@]} transactional, ${#outside_transaction[@]} outside a transaction)"
