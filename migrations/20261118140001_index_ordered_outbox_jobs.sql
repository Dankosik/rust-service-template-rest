-- no-transaction
CREATE INDEX CONCURRENTLY IF NOT EXISTS background_jobs_outbox_ordered_created
    ON background_jobs (created_at) INCLUDE (predecessor_job_id)
    WHERE kind = 'publish_domain_event' AND state = 'pending' AND predecessor_job_id IS NOT NULL;
