-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS background_jobs_outbox_unconfirmed_created
    ON background_jobs (created_at)
    WHERE kind = 'publish_domain_event' AND state <> 'completed';
