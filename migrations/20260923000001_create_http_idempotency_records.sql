CREATE TYPE http_idempotency_header_pair AS (
    name text,
    value bytea
);

CREATE TABLE http_idempotency_records (
    scope_key   bytea                          NOT NULL PRIMARY KEY CHECK (octet_length(scope_key) = 32),
    fingerprint bytea                          NOT NULL CHECK (octet_length(fingerprint) = 32),
    status      smallint                       NOT NULL CHECK (status BETWEEN 200 AND 299),
    headers     http_idempotency_header_pair[] NOT NULL,
    body        bytea                          NOT NULL CHECK (octet_length(body) <= 1048576),
    issuer      text                           NOT NULL,
    caller_kind text                           NOT NULL CHECK (caller_kind IN ('subject', 'client')),
    caller_value text                          NOT NULL,
    expires_at  timestamptz                    NOT NULL
);
CREATE INDEX http_idempotency_records_expires_at ON http_idempotency_records (expires_at);
