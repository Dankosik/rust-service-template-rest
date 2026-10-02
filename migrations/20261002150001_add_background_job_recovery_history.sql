-- Failed cycles remain attached to the same job through explicit recovery.
ALTER TABLE background_jobs
    ADD COLUMN recovery_history jsonb NOT NULL DEFAULT '[]';
