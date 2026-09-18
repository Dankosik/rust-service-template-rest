# Migrations

Forward-only SQL migrations for the PostgreSQL profile, embedded into the
`migrate` binary by `crates/migrate` at compile time and applied by
`sqlx::migrate::Migrator` under a session lock. The first migration arrives
with the first durable feature; until then the runner proves the empty
history path. Rationale: [Persistence Architecture](../docs/architecture/persistence.md).

Rules, proven by `cargo test -p migrate` over the embedded set:

- File name `<version>_<lowercase_snake_case>.sql`, where `<version>` is a
  positive integer. Use the UTC timestamp `YYYYMMDDHHMMSS` so concurrent
  branches do not collide, for example `20260918120000_create_widgets.sql`.
- One transaction per file. `-- no-transaction` is refused; an operation
  that cannot run in a transaction (`CREATE INDEX CONCURRENTLY`) needs its
  own decision recorded in the persistence document first.
- No `.up.sql`/`.down.sql` pairs. A rollback is a new forward migration.
- An applied file is never edited or deleted: the runner compares checksums
  and refuses a history that disagrees with the source
  (`make migration-check` also refuses a pull request that touches one).

Files that are not `<version>_<name>.sql` (this README) are ignored by the
resolver.
