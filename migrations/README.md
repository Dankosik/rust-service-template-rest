# Migrations

The PostgreSQL `migrate` binary takes its forward-only migration inventory
from the `sqlx::migrate::Migrator` embedded by `crates/migrate` at compile
time. On one PostgreSQL connection, the runner takes a session lock, checks
applied history, and uses SQLx's `Migrate` operations to apply pending
migrations. The runner reports progress for each migration. A service's own
migrations arrive with its first durable feature. With an empty set, the
runner checks the empty-history path.

<!-- template:begin http-idempotency:migrations-readme-http-idempotency -->
`http_idempotency_records` intentionally omits a caller-identity index:
unrestricted verified caller values cannot be safely represented by a literal
btree key, and no runtime query needs that access path (operators use bound
queries over heap metadata). Table-local `lz4` TOAST compression and autovacuum
tuning (`5,000` rows, `1%` scale factor) alter no server setting or role, and
only `infra-idempotency-store` names the table.
<!-- template:end http-idempotency:migrations-readme-http-idempotency -->
<!-- template:begin jobs:migrations-readme-jobs -->
`background_jobs` uses table-local autovacuum tuning (`5,000` rows, `0` scale
factor) without changing server settings or roles, and only `crates/infra-jobs`
names the table. Migration identifiers `20261002150001` and `20261002150002`
follow the `20261002150000` high-water mark allocated ahead of UTC.

Old workers remain schema-compatible with `errors` and `recovery_history` but
still delete seven-day failures: apply the migrations and stop/replace every old
retention owner before activating recovery or relying on retained-failure
custody. New workers require complete admitted history; on rollback, keep the
additive schema, roll forward with migrations, and never clear `recovery_history`
or reset claim-generation sequences.

Apply `20261112120000_preserve_background_job_payload_text.sql` before replacing
producers and workers. New producers populate both nullable text and JSONB
columns so old workers remain compatible during rollout or rollback; existing
JSONB-only rows retain their normalized representation.
<!-- template:end jobs:migrations-readme-jobs -->
<!-- template:begin inbound-webhooks:migrations-readme-inbound-webhooks -->
Stop pre-cutover receivers before running
`20260926130000_native_webhook_receipts.sql` (which locks `webhook_receipts` in
`ACCESS EXCLUSIVE MODE` and refuses duplicate `(endpoint_id COLLATE "C",
message_id)` rows) and keep old receivers stopped during and after cutover.
<!-- template:end inbound-webhooks:migrations-readme-inbound-webhooks -->

Rules enforced by `cargo test -p migrate` over the embedded set and by
`make migration-check` over the change:

- File name `<version>_<lowercase_snake_case>.sql`, where `<version>` is a
  positive integer. Use the UTC timestamp `YYYYMMDDHHMMSS` so concurrent
  branches do not collide, for example `20260918120000_create_widgets.sql`.
  A version identifies one file: two added files with the same version are
  refused by `make migration-check` and by the runner.
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

  The runner bounds it by the remaining `postgres.migration_deadline`
  (default `5m`) instead of the two-minute statement budget and the
  15-second lock budget; raise that key on the run that builds an index on
  a large table. A build that fails leaves an invalid index, and the next
  run refuses to start the migration until it is dropped (`DROP INDEX
  CONCURRENTLY <name>`), naming it in the error.
- No `.up.sql`/`.down.sql` pairs. A rollback is a new forward migration.
- An applied file is never edited or deleted: the runner compares checksums
  and refuses a history that disagrees with the source. `make migration-check`
  also refuses a pull request that touches one, with no exception.
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
