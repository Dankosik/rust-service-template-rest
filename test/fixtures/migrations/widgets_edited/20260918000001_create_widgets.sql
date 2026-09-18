-- Fixture: the first widgets migration after an edit; its checksum differs.
CREATE TABLE widgets (
    id bigint PRIMARY KEY,
    name text NOT NULL,
    edited boolean NOT NULL DEFAULT false
);
