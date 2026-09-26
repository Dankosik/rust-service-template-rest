LOCK TABLE http_idempotency_records IN ACCESS EXCLUSIVE MODE;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM http_idempotency_records
        WHERE expires_at > clock_timestamp()
    ) THEN
        RAISE EXCEPTION 'http idempotency simplification refuses live records';
    END IF;
END
$$;

DELETE FROM http_idempotency_records
WHERE expires_at <= clock_timestamp();

CREATE TYPE http_idempotency_header_pair AS (
    name text,
    value bytea
);

ALTER TABLE http_idempotency_records
    DROP COLUMN format,
    DROP COLUMN headers,
    ADD COLUMN headers http_idempotency_header_pair[] NOT NULL,
    ADD COLUMN issuer text NOT NULL,
    ADD COLUMN caller_kind text NOT NULL CHECK (caller_kind IN ('subject', 'client')),
    ADD COLUMN caller_value text NOT NULL,
    ADD CONSTRAINT http_idempotency_records_body_length
        CHECK (octet_length(body) <= 1048576);

CREATE INDEX http_idempotency_records_caller_identity_idx
    ON http_idempotency_records (issuer, caller_kind, caller_value);
