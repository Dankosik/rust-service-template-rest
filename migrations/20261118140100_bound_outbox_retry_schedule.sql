UPDATE background_jobs AS job
SET not_before = statement_timestamp() + interval '48.6 seconds' + random() * interval '10.8 seconds'
WHERE job.kind = 'publish_domain_event'
  AND job.state = 'pending'
  AND job.not_before > statement_timestamp() + interval '60 seconds'
  AND isfinite(job.not_before)
  AND job.attempts BETWEEN 1 AND 24
  AND job.claim_generation > 0
  AND job.attempted_by IS NOT NULL
  AND (
      job.error_summary IN (
          'broker_refused', 'broker_size_limit', 'publish_window_full', 'publisher_unavailable',
          'publication_ack_unknown', 'publish_context_stopped', 'attempt timed out after 30s', 'handler panicked'
      )
      OR starts_with(job.error_summary, 'payload does not decode as publish_domain_event: ')
  )
  AND (job.errors -> -1) @> jsonb_build_object('attempt', job.attempts, 'error', job.error_summary)
  AND EXISTS (
      SELECT 1
      FROM (
          SELECT CASE
              WHEN (job.errors -> -1 ->> 'at') ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}T'
                  AND pg_input_is_valid(job.errors -> -1 ->> 'at', 'timestamptz')
              THEN (job.errors -> -1 ->> 'at')::timestamptz
          END AS failed_at
      ) AS retry
      WHERE retry.failed_at BETWEEN job.created_at AND statement_timestamp()
        AND EXTRACT(EPOCH FROM job.not_before) - EXTRACT(EPOCH FROM retry.failed_at)
            BETWEEN power(job.attempts::numeric, 4) * 0.9 AND power(job.attempts::numeric, 4) * 1.1
  )
  AND outbox_verified_payload(job.id, job.unique_key, job.predecessor_job_id, job.payload_text, job.payload) IS NOT NULL;
