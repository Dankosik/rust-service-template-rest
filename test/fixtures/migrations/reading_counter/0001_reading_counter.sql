-- Fixture-owned schema. These durable markers deliberately have no retention TTL.
CREATE TABLE IF NOT EXISTS reading_requests (
    scope uuid NOT NULL,
    operation_id uuid NOT NULL,
    operation text NOT NULL,
    event_id text NOT NULL,
    event_time text NOT NULL,
    event_subject text NOT NULL,
    local_job_id uuid,
    outbox_job_id uuid,
    webhook_job_id uuid,
    PRIMARY KEY (scope, operation_id)
);

CREATE TABLE IF NOT EXISTS reading_articles (
    scope uuid NOT NULL,
    channel text NOT NULL CHECK (channel IN ('local', 'outbox', 'webhook')),
    article_id uuid NOT NULL,
    read_count bigint NOT NULL CHECK (read_count >= 0),
    PRIMARY KEY (scope, channel, article_id)
);

CREATE TABLE IF NOT EXISTS reading_effects (
    scope uuid NOT NULL,
    channel text NOT NULL CHECK (channel IN ('local', 'outbox', 'webhook')),
    operation_id uuid NOT NULL,
    operation text NOT NULL,
    read_count bigint NOT NULL CHECK (read_count >= 0),
    PRIMARY KEY (scope, channel, operation_id)
);
