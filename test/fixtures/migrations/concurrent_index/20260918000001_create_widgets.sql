-- Fixture: two rows share a sku, so the unique build that follows fails.
CREATE TABLE widgets (
    id bigint PRIMARY KEY,
    sku text NOT NULL
);
INSERT INTO widgets (id, sku) VALUES (1, 'a'), (2, 'a');
