ALTER TABLE background_jobs ADD COLUMN predecessor_job_id uuid;

CREATE TABLE outbox_event_receipts (
    event_key text COLLATE "C" PRIMARY KEY,
    job_id uuid NOT NULL UNIQUE,
    intent_sha256 bytea NOT NULL CHECK (octet_length(intent_sha256) = 32),
    aggregate_key text COLLATE "C" CHECK (octet_length(aggregate_key) BETWEEN 1 AND 255),
    aggregate_version bigint CHECK (aggregate_version > 0),
    CHECK ((aggregate_key IS NULL) = (aggregate_version IS NULL))
);

CREATE UNIQUE INDEX outbox_event_receipts_aggregate_version
    ON outbox_event_receipts (aggregate_key, aggregate_version)
    WHERE aggregate_key IS NOT NULL;

CREATE TABLE outbox_legacy_job_aliases (
    job_id uuid PRIMARY KEY REFERENCES background_jobs (id) ON DELETE CASCADE,
    receipt_job_id uuid NOT NULL REFERENCES outbox_event_receipts (job_id)
);

CREATE FUNCTION outbox_intent_sha256(intent jsonb) RETURNS bytea
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
RETURN sha256(convert_to(intent::text, 'UTF8'));

CREATE FUNCTION outbox_event_key(intent jsonb) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
RETURN CASE WHEN jsonb_typeof(intent -> 'message_id') = 'string'
                 AND (intent ->> 'message_id') <> ''
            THEN 'event-' || encode(sha256(convert_to(intent ->> 'message_id', 'UTF8')), 'hex')
       END;

CREATE FUNCTION outbox_verified_payload(
    candidate_id uuid, candidate_key text, predecessor_id uuid, serialized text, legacy jsonb
) RETURNS text LANGUAGE sql STABLE
RETURN CASE WHEN EXISTS (
    SELECT 1 FROM outbox_event_receipts AS receipt
    WHERE receipt.event_key = candidate_key COLLATE "C"
      AND (receipt.job_id = candidate_id OR EXISTS (
          SELECT 1 FROM outbox_legacy_job_aliases AS alias
          WHERE alias.job_id = candidate_id AND alias.receipt_job_id = receipt.job_id
      ))
      AND CASE WHEN receipt.aggregate_version > 1 THEN EXISTS (
          SELECT 1 FROM outbox_event_receipts AS prior
          WHERE prior.aggregate_key = receipt.aggregate_key
            AND prior.aggregate_version = receipt.aggregate_version - 1
            AND prior.job_id = predecessor_id
      ) ELSE predecessor_id IS NULL END
      AND receipt.intent_sha256 = outbox_intent_sha256(CASE
          WHEN serialized IS NULL THEN legacy
          ELSE (CASE WHEN pg_input_is_valid(serialized, 'jsonb') THEN serialized END)::jsonb
      END)
) THEN COALESCE(serialized, legacy::text) END;
