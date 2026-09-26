-- Coordinated cutover: old receivers must remain stopped after this commits.
LOCK TABLE webhook_receipts IN ACCESS EXCLUSIVE MODE;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM webhook_receipts
        GROUP BY endpoint_id COLLATE "C", message_id
        HAVING count(*) > 1
    ) THEN
        RAISE EXCEPTION 'webhook receipt migration refused duplicate exact identities';
    END IF;
END
$$;

ALTER TABLE webhook_receipts
    ALTER COLUMN endpoint_id TYPE text COLLATE "C",
    ADD COLUMN received_at timestamptz NOT NULL DEFAULT now();

-- Actual index admission proves historical widths before removing old ownership.
CREATE UNIQUE INDEX webhook_receipts_exact_identity
    ON webhook_receipts (endpoint_id, message_id);
ALTER TABLE webhook_receipts DROP CONSTRAINT webhook_receipts_pkey;
ALTER TABLE webhook_receipts
    ADD CONSTRAINT webhook_receipts_pkey PRIMARY KEY USING INDEX webhook_receipts_exact_identity,
    DROP COLUMN identity_hash,
    DROP COLUMN body_sha256;
CREATE INDEX webhook_receipts_received_at ON webhook_receipts (received_at);
