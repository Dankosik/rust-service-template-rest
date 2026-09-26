# Migrations

Forward-only SQL migrations for the PostgreSQL profile, embedded into the
`migrate` binary by `crates/migrate` at compile time and applied by
`sqlx::migrate::Migrator` under a session lock. A service's own migrations
arrive with its first durable feature; with an empty set, the runner proves
the empty-history path. Rationale: [Persistence Architecture](../docs/architecture/persistence.md).

<!-- template:begin http-idempotency:migrations-readme-http-idempotency -->
The HTTP idempotency pack first creates `http_idempotency_records` in
`20260923000001_create_http_idempotency_records.sql`. Its guarded replacement,
`20260926001448_simplify_http_idempotency_records.sql`, takes an ACCESS
EXCLUSIVE table lock before refusing any row whose `expires_at` is still live.
It deletes only expired rows, replaces the legacy binary-header format with the
`http_idempotency_header_pair[]` composite array, and adds verified caller
metadata. This is maintenance-only forward recovery: quiesce idempotent
traffic, drain old replicas, retain live legacy rows through expiry, run the
migration, start new replicas, then reopen traffic. Never run old and new
implementations together. The service never creates or alters schema at
runtime, and only `infra-idempotency-store` names the table.

There is no down migration after the guard has admitted the new schema. If the
new application must be rolled back before cutover completes, stop it and
restore a compatible pre-migration deployment only while the forward migration
has not been applied; after application, roll forward with a new reviewed
migration or restore from an operator-managed backup. Applied migration files
remain byte-for-byte history.
<!-- template:end http-idempotency:migrations-readme-http-idempotency -->
<!-- template:begin jobs:migrations-readme-jobs -->
The background jobs pack ships two forward-only migrations:
`20260924000001_create_background_jobs.sql` creates the `background_jobs`
table and its claim-generation sequence, and
`20260925000001_simplify_background_jobs.sql` converts payload to JSONB,
unique keys to C-collated text, adds trace-state, and replaces the running
index. The existing `migrate` binary applies both with the rest of the set.
The conversion requires all old producers and workers to be stopped; neither
the service nor worker creates or alters schema at runtime, and only
`crates/infra-jobs` names the table.
<!-- template:end jobs:migrations-readme-jobs -->

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
