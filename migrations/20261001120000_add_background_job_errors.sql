-- Failure history. Each failed attempt and each lease-expiry rescue appends
-- one `{"attempt", "at", "error"}` entry, so a job has at most one entry per
-- spent attempt; `error_summary` stays the most recent one. Enqueue is the
-- only insert path and supplies a time-ordered id, so the random default goes.
ALTER TABLE background_jobs
    ALTER COLUMN id DROP DEFAULT,
    ADD COLUMN errors jsonb NOT NULL DEFAULT '[]';
