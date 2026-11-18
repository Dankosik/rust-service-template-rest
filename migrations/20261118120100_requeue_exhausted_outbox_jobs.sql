WITH recoverable AS (
    SELECT DISTINCT ON (job.unique_key) job.id
    FROM background_jobs AS job
    WHERE job.kind = 'publish_domain_event'
      AND job.state = 'failed'
      AND job.failure_reason = 'exhausted'
      AND job.attempts = 25
      AND job.error_summary IN ('broker_refused', 'broker_size_limit', 'publish_window_full', 'publisher_unavailable')
      AND (job.errors -> -1) @> jsonb_build_object('attempt', job.attempts, 'error', job.error_summary)
      AND outbox_verified_payload(job.id, job.unique_key, job.predecessor_job_id, job.payload_text, job.payload) IS NOT NULL
      AND NOT EXISTS (
          SELECT 1 FROM background_jobs AS peer
          WHERE peer.kind = job.kind AND peer.unique_key = job.unique_key
            AND peer.state IN ('pending', 'running', 'completed')
      )
    ORDER BY job.unique_key, job.created_at, job.id
)
UPDATE background_jobs AS job
SET recovery_history = job.recovery_history || jsonb_build_object(
        'version', job.claim_generation::text, 'attempts', job.attempts,
        'failure_reason', job.failure_reason, 'finished_at', job.finished_at,
        'attempted_by', job.attempted_by, 'error_summary', job.error_summary,
        'errors', job.errors, 'redriven_at', statement_timestamp()),
    state = 'pending',
    not_before = statement_timestamp(),
    claim_generation = nextval('background_jobs_claim_generation'),
    attempted_by = NULL,
    finished_at = NULL,
    failure_reason = NULL
FROM recoverable
WHERE job.id = recoverable.id;
