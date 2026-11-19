# PostgreSQL decisions

## Restarting an older release after a migration

During a rolling deployment or an application rollback, an older binary can
restart after the newer release has applied its migrations. Rejecting every
unknown applied version, as SQLx does by default, would keep that replica
offline even after an additive schema change. The template therefore admits
later versions while checking the history its binary knows. The
[migration integration tests](../../test/tests/postgres.rs) cover this in
`an_older_release_admits_the_history_of_a_later_one`.

Each release must keep the previous release's queries compatible throughout
their overlap. Reopen this choice if the deployment contract requires an
exclusive schema version and guarantees older binaries cannot restart.

## Retaining the PostgreSQL write buffer between checkouts

Shrinking an idle connection's write buffer to the length of its last COMMIT
frame makes the next prepared statement grow it again. Retaining the initial
8 KiB capacity removed three allocator reallocation calls per warm
single-statement transaction in each of five baseline/candidate pairs, for
both SELECT and UPDATE. The isolated comparison used four pooled connections,
PostgreSQL 18.6 over loopback, and a dedicated DigitalOcean c-4 with jemalloc.

After four concurrent 2 MiB round trips and 60 seconds idle, median anonymous
client RSS was 4,348 KiB on the baseline and 4,436 KiB with the floor. The
observed ranges overlapped. Reopen the floor if a derived service with a
larger idle pool measures retained memory that outweighs the saved reallocations.
