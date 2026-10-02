-- no-transaction
-- Fixture: outlives a short statement budget, inside the run's deadline.
SELECT pg_sleep(1.5);
