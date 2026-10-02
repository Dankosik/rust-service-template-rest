# Migrations

Forward-only SQL migrations for the PostgreSQL profile, embedded into the
`migrate` binary by `crates/migrate` at compile time and applied by
`sqlx::migrate::Migrator` under a session lock. A service's own migrations
arrive with its first durable feature; with an empty set, the runner proves
the empty-history path. Rationale: [Persistence Architecture](../docs/architecture/persistence.md).

<!-- template:begin http-idempotency:migrations-readme-http-idempotency -->
The HTTP idempotency pack creates `http_idempotency_records` in
`20260923000001_create_http_idempotency_records.sql`. The canonical schema
stores digest-backed identity, the request fingerprint, accepted 2xx response
bytes and headers, expiry, and trusted caller metadata. It intentionally omits
a caller-identity index: unrestricted verified caller values cannot be safely
represented by a literal btree key, and no runtime query needs that access
path. Trusted operators can still use the heap metadata with bound queries.
`20260928190000_compress_http_idempotency_bodies_with_lz4.sql` switches new
bodies to lz4 TOAST compression, which PostgreSQL 14+ builds with lz4 support
provide. `20261002150000_tune_http_idempotency_records_autovacuum.sql` sets a
table-local autovacuum threshold of 5,000 rows and scale factor of 1%, which
the TOAST table follows; it does not change any server setting or role. Only `infra-idempotency-store` names the table; the service never
creates or alters it at runtime.
<!-- template:end http-idempotency:migrations-readme-http-idempotency -->
<!-- template:begin jobs:migrations-readme-jobs -->
The background jobs pack creates `background_jobs` and its claim-generation
sequence in `20260924000001_create_background_jobs.sql`. That canonical schema
uses JSONB payloads, C-collated text unique keys, trace state, enqueue-time
`created_at`, nullable UUID `attempted_by`, and the current partial indexes.
It sets a table-local autovacuum scale factor of zero and threshold of 5,000;
it does not change any server setting or role. The existing `migrate` binary
applies it with the rest of the set.
`20261001120000_add_background_job_errors.sql` adds the `errors` JSONB
failure history and drops the unused random `id` default, since enqueue
supplies a time-ordered id. Neither the service nor worker creates or
alters schema at runtime, and only `crates/infra-jobs` names the table.
`20261002150001_add_background_job_recovery_history.sql` adds
`recovery_history jsonb NOT NULL DEFAULT '[]'` for prior recovery cycles.
`20261002150002_index_failed_background_jobs.sql` then adds
`background_jobs_failed_kind (kind,id) WHERE state='failed'` concurrently,
using the canonical single-statement `-- no-transaction` path and runner
budgets. These identifiers follow the immutable `20261002150000` high-water
mark, which was ahead of current UTC when allocated; no applied file changed.

Old workers remain schema-compatible but still delete seven-day failures.
Apply both migrations and stop/replace every old retention owner before
activating recovery or relying on retained-failure custody. New workers require
their complete admitted history. Keep the additive schema on rollback and roll
forward: an old binary restores failed deletion. Never clear recovery history
or reset claim-generation sequences as a rollback step. See
[upgrade and custody](../docs/background-jobs.md#upgrade-and-custody).
<!-- template:end jobs:migrations-readme-jobs -->

Rules, proven by `cargo test -p migrate` over the embedded set:

- File name `<version>_<lowercase_snake_case>.sql`, where `<version>` is a
  positive integer. Use the UTC timestamp `YYYYMMDDHHMMSS` so concurrent
  branches do not collide, for example `20260918120000_create_widgets.sql`.
- One transaction per file, with its history row. The exception is a
  statement PostgreSQL refuses inside a transaction, in practice
  `CREATE INDEX CONCURRENTLY` on a table that already holds rows. Such a
  file starts with the line `-- no-transaction`, holds that one statement,
  and spells it so that a rerun is safe (`IF NOT EXISTS`), because its
  history row is written after it:

  ```sql
  -- no-transaction
  CREATE INDEX CONCURRENTLY IF NOT EXISTS widgets_sku ON widgets (sku);
  ```

  The runner bounds it by `postgres.migration_deadline` (default `5m`)
  instead of the two-minute statement budget and the 15-second lock
  budget; raise that key on the run that builds an index on a large table. A build that fails leaves an
  invalid index, and the next run refuses to start the migration until it
  is dropped (`DROP INDEX CONCURRENTLY <name>`), naming it in the error.
- No `.up.sql`/`.down.sql` pairs. A rollback is a new forward migration.
- An applied file is never edited or deleted: the runner compares checksums
  and refuses a history that disagrees with the source. `make migration-check`
  also refuses a pull request that touches one, except for the one reviewed
  pre-adoption rewrite that replaces the two idempotency/jobs create blobs and
  deletes their two superseded simplify blobs as one exact four-file change.
  That source-only exception does not make runtime history compatible with the
  former migrations; a database made from the former history must be explicitly
  recreated outside startup.
- `make migration-check` lints every file a change adds with
  [Squawk](https://squawk.dev) for DDL that blocks or breaks a running
  service: an index built without `CONCURRENTLY` on an existing table, a
  required column without a default, a column type change, a dropped
  column. When the operation is intended, say why and waive the rule above
  the statement:

  ```sql
  -- The previous release stopped reading this column.
  -- squawk-ignore ban-drop-column
  ALTER TABLE widgets DROP COLUMN sku;
  ```

  A file sets no `lock_timeout` or `statement_timeout`; the runner
  publishes both for its session.

A migration changes what the checked statements (`sqlx::query!`) compile
against: run `make sqlx-prepare` with it and commit the `.sqlx/` changes.
`make sqlx-check` refuses stale metadata in CI.

Files that are not `<version>_<name>.sql` (this README) are ignored by the
resolver.
