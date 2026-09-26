CREATE TABLE webhook_receipts (
    identity_hash bytea PRIMARY KEY CHECK (octet_length(identity_hash) = 32),
    endpoint_id text NOT NULL,
    message_id bytea NOT NULL,
    body_sha256 bytea NOT NULL CHECK (octet_length(body_sha256) = 32)
);
