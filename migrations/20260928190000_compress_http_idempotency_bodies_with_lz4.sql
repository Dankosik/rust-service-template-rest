-- lz4 compresses and decompresses stored success bodies several times faster
-- than the default pglz. Only rows written after this use it; older rows keep
-- pglz until they expire. A server built without lz4 refuses this migration.
ALTER TABLE http_idempotency_records ALTER COLUMN body SET COMPRESSION lz4;
