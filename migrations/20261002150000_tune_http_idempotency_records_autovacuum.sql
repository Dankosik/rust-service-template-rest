-- Every record is inserted once and deleted after its expiry, so dead rows
-- arrive at the rate of stored successes. The server default starts a vacuum
-- at 20% of the table, which for a long retention leaves a fifth of the
-- records and their TOAST bodies as dead space. 5,000 rows plus 1% bounds it.
-- The TOAST table follows the same two values because no toast.* value is
-- set. The change takes SHARE UPDATE EXCLUSIVE: reads and writes continue.
ALTER TABLE http_idempotency_records SET (
    autovacuum_vacuum_scale_factor = 0.01,
    autovacuum_vacuum_threshold = 5000
);
