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
Only `infra-idempotency-store` names the table; the service never creates or
alters it at runtime.
<!-- template:end http-idempotency:migrations-readme-http-idempotency -->
<!-- template:begin jobs:migrations-readme-jobs -->
The background jobs pack creates `background_jobs` and its claim-generation
sequence in `20260924000001_create_background_jobs.sql`. That canonical schema
uses JSONB payloads, C-collated text unique keys, trace state, and the current
partial indexes. The existing `migrate` binary applies it with the rest of the
set. Neither the service nor worker creates or alters schema at runtime, and
only `crates/infra-jobs` names the table.
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
  and refuses a history that disagrees with the source. `make migration-check`
  also refuses a pull request that touches one, except for the one reviewed
  pre-adoption rewrite that replaces the two idempotency/jobs create blobs and
  deletes their two superseded simplify blobs as one exact four-file change.
  That source-only exception does not make runtime history compatible with the
  former migrations; a database made from the former history must be explicitly
  recreated outside startup.

Files that are not `<version>_<name>.sql` (this README) are ignored by the
resolver.
