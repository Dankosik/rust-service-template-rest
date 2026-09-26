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
