CREATE TABLE widgets (
    id bigint PRIMARY KEY,
    name text NOT NULL,
    edited boolean NOT NULL DEFAULT false
);
