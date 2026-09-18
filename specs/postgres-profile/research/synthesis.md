# Stage 8 research synthesis: PostgreSQL profile

Decisions for roadmap stage 8 (`crates/infra-postgres`, `crates/migrate`,
`migrations/`, database-backed proof). Versions were read from crates.io and
GitHub on 2026-09-18. Claims marked *verified* were executed in a scratch
project under `/tmp` against `postgres:18.4-alpine` (Rust 1.98.1, Docker
29.4.0, macOS arm64); claims marked *observed* were read in the crate source
or documentation without being executed.

The Go template supplies the problems and the reasons behind its choices
(`internal/infra/postgres`, `internal/infra/postgresmigrate`, `cmd/migrate`,
`env/docker-compose.yml`, `scripts/ci/migration-validate.sh`): one explicit
DSN as the only connection source, template-owned timeouts published as
session defaults, a readiness probe that shares the pool, a transaction seam
whose commit outcome is never misreported, forward migrations under a session
lock with an orchestration deadline and an append-only history, and proof on a
real PostgreSQL behind a heavy-run guard. Where Rust solves a problem
differently, the Rust way wins and the deviation is recorded at the end.

## Requirements

- The profile is inert unless `postgres.enabled = true`; a default
  configuration starts the service without touching PostgreSQL.
- The DSN is a secret-like key (`postgres.dsn`, environment only), a
  `postgres://` or `postgresql://` URL with an explicit host, port, user,
  password, database, and `sslmode` in `disable`, `require`, `verify-ca`,
  `verify-full`. No libpq environment, no passfile, no service file, no TLS
  key or certificate file, no socket path, no fallback hosts, no unknown
  parameters. Diagnostics never carry the DSN value.
- Every pooled connection carries `statement_timeout` and
  `idle_in_transaction_session_timeout` as session defaults; acquiring a
  connection is bounded; the pool size is the one operator-owned capacity
  value.
- A readiness probe shares the pool, respects the health budget, and reports
  the dependency name only.
- The transaction helper commits on `Ok`, rolls back on `Err`, distinguishes a
  commit that definitely failed from a commit whose durable outcome is
  unknown, and exposes a retryability predicate without a retry loop.
- Migrations: forward-only SQL files with a canonical name, each in its own
  transaction, applied under a session lock with a lock timeout, a statement
  timeout, and an orchestration deadline; an applied migration that was edited
  or removed fails the run; the terminal record names before, target, after,
  applied count, duration, outcome, and failure stage.
- Unit tests need no Docker. Database-backed proof runs behind
  `ALLOW_HEAVY=1` locally and on the `db_integration` surface in CI; the
  runtime image is rehearsed with migrations and a live database.

## Driver, pool, and migrations

| Candidate | Latest (date) | What it covers | Verdict |
| --- | --- | --- | --- |
| `sqlx` | 0.9.0 (2026-05-21) | Async driver, pool, transactions, embedded migrations with checksums and an advisory session lock, `#[sqlx::test]` per-test databases, compile-time checked queries with offline `.sqlx` metadata, rustls or native TLS | **selected**. One crate covers pool, migrations, and test isolation; `SqlSafeStr` (*verified*: `sqlx::query(&format!(..))` is a compile error unless wrapped in `AssertSqlSafe`) makes dynamic SQL a reviewed decision |
| `tokio-postgres` + `deadpool-postgres` (+ `tokio-postgres-rustls`, `refinery`) | 0.7.18 (2026-06-12), 0.14.2 (2026-08-26), 0.9.2 (2026-06-10) | Driver, pool, TLS, and migrations from four crates | rejected: assembles what `sqlx` ships in one; `refinery` runs on `tokio-postgres`, not `sqlx`, and takes no database lock (*observed* in its README) |
| `diesel` + `diesel-async` | 0.9.2 (2026-06-19) | Query DSL over a generated `schema.rs`, `diesel_migrations` embedding | rejected: a schema DSL and code generation step the template has no query for yet; `diesel_migrations` has no session lock |
| `sea-orm` | 2.0.3 (2026-09-13) | ORM over `sqlx` | rejected: an entity layer on top of the crate already selected; a feature may add it later without changing this profile |
| `bb8-postgres` | 0.9.0 (2024-12-09) | Pool only | rejected: pool only, quiet since 2024 |

`sqlx` behavior verified against the requirements:

- `PgConnectOptions::from_str` starts from `new_without_pgpass()`, which reads
  `PGHOSTADDR`, `PGHOST`, `PGPORT`, `PGUSER`, `PGPASSWORD`, `PGDATABASE`,
  `PGSSLMODE`, `PGSSLROOTCERT`, `PGSSLCERT`, `PGSSLKEY`, `PGAPPNAME`, and
  `PGOPTIONS`, then overlays the URL and finally reads `PGPASSFILE` or
  `~/.pgpass` when the URL carries no password (*verified*: `PGAPPNAME` set in
  the environment surfaced as `application_name` on a URL without one). Every
  constructor, including `Default`, takes this path; there is no
  environment-free builder.
- URL parameters recognised: `sslmode`, `sslrootcert`, `sslcert`, `sslkey`,
  `statement-cache-capacity`, `host`, `hostaddr`, `port`, `dbname`, `user`,
  `password`, `application_name`, `options`, `options[key]`; anything else
  emits `tracing::warn!` with the key **and the value** (*observed*, `parse.rs`).
  A percent-encoded host starting with `/` selects a Unix socket (*verified*);
  a comma-separated host list fails as `InvalidPort` (*verified*). `sslmode`
  accepts `allow` and `prefer` (*verified*), which the Go rules refuse as
  fallback targets. `sslrootcert` accepts an inline PEM or a file path
  (*observed*, `CertificateInput::from`).
- `PgConnectOptions::options([("statement_timeout", "8000ms"), ...])` publishes
  session defaults through the startup packet; `SHOW` returned `8s`, `8s`,
  `1500ms` for `statement_timeout`, `idle_in_transaction_session_timeout`,
  `lock_timeout` (*verified*). `lock_timeout` applies to `pg_advisory_lock`:
  a contended lock failed with SQLSTATE `55P03` after 1.5 s (*verified*).
  `statement_timeout` cancelled `pg_sleep(20)` with `57014` after 8 s
  (*verified*).
- A deferred unique constraint that fails at `COMMIT` surfaces as
  `Error::Database` with code `23505`, kind `UniqueViolation` (*verified*), so
  commit-outcome classification can use the SQLSTATE class.
- `PgPoolOptions::acquire_timeout` returns `Error::PoolTimedOut` when the pool
  is exhausted (*verified*, 500 ms budget, 2 connections) and also when a
  refused target is retried until the budget ends. `connect_with` opens one
  connection eagerly, which is the startup ping. `test_before_acquire`
  defaults to `true` (one ping per acquire) and stays at the default.
- `Migrator::run` takes `pg_advisory_lock(hash(database_name))` on the
  migration connection, creates `_sqlx_migrations`, refuses a dirty row,
  compares checksums of applied migrations (`VersionMismatch` after editing an
  applied file, *verified*), refuses an applied version missing from the
  source (`VersionMissing`, default `ignore_missing = false`), and applies each
  pending migration with its bookkeeping row in one transaction. The lock is
  released only on success; on failure the connection is dropped and the
  session lock goes with it. `pg_advisory_lock` waits without its own
  timeout, so the session `lock_timeout` above is what bounds it.
- `Migration::no_tx` is `true` for a file starting with `-- no-transaction`
  (*verified*); the resolver rejects a filename without an integer version
  prefix (*verified*, `Source(ResolveError)`) and ignores files that are not
  `<version>_<name>.sql` (*observed*, `source.rs`).
- `sqlx::migrate!("path")` embeds the files at compile time with
  `include_str!` per file; a new file is noticed only through a build script
  that prints `cargo:rerun-if-changed=migrations` (*observed*, macro docs).
- `#[sqlx::test(migrations = "path")]` reads `DATABASE_URL` at test start,
  creates a `_sqlx_test_*` database per test, applies the migrations, hands
  the test a `PgPool`, and drops the database afterwards (*verified*: two
  tests saw independent tables; no `_sqlx_test%` database remained). Without
  `DATABASE_URL` every such test fails at once (*verified*), so these tests
  must sit behind a Cargo feature that the workspace test run does not enable.
- `AsyncFnOnce(&mut PgConnection) -> Result<T, E>` as the transaction seam
  compiles on 1.98, the closure borrows the connection, and the resulting
  future is `Send` when the closure's is (*verified* under `tokio::spawn`).

Features: `postgres`, `runtime-tokio`, `tls-rustls-ring-webpki`, `migrate`;
`macros` only where `migrate!` or `#[sqlx::test]` is used. Dependency delta
over the current lockfile: 46 crates (*verified* with `cargo tree`), of which
rustls, ring, webpki-roots, and the SCRAM primitives are the bulk; the
workspace had no TLS stack before. `ring` over `aws-lc-rs` because the slim
builder image has no cmake; webpki roots over native roots because the
distroless runtime is the deployment target and the DSN policy admits no root
file anyway. ISC (ring, rustls-webpki, untrusted) and CDLA-Permissive-2.0
(webpki-roots) join the license allow list.

Deferred to the first repository (stage 10): `query!` macros with offline
metadata, which add `sqlx-cli` to the tool manifest and
`cargo sqlx prepare --check` to the quality job; per-query tracing spans
(`sqlx` emits `tracing` events, not spans; the Go template used `otelpgx`).

## DSN admission

No crate refuses what the policy refuses: `sqlx` merges the ambient
environment, accepts sockets, `allow`/`prefer`, TLS files, and unknown keys.
`infra_postgres::Dsn` is template-owned for that named gap: it parses the URL
with the `url` crate already in the tree (a `sqlx-core` dependency), applies
the Go rules, refuses when any of the environment variables `sqlx` reads is
non-empty, and only then hands a `PgConnectOptions` to `sqlx`. Diagnostics
name the rule, never the value. The allowed query parameter set is exactly
`sslmode`; `application_name` is set by the template from the service name so
`pg_stat_activity` attributes sessions.

## Pool, probe, transactions

Constants, not configuration keys, as in Go: acquire 3 s (the Go connect
timeout), `statement_timeout` and `idle_in_transaction_session_timeout` 8 s,
healthcheck bounded by the health budget. `postgres.max_connections` (1..500,
default 4) is the one operator-owned value; the Go key was `max_open_conns`,
renamed to the `sqlx` term. Pool gauges `db_client_connection_count{state}`
follow the OpenTelemetry semantic convention name and are polled by a task on
the existing metrics upkeep cadence from `Pool::size()` and `Pool::num_idle()`.

`in_tx` takes an `AsyncFnOnce(&mut PgConnection) -> Result<T, E>` (edition
2024 async closures; the closure borrows the connection, so it cannot commit
or roll back on its own). `Ok` commits; `Err` rolls back inside a 3 s budget
and returns the caller's error, logging a rollback failure. A commit error
whose SQLSTATE class is `23` (integrity constraint) or `40` (transaction
rollback, except `40003` statement completion unknown) is `CommitFailed`;
every other commit error is `CommitUnknown` and the caller must reconcile
rather than retry. `retryable(&Error)` is `40001` or `40P01`. `TxOptions`
renders the `BEGIN` statement (isolation level, read-only) for
`Connection::begin_with`; stage 10's idempotency store needs serializable.

## Migrations

`crates/migrate` is a library plus a binary. The library embeds
`migrations/` with `sqlx::migrate!` and runs `Migrator` over one
`PgConnection` (never the pool) whose session defaults are
`lock_timeout` 15 s, `statement_timeout` and
`idle_in_transaction_session_timeout` 2 min, and `client_min_messages`
`warning`, under a 5 min `tokio::time::timeout`. Source rules beyond `sqlx`'s
resolver are proven by a unit test over the embedded migrator, not at
runtime: no `-- no-transaction` file, no reversible `.down.sql` file
(forward-only; a rollback is a new forward migration), lowercase
`snake_case` descriptions, a positive version. The run result records
before, target, after, applied count, and duration; a failure carries a
stage (`source`, `connect`, `lock`, `state`, `execute`, `deadline`) mapped
from `MigrateError` and the SQLSTATE. The binary loads the same
configuration as the service, requires `postgres.enabled`, logs one terminal
`migration_run` record, and exits 1 on failure. The migration set is empty
at this stage; `migrations/README.md` states the rules and the runner proves
the empty-history path (the resolver ignores non-`.sql` files).

## Local PostgreSQL and proof

| Candidate | Latest (date) | What it covers | Verdict |
| --- | --- | --- | --- |
| `env/docker-compose.yml` + `#[sqlx::test]` | compose in Docker 29; `postgres:18` (digest-pinned) | One server per run, one database per test, migrations applied by `sqlx`, the same file serves local development, `make test-integration-db`, and the image rehearsal | **selected**: mirrors the Go template's compose file and reuses the `sqlx` test isolation instead of a second container library |
| `testcontainers` + `testcontainers-modules` | 0.28.0 (2026-08-06), 0.15.0 (2026-02-21) | A container per test binary from Rust, Docker socket access, readiness waits | rejected for now: `#[sqlx::test]` reads `DATABASE_URL` from the process environment, so the container would have to publish it before the harness starts; that means either a global constructor or giving up per-test databases. Reopen if compose becomes a prerequisite problem |
| `cargo-nextest` | 0.9.x | Process-per-test, per-test timeouts, retries | not adopted: `acquire_timeout`, the session timeouts, and the orchestration deadline already bound every database test; reopen if a hung test is observed |

The `test/` workspace crate (package `integration-tests`) holds the
database-backed tests behind its `integration` feature: `make test` builds
the crate with no tests to run; `scripts/ci/test-integration-db.sh` brings
compose up on an ephemeral port, exports `DATABASE_URL`, runs
`cargo test --locked -p integration-tests --features integration`, and tears
compose down. `REQUIRE_DOCKER=1` turns a missing Docker into a failure
instead of a refusal. The image rehearsal (`make migration-validate`) starts
compose, runs `/migrate` from the runtime image on the compose network, and
then the lifecycle check with `APP__POSTGRES__ENABLED=true`, so readiness is
proven with a live database. `runtime-image-check.sh` learns the same two
inputs (`RUNTIME_IMAGE_NETWORK`, `RUNTIME_IMAGE_POSTGRES_DSN`) the Go script
has.

## Deviations from the Go template

| Go | Rust | Why |
| --- | --- | --- |
| Runtime migration directory (`/migrations` in the image, `./migrations` locally) validated for symlinks, nesting, file types | `sqlx::migrate!` embeds the files at compile time | The binary is self-contained; the directory checks have nothing to check. A build script re-runs the macro when a file is added |
| Goose `-- +goose Up/Down` sections, `goose validate`, `MigrateDown` for disposable databases | Forward-only simple migrations; `sqlx` checksums; source rules as a unit test | `sqlx` reversible pairs would ship `.down.sql` into production for a path the Go template also refused to expose |
| Goose session locker with lock and unlock timeouts | `sqlx`'s advisory lock bounded by the session `lock_timeout`; the lock dies with the connection | `lock_timeout` applies to `pg_advisory_lock` (*verified*) |
| `pgx` context watcher sends `CancelRequest` on deadline | The orchestration deadline drops the connection; the server-side `statement_timeout` bounds the work | `sqlx` has no cancel-request hook; the session default is the authority either way |
| `otelpgx` spans per statement | Deferred to the first repository | `sqlx` has no span integration; adding one before a query exists is speculative |
| `max_open_conns` | `max_connections` | `sqlx` term |
| `errors.Join` of the callback error and the rollback error | The callback error is returned; the rollback failure is logged | A Rust error type cannot carry two unrelated errors without a new type; the log record is the operator's evidence |
| `testcontainers` named as a candidate in the roadmap | compose + `#[sqlx::test]` | See the proof table |
| The template exempts itself from the static history check (`scripts/profiles` marker) | Deferred to the stage 9 profile markers | No template migration exists yet to author in place |

## Gotchas recorded for the implementation

1. Never log `sqlx::Error::Configuration` from URL parsing verbatim; it may
   quote the offending component. Map to a fixed message.
2. `sqlx` warns with key and value on an unrecognised URL parameter; the
   admission allowlist runs before `sqlx` ever sees the URL.
3. `#[sqlx::test]` needs `DATABASE_URL`; keep it behind a feature.
4. `sqlx::migrate!` needs `build.rs` with `cargo:rerun-if-changed`.
5. `AssertSqlSafe` is the only way to run a non-literal SQL string; every use
   is a review item.
6. A crate named `test` collides with the built-in test crate; the directory
   is `test/`, the package is `integration-tests`.
7. `ensure_migrations_table` raises a `NOTICE` on every run after the first;
   `client_min_messages = warning` on the migration session keeps the log to
   the terminal record.
8. clippy's `allow-*-in-tests` does not cover `tests/*.rs`; the file-level
   allow the service crate's process tests use applies here too, and the
   feature-gated file is linted through `--features integration-tests/integration`.
9. The pool retries a refused target until `acquire_timeout` and surfaces
   `PoolTimedOut`; `ConnectError::Timeout` names that so an unreachable
   database is not reported as pool exhaustion.
