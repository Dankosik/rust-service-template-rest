-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS background_jobs_failed_kind
    ON background_jobs (kind, id) WHERE state = 'failed';
