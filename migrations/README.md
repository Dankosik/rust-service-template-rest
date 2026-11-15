# Migrations

The PostgreSQL `migrate` binary takes its forward-only migration inventory from
the `sqlx::migrate::Migrator` embedded by `crates/migrate` at compile time,
acquiring a session lock on one connection and checking applied history before
applying pending SQL files (`.sql` files in `migrations/` are the sole source of
truth for columns, types, indexes, and table storage parameters).

<!-- template:begin http-idempotency:migrations-readme-http-idempotency -->
`http_idempotency_records` intentionally omits a caller-identity index:
unrestricted verified caller values cannot be safely represented by a literal
btree key, and no runtime query needs that access path (operators use bound
queries over heap metadata). Table-local compression and autovacuum tuning alter
no server setting or role, and only `infra-idempotency-store` names the table.
<!-- template:end http-idempotency:migrations-readme-http-idempotency -->
<!-- template:begin jobs:migrations-readme-jobs -->
`background_jobs` uses table-local autovacuum tuning without changing server
settings or roles, and only `crates/infra-jobs` names the table. Migration
identifiers `20261002150001` and `20261002150002` follow the `20261002150000`
high-water mark allocated ahead of UTC.

Old workers remain schema-compatible with `errors` and `recovery_history` but
still delete failed rows on their legacy horizon: apply the migrations and
stop/replace every old retention owner before activating recovery or relying on
retained-failure custody. On rollback, keep the additive schema, roll forward
with migrations, and never clear `recovery_history` or reset claim-generation
sequences. Apply `20261112120000_preserve_background_job_payload_text.sql`
before replacing producers and workers so dual-written nullable text and JSONB
columns preserve rolling compatibility.
<!-- template:end jobs:migrations-readme-jobs -->
<!-- template:begin inbound-webhooks:migrations-readme-inbound-webhooks -->
Stop pre-cutover receivers before running
`20260926130000_native_webhook_receipts.sql` (which locks `webhook_receipts` in
`ACCESS EXCLUSIVE MODE` and refuses duplicate `(endpoint_id COLLATE "C",
message_id)` rows) and keep old receivers stopped during and after cutover.
<!-- template:end inbound-webhooks:migrations-readme-inbound-webhooks -->

Authoring rules enforced by `cargo test -p migrate` and `make migration-check`:

- Name files `<version>_<lowercase_snake_case>.sql` using UTC `YYYYMMDDHHMMSS`
  versions so concurrent branches do not collide; never edit or delete an
  applied file, and never add `.up.sql`/`.down.sql` pairs (roll forward with a
  new migration).
- Each file runs in one transaction unless PostgreSQL forbids it (such as
  `CREATE INDEX CONCURRENTLY` on an existing table): start non-transactional
  files with `-- no-transaction`, keep one idempotent statement (`IF NOT EXISTS`)
  bounded by `postgres.migration_deadline`, and drop any failed invalid index
  with `DROP INDEX CONCURRENTLY <name>` before rerunning.
- Never set session `lock_timeout` or `statement_timeout` inside a migration
  file (the runner owns session timeouts). `make migration-check` lints added
  files with [Squawk](https://squawk.dev); justify intentional waivers inline
  above the statement with a comment and `-- squawk-ignore <rule>`.
- When a migration affects `sqlx::query!` statements, run `make sqlx-prepare`
  and commit `.sqlx/` updates (`make sqlx-check` enforces parity in CI).
