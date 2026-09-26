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
