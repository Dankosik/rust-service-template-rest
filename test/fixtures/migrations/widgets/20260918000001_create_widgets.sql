-- Fixture for the migration runner tests; not part of the service schema.
CREATE TABLE widgets (
    id bigint PRIMARY KEY,
    name text NOT NULL
);
