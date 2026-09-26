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

	# The reviewed pre-adoption exceptions are exact endpoint pairs. Keep their
	# byte fixtures here so this harness works in derived profiles and shallow
	# repositories that do not carry these migrations or their history objects.
	local jobs_create_path="migrations/20260924000001_create_background_jobs.sql"
	local idempotency_create_path="migrations/20260923000001_create_http_idempotency_records.sql"
	local jobs_simplify_path="migrations/20260925000001_simplify_background_jobs.sql"
	local idempotency_simplify_path="migrations/20260926001448_simplify_http_idempotency_records.sql"
	# EXIT runs after this function returns; keep its cleanup paths in script scope.
	endpoint_fixture=$(mktemp -d -t migration-history-endpoint.XXXXXX)
	transition_fixture=$(mktemp -d -t migration-history-transition.XXXXXX)
	trap 'rm -rf -- "${fixture}" "${endpoint_fixture}" "${transition_fixture}"' EXIT
	write_fixture() { mkdir -p "$(dirname "$1")" && cat >"$1"; }
	assert_fixture_hash() {
		[[ $(git hash-object "$1") == "$2" ]] || {
			echo "self-test: endpoint fixture hash is stale: $1" >&2
			exit 1
		}
	}
	write_jobs_consolidated() { write_fixture "$1" <<'SQL'; }
DO $$
BEGIN
    IF current_setting('server_encoding') <> 'UTF8' THEN
        RAISE EXCEPTION 'background jobs require a UTF8 database encoding';
    END IF;
END $$;

-- Durable background jobs. Only crates/infra-jobs names this table. A job is
-- live while pending or running; completed and failed jobs are terminal.
CREATE TABLE background_jobs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    kind text NOT NULL,
    payload jsonb NOT NULL,
    unique_key text COLLATE "C",
    state text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'running', 'completed', 'failed')),
    failure_reason text CHECK (failure_reason IN ('permanent', 'exhausted')),
    attempts smallint NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claim_generation bigint NOT NULL DEFAULT 0 CHECK (claim_generation >= 0),  -- 0: never claimed
    not_before timestamptz NOT NULL,
    claim_expires_at timestamptz,
    finished_at timestamptz,
    error_summary text,
    trace_context text,
    trace_state text,
    CONSTRAINT background_jobs_claim_matches_state
        CHECK ((state = 'running') = (claim_expires_at IS NOT NULL)),
    CONSTRAINT background_jobs_reason_matches_state
        CHECK ((state = 'failed') = (failure_reason IS NOT NULL)),
    CONSTRAINT background_jobs_finished_matches_state
        CHECK ((state IN ('completed', 'failed')) = (finished_at IS NOT NULL))
);

-- Every claim draws its fencing token here, so no value is ever reused, not even
-- after a rolled-back claim.
CREATE SEQUENCE background_jobs_claim_generation AS bigint
    OWNED BY background_jobs.claim_generation;

-- At most one live job per kind and unique key; a terminal job frees it.
CREATE UNIQUE INDEX background_jobs_live_unique_key ON background_jobs (kind, unique_key)
    WHERE unique_key IS NOT NULL AND state IN ('pending', 'running');
-- Claim order per registered kind.
CREATE INDEX background_jobs_pending ON background_jobs (kind, not_before, id)
    WHERE state = 'pending';
-- Expired claims and claim upkeep.
CREATE INDEX background_jobs_running ON background_jobs (kind, claim_expires_at, not_before, id)
    WHERE state = 'running';
-- Retention.
CREATE INDEX background_jobs_terminal ON background_jobs (state, finished_at)
    WHERE state IN ('completed', 'failed');
SQL
	write_jobs_old() { write_fixture "$1" <<'SQL'; }
-- Durable background jobs (docs/background-jobs.md). The jobs pack's one
-- table: only crates/infra-jobs names it. A job is live while pending or
-- running; completed and failed jobs are terminal and deleted by retention.
CREATE TABLE background_jobs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    kind text NOT NULL,
    payload bytea NOT NULL,
    unique_key bytea,
    state text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'running', 'completed', 'failed')),
    failure_reason text CHECK (failure_reason IN ('permanent', 'exhausted')),
    attempts smallint NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claim_generation bigint NOT NULL DEFAULT 0 CHECK (claim_generation >= 0),  -- 0: never claimed
    not_before timestamptz NOT NULL,
    claim_expires_at timestamptz,
    finished_at timestamptz,
    error_summary text,
    trace_context text,
    CONSTRAINT background_jobs_claim_matches_state
        CHECK ((state = 'running') = (claim_expires_at IS NOT NULL)),
    CONSTRAINT background_jobs_reason_matches_state
        CHECK ((state = 'failed') = (failure_reason IS NOT NULL)),
    CONSTRAINT background_jobs_finished_matches_state
        CHECK ((state IN ('completed', 'failed')) = (finished_at IS NOT NULL))
);

-- Every claim draws its fencing token here, so no value is ever reused, not even
-- after a rolled-back claim.
CREATE SEQUENCE background_jobs_claim_generation AS bigint
    OWNED BY background_jobs.claim_generation;

-- At most one live job per kind and unique key; a terminal job frees it.
CREATE UNIQUE INDEX background_jobs_live_unique_key ON background_jobs (kind, unique_key)
    WHERE unique_key IS NOT NULL AND state IN ('pending', 'running');
-- Claim order per registered kind.
CREATE INDEX background_jobs_pending ON background_jobs (kind, not_before, id)
    WHERE state = 'pending';
-- Expired claims and claim upkeep.
CREATE INDEX background_jobs_running ON background_jobs (claim_expires_at)
    WHERE state = 'running';
-- Retention.
CREATE INDEX background_jobs_terminal ON background_jobs (state, finished_at)
    WHERE state IN ('completed', 'failed');
SQL
	write_jobs_new() { write_fixture "$1" <<'SQL'; }
DO $$
BEGIN
    IF current_setting('server_encoding') <> 'UTF8' THEN
        RAISE EXCEPTION 'background jobs require a UTF8 database encoding';
    END IF;
END $$;

-- Durable background jobs. Only crates/infra-jobs names this table. A job is
-- live while pending or running; completed and failed jobs are terminal.
CREATE TABLE background_jobs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    created_at timestamptz NOT NULL DEFAULT statement_timestamp(),
    kind text NOT NULL,
    payload jsonb NOT NULL,
    unique_key text COLLATE "C",
    state text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'running', 'completed', 'failed')),
    failure_reason text CHECK (failure_reason IN ('permanent', 'exhausted')),
    attempts smallint NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claim_generation bigint NOT NULL DEFAULT 0 CHECK (claim_generation >= 0),  -- 0: never claimed
    not_before timestamptz NOT NULL,
    claim_expires_at timestamptz,
    attempted_by uuid,
    finished_at timestamptz,
    error_summary text,
    trace_context text,
    trace_state text,
    CONSTRAINT background_jobs_claim_matches_state
        CHECK ((state = 'running') = (claim_expires_at IS NOT NULL)),
    CONSTRAINT background_jobs_reason_matches_state
        CHECK ((state = 'failed') = (failure_reason IS NOT NULL)),
    CONSTRAINT background_jobs_finished_matches_state
        CHECK ((state IN ('completed', 'failed')) = (finished_at IS NOT NULL))
) WITH (
    autovacuum_vacuum_scale_factor = 0,
    autovacuum_vacuum_threshold = 5000
);

-- Every claim draws its fencing token here, so no value is ever reused, not even
-- after a rolled-back claim.
CREATE SEQUENCE background_jobs_claim_generation AS bigint
    OWNED BY background_jobs.claim_generation;

-- At most one live job per kind and unique key; a terminal job frees it.
CREATE UNIQUE INDEX background_jobs_live_unique_key ON background_jobs (kind, unique_key)
    WHERE unique_key IS NOT NULL AND state IN ('pending', 'running');
-- Claim order per registered kind.
CREATE INDEX background_jobs_pending ON background_jobs (kind, not_before, id)
    WHERE state = 'pending';
-- Expired claims and claim upkeep.
CREATE INDEX background_jobs_running ON background_jobs (kind, claim_expires_at, not_before, id)
    WHERE state = 'running';
-- Retention.
CREATE INDEX background_jobs_terminal ON background_jobs (state, finished_at)
    WHERE state IN ('completed', 'failed');
SQL
	write_idempotency_old() { write_fixture "$1" <<'SQL'; }
CREATE TABLE http_idempotency_records (
    scope_key   bytea       NOT NULL PRIMARY KEY CHECK (octet_length(scope_key) = 32),
    fingerprint bytea       NOT NULL CHECK (octet_length(fingerprint) = 32),
    format      smallint    NOT NULL CHECK (format > 0),
    status      smallint    NOT NULL CHECK (status BETWEEN 200 AND 299),
    headers     bytea       NOT NULL,
    body        bytea       NOT NULL,
    expires_at  timestamptz NOT NULL
);
CREATE INDEX http_idempotency_records_expires_at ON http_idempotency_records (expires_at);
SQL
	write_jobs_simplify() { write_fixture "$1" <<'SQL'; }
DO $$
BEGIN
    IF current_setting('server_encoding') <> 'UTF8' THEN
        RAISE EXCEPTION 'background jobs require a UTF8 database encoding';
    END IF;
END $$;

ALTER TABLE background_jobs
    ALTER COLUMN payload TYPE jsonb USING convert_from(payload, 'UTF8')::jsonb,
    ALTER COLUMN unique_key TYPE text COLLATE "C" USING convert_from(unique_key, 'UTF8'),
    ADD COLUMN trace_state text;

DROP INDEX background_jobs_running;

CREATE INDEX background_jobs_running
    ON background_jobs (kind, claim_expires_at, not_before, id)
    WHERE state = 'running';
SQL
	write_idempotency_simplify() { write_fixture "$1" <<'SQL'; }
LOCK TABLE http_idempotency_records IN ACCESS EXCLUSIVE MODE;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM http_idempotency_records
        WHERE expires_at > clock_timestamp()
    ) THEN
        RAISE EXCEPTION 'http idempotency simplification refuses live records';
    END IF;
END
$$;

DELETE FROM http_idempotency_records
WHERE expires_at <= clock_timestamp();

CREATE TYPE http_idempotency_header_pair AS (
    name text,
    value bytea
);

ALTER TABLE http_idempotency_records
    DROP COLUMN format,
    DROP COLUMN headers,
    ADD COLUMN headers http_idempotency_header_pair[] NOT NULL,
    ADD COLUMN issuer text NOT NULL,
    ADD COLUMN caller_kind text NOT NULL CHECK (caller_kind IN ('subject', 'client')),
    ADD COLUMN caller_value text NOT NULL,
    ADD CONSTRAINT http_idempotency_records_body_length
        CHECK (octet_length(body) <= 1048576);

CREATE INDEX http_idempotency_records_caller_identity_idx
    ON http_idempotency_records (issuer, caller_kind, caller_value);
SQL
	write_idempotency_target() { write_fixture "$1" <<'SQL'; }
CREATE TYPE http_idempotency_header_pair AS (
    name text,
    value bytea
);

CREATE TABLE http_idempotency_records (
    scope_key   bytea                          NOT NULL PRIMARY KEY CHECK (octet_length(scope_key) = 32),
    fingerprint bytea                          NOT NULL CHECK (octet_length(fingerprint) = 32),
    status      smallint                       NOT NULL CHECK (status BETWEEN 200 AND 299),
    headers     http_idempotency_header_pair[] NOT NULL,
    body        bytea                          NOT NULL CHECK (octet_length(body) <= 1048576),
    issuer      text                           NOT NULL,
    caller_kind text                           NOT NULL CHECK (caller_kind IN ('subject', 'client')),
    caller_value text                          NOT NULL,
    expires_at  timestamptz                    NOT NULL
);
CREATE INDEX http_idempotency_records_expires_at ON http_idempotency_records (expires_at);
SQL

	endpoint_run() { env -u BASE_REF -u MIGRATION_DIR "MIGRATION_REPO_ROOT=$1" bash "${script}"; }
	for endpoint in "${endpoint_fixture}" "${transition_fixture}"; do
		git -C "${endpoint}" init -q
		git -C "${endpoint}" config user.name migration-history
		git -C "${endpoint}" config user.email migration-history@example.invalid
	done

	mkdir -p "${endpoint_fixture}/migrations"
	write_jobs_consolidated "${endpoint_fixture}/${jobs_create_path}"
	assert_fixture_hash "${endpoint_fixture}/${jobs_create_path}" 600b30e77bcc5e6bdb0607b4f3d58fd2779cd874
	printf 'baseline documentation\n' >"${endpoint_fixture}/migrations/README.md"
	git -C "${endpoint_fixture}" add .
	git -C "${endpoint_fixture}" commit -qm consolidated-baseline
	write_jobs_new "${endpoint_fixture}/${jobs_create_path}"
	printf 'updated documentation\n' >"${endpoint_fixture}/migrations/README.md"
	assert_fixture_hash "${endpoint_fixture}/${jobs_create_path}" 16733d177126483971f1f4760f7b0f64039fda59
	endpoint_run "${endpoint_fixture}" >/dev/null || {
		echo "self-test: consolidated jobs endpoint transition failed" >&2
		exit 1
	}
	printf ' ' >>"${endpoint_fixture}/${jobs_create_path}"
	if endpoint_run "${endpoint_fixture}" >/dev/null 2>&1; then
		echo "self-test: edited consolidated jobs target passed" >&2
		exit 1
	fi

	mkdir -p "${transition_fixture}/migrations"
	write_idempotency_old "${transition_fixture}/${idempotency_create_path}"
	write_jobs_old "${transition_fixture}/${jobs_create_path}"
	write_jobs_simplify "${transition_fixture}/${jobs_simplify_path}"
	write_idempotency_simplify "${transition_fixture}/${idempotency_simplify_path}"
	assert_fixture_hash "${transition_fixture}/${idempotency_create_path}" 67545b94cfe4f58f931b24515801f6ab6e157720
	assert_fixture_hash "${transition_fixture}/${jobs_create_path}" 167ca4d43efd05cd6e521a3818cf07a317005138
	assert_fixture_hash "${transition_fixture}/${jobs_simplify_path}" 8e947805e66ce535f3853a08704fd70ff8e66a89
	assert_fixture_hash "${transition_fixture}/${idempotency_simplify_path}" e98208f8aab9b15b6c369a16383fa4d1a549e7d1
	git -C "${transition_fixture}" add .
	git -C "${transition_fixture}" commit -qm four-file-baseline
	apply_historical_transition() {
		write_idempotency_target "${transition_fixture}/${idempotency_create_path}"
		write_jobs_consolidated "${transition_fixture}/${jobs_create_path}"
		assert_fixture_hash "${transition_fixture}/${idempotency_create_path}" ca23412adb35422a42235979356b13358723bf85
		assert_fixture_hash "${transition_fixture}/${jobs_create_path}" 600b30e77bcc5e6bdb0607b4f3d58fd2779cd874
		rm "${transition_fixture}/${jobs_simplify_path}" "${transition_fixture}/${idempotency_simplify_path}"
	}
	apply_historical_transition
	endpoint_run "${transition_fixture}" >/dev/null || {
		echo "self-test: historical four-file endpoint transition failed" >&2
		exit 1
	}
	git -C "${transition_fixture}" reset --hard -q
	apply_new_transition() {
		write_idempotency_target "${transition_fixture}/${idempotency_create_path}"
		write_jobs_new "${transition_fixture}/${jobs_create_path}"
		assert_fixture_hash "${transition_fixture}/${idempotency_create_path}" ca23412adb35422a42235979356b13358723bf85
		assert_fixture_hash "${transition_fixture}/${jobs_create_path}" 16733d177126483971f1f4760f7b0f64039fda59
		rm "${transition_fixture}/${jobs_simplify_path}" "${transition_fixture}/${idempotency_simplify_path}"
	}
	apply_new_transition
	endpoint_run "${transition_fixture}" >/dev/null || {
		echo "self-test: new four-file endpoint transition failed" >&2
		exit 1
	}
	git -C "${transition_fixture}" reset --hard -q
	write_jobs_new "${transition_fixture}/${jobs_create_path}"
	rm "${transition_fixture}/${jobs_simplify_path}" "${transition_fixture}/${idempotency_simplify_path}"
	if endpoint_run "${transition_fixture}" >/dev/null 2>&1; then
		echo "self-test: partial four-file transition passed" >&2
		exit 1
	fi
	git -C "${transition_fixture}" reset --hard -q
	apply_new_transition
	printf ' ' >>"${transition_fixture}/${jobs_create_path}"
	if endpoint_run "${transition_fixture}" >/dev/null 2>&1; then
		echo "self-test: edited four-file jobs target passed" >&2
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

# The consolidated PR #60 endpoint may be rewritten exactly once before any
# database adopts it. Only that migration entry is excluded; additions still
# take the ordinary version-order path below.
reviewed_consolidated_jobs_rewrite() {
	[[ ${migration_dir} == migrations ]] || return 1
	local non_additions
	non_additions=$(printf '%s\n' "${diff_status}" | awk -F/ "${is_migration}" | awk -F'\t' '$1 != "A" && $1 != ""')
	[[ ${non_additions} == $'M\t'"${jobs_create}" ]] || return 1
	[[ $(git rev-parse "${base}:${jobs_create}" 2>/dev/null) == 600b30e77bcc5e6bdb0607b4f3d58fd2779cd874 ]] || return 1
	[[ -f ${jobs_create} ]] || return 1
	[[ $(git hash-object "${jobs_create}") == 16733d177126483971f1f4760f7b0f64039fda59 ]] || return 1
	return 0
}

# The only pre-adoption history rewrite accepted by this gate. It preserves
# the historical PR #60 endpoint and accepts the new canonical endpoint, each
# with the same exact old identities and idempotency/deletion endpoints.
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
	local jobs_target
	jobs_target=$(git hash-object "${jobs_create}")
	[[ ${jobs_target} == 600b30e77bcc5e6bdb0607b4f3d58fd2779cd874 || ${jobs_target} == 16733d177126483971f1f4760f7b0f64039fda59 ]] || return 1
	return 0
}

if reviewed_consolidated_jobs_rewrite; then
	# Only the exact jobs rewrite bypasses append-only history; every other
	# migration change, including future additions, stays on the normal path.
	changes=$(printf '%s\n' "${diff_status}" | awk -F'\t' -v jobs="${jobs_create}" '$2 != jobs && $1 != "A" && $1 != ""' | awk -F/ "${is_migration}" || true)
elif reviewed_pre_adoption_squash; then
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
