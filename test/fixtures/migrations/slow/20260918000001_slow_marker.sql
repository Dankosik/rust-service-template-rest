-- Fixture: outlives a short orchestration deadline.
SELECT pg_sleep(5);
CREATE TABLE slow_marker (id int);
