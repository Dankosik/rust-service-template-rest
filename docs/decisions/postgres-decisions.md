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
