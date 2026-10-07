-- Apply explicitly to an owned example database after the template migrations.
-- No receipt TTL: retain identity for the complete permitted replay horizon.
CREATE TABLE recovery_producer_counters (
    counter_id text PRIMARY KEY,
    value bigint NOT NULL
);
-- This audit identity proves the business/intent transaction. It intentionally
-- retains a digest, not replay bytes: completed/deleted outbox rows cannot be
-- reconstructed from this table during a mismatched broker restore.
CREATE TABLE recovery_producer_events (
    logical_id text PRIMARY KEY,
    event_type text NOT NULL,
    schema_version integer NOT NULL,
    occurred_at text NOT NULL,
    subject text NOT NULL,
    payload_sha256 text NOT NULL
);
CREATE TABLE recovery_counters (
    counter_id text PRIMARY KEY,
    value bigint NOT NULL
);
CREATE TABLE recovery_effect_receipts (
    consumer_scope text NOT NULL,
    logical_id text NOT NULL,
    event_type text NOT NULL,
    schema_version integer NOT NULL CHECK (schema_version > 0),
    occurred_at text NOT NULL,
    counter_id text NOT NULL,
    delta bigint NOT NULL,
    PRIMARY KEY (consumer_scope, logical_id)
);

-- The existing integration_tests::jobs::Probe ordinary job records attempts here.
CREATE TABLE probe_attempts (
    seq bigserial PRIMARY KEY,
    job_id uuid NOT NULL,
    attempt integer NOT NULL,
    started_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
