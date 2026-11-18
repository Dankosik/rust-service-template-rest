DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM (
            SELECT unique_key, COALESCE(payload_text::jsonb, payload) AS intent
            FROM background_jobs WHERE kind = 'publish_domain_event'
        ) AS retained
        WHERE unique_key IS NULL OR unique_key IS DISTINCT FROM outbox_event_key(intent)
    ) THEN
        RAISE EXCEPTION 'retained outbox job identity cannot be reconciled';
    END IF;
END
$$;

INSERT INTO outbox_event_receipts (event_key, job_id, intent_sha256)
SELECT DISTINCT ON (unique_key, fingerprint) unique_key, id, fingerprint
FROM (
    SELECT unique_key, id, created_at,
           outbox_intent_sha256(COALESCE(payload_text::jsonb, payload)) AS fingerprint
    FROM background_jobs WHERE kind = 'publish_domain_event'
) AS retained
ORDER BY unique_key, fingerprint, created_at, id;

INSERT INTO outbox_legacy_job_aliases (job_id, receipt_job_id)
SELECT retained.id, receipt.job_id
FROM background_jobs AS retained
JOIN outbox_event_receipts AS receipt ON receipt.event_key = retained.unique_key
WHERE retained.kind = 'publish_domain_event' AND retained.id <> receipt.job_id;
