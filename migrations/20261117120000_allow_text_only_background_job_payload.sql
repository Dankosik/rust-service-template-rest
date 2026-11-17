ALTER TABLE background_jobs
    -- Readers must support payload_text before deploying a text-only producer.
    -- squawk-ignore ban-drop-not-null
    ALTER COLUMN payload DROP NOT NULL,
    ADD CONSTRAINT background_jobs_has_payload
        CHECK (payload_text IS NOT NULL OR payload IS NOT NULL) NOT VALID;
