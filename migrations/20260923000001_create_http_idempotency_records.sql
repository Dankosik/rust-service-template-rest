CREATE TABLE http_idempotency_records (
    scope_key   bytea       NOT NULL PRIMARY KEY CHECK (octet_length(scope_key) = 32),
    fingerprint bytea       NOT NULL CHECK (octet_length(fingerprint) = 32),
    format      smallint    NOT NULL CHECK (format > 0),
    status      smallint    NOT NULL CHECK (status BETWEEN 200 AND 299),
    headers     bytea       NOT NULL,
    body        bytea       NOT NULL,
    expires_at  timestamptz NOT NULL
);
CREATE INDEX http_idempotency_records_expires_at ON http_idempotency_records (expires_at);
