# Persistence Architecture

The database selection in `template.lock` owns availability. A derived service
with `database: none` has no PostgreSQL provider, migrator, database configuration
or executable database commands. A retained `postgres` profile remains inert
until configured. The source checkout without a lock carries that profile.

<!-- template:begin postgres:docs-persistence-runtime -->
Load for a PostgreSQL pool, repository, transaction, migration, query, or
durable-schema change. Code and crate documentation remain the final factual
authority; this leaf records the decisions and the reasons a later change
should weigh before reopening them.

## Ownership

| Owner | Owns | Does not own |
| --- | --- | --- |
| `infra-postgres` (`crates/infra-postgres`) | Admission of the one connection string (`Dsn`), the pool with the template's session budgets (`connect`), one-connection attach for the migrator (`connect_session`), readiness participation (`PostgresProbe`), pool gauges, the transaction seam and its commit-outcome policy (`in_tx`, `in_tx_with`, `TxError`, `retryable`). | Business rules, when the pool opens or closes, configuration precedence, what runs inside a transaction. |
| `migrate` (`crates/migrate`) | The embedded migration set (`MIGRATOR`), the runner over one dedicated connection (`run`), the source rules beyond the resolver's, the failure stages, the terminal record; the `migrate` binary. | Schema content, the pool, readiness. |
| `migrations/` | Forward-only SQL files, one transaction each, `<version>_<snake_case>.sql` ([rules](../../migrations/README.md)). | Access code; a repository adapts to the schema, never the reverse. |
| `service-config` (`postgres` section) | `postgres.enabled`, `postgres.dsn` (secret, environment only), `postgres.max_connections`. | DSN shape (the adapter refuses what the driver would accept). |
| `service` bootstrap | Opening the pool before readiness admission when the profile is enabled, registering the probe and the gauge task, partial-startup cleanup, closing the pool in the dependency-close stage. | Pool mechanics, migration execution. |
| `test/` (`integration-tests`) | Database-backed proof behind the `integration` feature; fixtures under `test/fixtures/migrations/`. | Anything the service binary runs. |

Future feature crates own persistence ports and business invariants; a
repository maps rows into feature-facing types and joins a caller
transaction only through `in_tx`. Pool mechanics do not own process
lifecycle, HTTP behavior, config precedence, or feature truth.

A new durable behavior evolves `migrations/` first, then writes or adapts
access code from that schema.

## Connection Admission

`postgres.dsn` is a `postgres://` or `postgresql://` URL with an explicit
host, port, user, password, database, and `sslmode` in `disable`, `require`,
`verify-ca`, or `verify-full`, and no other parameter. `Dsn::admit` refuses,
in this order and without ever quoting the value: an empty string, another
scheme, an unparsable URL, a URL fragment, a missing component, a Unix socket
host, a comma-separated host list, `allow`/`prefer`, a service or passfile
parameter, a TLS certificate or key file parameter, any other parameter, a
non-empty libpq variable (`PGHOST`, `PGPASSWORD`, `PGSSLMODE`, ... the
thirteen names in `AMBIENT_ENVIRONMENT`), and a string the driver still
cannot turn into connect options. The result is exactly what the
operator wrote; `application_name` is added by the template from
`observability.otel.service_name` (the same identity traces publish) so
`pg_stat_activity` attributes sessions. A distinct database session label
is not a configuration axis.

## Budgets

Constants in `infra-postgres`, not configuration keys: a service that needs
different ones changes them in one reviewed place.

| Budget | Value | Where it acts |
| --- | --- | --- |
| Acquire (including opening a connection) | 3 s | `PgPoolOptions::acquire_timeout`; the startup connection draws it too |
| `statement_timeout` | 8 s | Session default in the startup packet of every pooled connection |
| `idle_in_transaction_session_timeout` | 8 s | Same duration as `statement_timeout` by policy; a separate constant |
| Slow statement warning | 1 s | `warn` with SQL text and duration; statement logging is otherwise off |
| Rollback after a failed closure | 3 s | `tokio::time::timeout` around `Transaction::rollback` |
| Readiness probe | health `probe_budget` | The refresher bounds the acquire plus ping |
| Pool close at shutdown | 5 s (`DEPENDENCY_CLOSE`) | After background tasks joined, before the telemetry flush |
| Migration `statement_timeout`, idle-in-transaction | 2 min | Session defaults of the one migration connection |
| Migration `lock_timeout` | 15 s | Also bounds the wait for the advisory session lock |
| Migration deadline | 5 min | `tokio::time::timeout` around the whole run |

`postgres.max_connections` (1..500, default 4) is the one operator-owned
value: size it from the database's `max_connections` divided across every
instance and job that shares it, not from the service's concurrency.

## Readiness

`PostgresProbe` acquires a pooled connection and pings it. It shares the pool
on purpose: readiness is refreshed by the background refresher, never per
request, so a saturated pool fails the probe there and the instance stops
receiving traffic instead of queueing it. The verdict message names the
failure class (`no connection available inside the acquire budget`,
`connection failed`, ...), never the target.

## Transactions

`in_tx(&pool, async |conn| ...)` opens a transaction, runs the async closure
with the connection (not the transaction, so the closure cannot commit or
roll back on its own), commits on `Ok`, and rolls back on `Err` inside the
rollback budget, returning the closure's error; a rollback failure is logged.
`in_tx_with(TxOptions { isolation, read_only })` renders the `BEGIN`
statement for `Connection::begin_with`. `Isolation::ServerDefault` omits the
isolation clause (server `default_transaction_isolation`);
`Isolation::ReadCommitted` always sends `BEGIN ISOLATION LEVEL READ COMMITTED`.

Commit-outcome policy: a commit error whose SQLSTATE class is `23`
(integrity constraint violation, which a deferred constraint raises at
commit) or `40` (transaction rollback) except `40003` is
`TxError::CommitFailed`, nothing was written, and the caller may retry.
Every other commit error, including a broken connection, is
`TxError::CommitUnknown`: the server may have committed, and the caller must
reconcile against the operation's own identity instead of retrying blindly.
`retryable(&sqlx::Error)` is `40001` or `40P01`; there is deliberately no
retry loop, because whether a retry is safe depends on what the caller
already did.

## Migrations

`crates/migrate` embeds `migrations/` with `sqlx::migrate!` (the image needs
no migration directory) and runs `sqlx::migrate::Migrator` over one
connection whose session defaults are the migration budgets above, under a
`tokio::time::timeout`. The runner takes the `pg_advisory_lock` (key derived
from the database name) before it reads the history, so `before` and
`applied` describe this run and not a concurrent one; `Migrator::run` takes
the same re-entrant lock again and releases its own count. `sqlx` owns the
append-only history: a checksum mismatch (`VersionMismatch`) or an applied
version missing from the source (`VersionMissing`) fails the run before
anything is applied, and each migration shares one transaction with its
history row. On any failure the connection is dropped, which ends the
session, the lock, and any open transaction.

Source rules beyond the resolver's are a unit test over the embedded set
(`cargo test -p migrate`, part of `make migration-check`) and a runtime
gate in `run`: positive version, simple forward-only files (no `.up.sql`/
`.down.sql`), no `-- no-transaction`, lowercase `snake_case` description.
`scripts/ci/migration-history-check.sh`
refuses a pull request that modifies, deletes, or renames an existing
migration or adds one older than the newest the base has.

The `migrate` binary loads the same configuration as the service, requires
`postgres.enabled = true`, writes `migration_starting` and one terminal
`migration_run` record (`before`, `target`, `after`, `applied_count`,
`duration_ms`, `outcome` in `success`/`no_change`/`error`, and on error
`stage` in `config`/`source`/`connect`/`lock`/`state`/`execute`/`deadline`/
`interrupted` with the `target` and any `before` the run had observed), and
exits 1 on failure. In the image it runs as
`--entrypoint /migrate`; a stop signal drops the run.

## Proof

Unit tests need no Docker: admission, budgets rendering, commit
classification, source rules, stage mapping. Database-backed proof lives in
`test/tests/postgres.rs` behind the `integration` feature and runs through
`ALLOW_HEAVY=1 make test-integration-db`: session defaults observed with
`SHOW`, probe verdicts including pool exhaustion, commit and rollback,
`CommitFailed` from a deferred constraint, a serialization failure,
read-only refusal, apply-then-no-change, edited and removed history, lock
contention, the deadline, source and connect failures. Each test gets its
own database from `#[sqlx::test]`. `ALLOW_HEAVY=1 make migration-validate`
rehearses the runtime image: `/migrate` against a fresh compose database,
replay is `no_change`, then the lifecycle check with the profile enabled.
[PostgreSQL Validation](../validation/postgres.md) selects the commands.
<!-- template:end postgres:docs-persistence-runtime -->

<!-- template:begin http-idempotency:docs-persistence-http-idempotency -->
## HTTP idempotency profile

With the HTTP idempotency profile retained, `crates/infra-idempotency-store`
owns one profile table, `http_idempotency_records`, and every statement
against it; no other crate names the table. An idempotent operation's
repository adapter joins the boundary's transaction through the opaque `Tx`
handle the store passes to the operation's work. The store opens that one
transaction with `in_tx_with` under `READ COMMITTED`, so the adapter's writes
and the success record commit together under the commit-outcome policy this
document already records (`CommitFailed`, `CommitUnknown`, `retryable`). The
adapter reaches the connection only through the store's
`connection(&mut Tx<'_>)` free function, never through a method or
conversion on `Tx`; it never ends the transaction with transaction-control
SQL and never names the profile table. The schema is the profile's one
migration in `migrations/`, and the statements are constants in the store
crate; the [guide](../http-idempotency.md) covers the rollout sequence.

This retargets three persistence deferrals: `query!` with offline `.sqlx`
metadata and `sqlx-cli`, and per-query tracing spans, move from "the first
repository" to the first *feature-owned* repository, because the store's own
statements are template-owned constants proven by the retained database
suite. No migration-history exemption is adopted: the profile migration is
ordinary forward-only history from the moment it merges.
<!-- template:end http-idempotency:docs-persistence-http-idempotency -->
<!-- template:begin jobs:docs-persistence-jobs -->

## Background jobs profile

`crates/infra-jobs` owns one table, `background_jobs`, and every statement
against it; no other crate names the table. Enqueue (`infra_jobs::enqueue`)
runs on the caller's `&mut PgConnection` inside the caller's transaction,
under the caller's isolation level, with no transaction-control SQL; the job
commits or rolls back with the caller's write under the commit-outcome
policy this document records. It rejects a non-UTF-8 session before sending
JSONB/text data. Every worker statement runs in its own explicit `READ
COMMITTED` transaction through `in_tx_with`, so a stricter server
default cannot turn `SKIP LOCKED` claims into serialization failures.

The worker's sessions carry a derived `application_name` of the form
`{service_name}-jobs-worker`, with the service name cut to at most 51 bytes
on a character boundary so the suffix survives PostgreSQL's 63-byte limit;
no key controls it. Size `postgres.max_connections` for the worker as at
least `jobs.max_workers + 2` (one connection per concurrent attempt plus the
engine's statements and the readiness probe); the worker refuses less. The
statements are template-owned constants proven by the jobs database suite,
so they adopt no `query!` (the deferral stays at the first feature-owned
repository). The schema includes the creation migration and
`20260925000001_simplify_background_jobs.sql`, which converts payload to
JSONB and unique keys to C-collated text, adds trace-state, and requires a
stopped-producer/worker conversion. Both are ordinary forward-only history
from the moment they merge. See the [guide](../background-jobs.md) and
[Async Architecture](async.md).
<!-- template:end jobs:docs-persistence-jobs -->

## Decisions Recorded Here

These decisions apply when the local selection retains PostgreSQL.

<!-- template:begin postgres:docs-persistence-decisions -->
From the stage 8 research (versions read 2026-09-18; behavior verified in a
scratch project against `postgres:18.4`):

- **`sqlx` 0.9** (`postgres`, `runtime-tokio`, `tls-rustls-aws-lc-rs`,
  `migrate`; `macros` only where `migrate!` or `#[sqlx::test]` is used) over
  `tokio-postgres` + `deadpool-postgres` + `refinery` (four crates, no lock in
  `refinery`), `diesel-async` (a schema DSL and a code generation step with no
  query to serve yet), `sea-orm` (an ORM over `sqlx`; a feature may add it
  later), `bb8-postgres` (pool only, quiet since 2024). One crate covers the
  pool, migrations with checksums and an advisory lock, and per-test
  databases; `SqlSafeStr` makes a dynamic SQL string a compile error unless
  wrapped in `AssertSqlSafe`, so every such use is a review item.
- **`aws-lc-rs` over `ring`, webpki roots for PostgreSQL**: rustls's
  process-default provider is `aws-lc-rs`, the same one `reqwest` uses for
  OTLP HTTPS; enabling both providers leaves rustls without a default and
  panics at first use. sqlx 0.9's aws-lc-rs feature only ships
  `webpki-roots` (no native-roots variant); the DSN policy admits no
  root-certificate file. `aws-lc-sys` lists `cmake` as a build dependency,
  but Linux `gnu`/`aarch64` and `x86_64` use the `cc` builder with
  pregenerated bindings, not cmake-the-tool. The slim builder already
  compiles C through `cc`. `ring` stays in the lockfile as an optional
  dependency of `rustls-webpki` and `quinn-proto` and is not selected on
  the Linux targets cargo-deny evaluates. ISC plus CDLA-Permissive-2.0
  remain on the license allow list for `rustls-webpki` and `webpki-roots`.
- **`Dsn` is template-owned** because no crate refuses what the policy
  refuses: `sqlx` seeds every `PgConnectOptions` from the libpq environment
  (there is no environment-free constructor), reads `.pgpass` when the URL
  has no password, accepts sockets, `allow`/`prefer`, TLS files, and warns
  with key and value on an unknown parameter.
- **Session defaults through `PgConnectOptions::options`**: `SHOW` returned
  the published values, `lock_timeout` cancels a waiting `pg_advisory_lock`
  with `55P03`, `statement_timeout` cancels with `57014`.
  `test_before_acquire` stays at the `sqlx` default (`true`).
- **`in_tx` takes an `AsyncFnOnce`** (edition 2024): the closure borrows the
  connection, the future is `Send` when the closure's is, and callers pass
  their own error type through `E: From<TxError>`. The Go template joined
  the callback error with the rollback error; Rust returns the callback
  error and logs the rollback failure.
- **Embedded migrations, forward-only**: `sqlx::migrate!` replaces the Go
  template's runtime directory with its symlink and nesting checks; a
  `build.rs` `rerun-if-changed=../../migrations` is required because the
  macro tracks the files it embedded, not the directory. Reversible pairs
  would ship `.down.sql` into production for a path the Go template also
  refused to expose. The template ships no feature migration;
  `migrations/README.md` states the rules and the runner proves the
  empty-history path.
- **The advisory lock is bounded by `lock_timeout`, not by a locker with its
  own timeouts** (Goose), and **the orchestration deadline drops the
  connection instead of sending a cancel request** (`pgx`): `sqlx` has
  neither hook, and the server-side session defaults are the authority
  either way.
- **compose + `#[sqlx::test]` over `testcontainers`**: `#[sqlx::test]`
  reads `DATABASE_URL` from the process environment and gives every test its
  own database with migrations applied, so a container library would have to
  publish the URL before the harness starts or give up per-test isolation.
  `env/docker-compose.yml` serves local development, `make
  test-integration-db`, and the image rehearsal. The database tests sit
  behind a Cargo feature because they fail at once without `DATABASE_URL`.
  Reopen if compose becomes a prerequisite problem.
- **`cargo-nextest` not adopted**: the acquire, session, and deadline
  budgets already bound every database test; reopen if a hung test is
  observed.
- **Renamed from Go**: `max_open_conns` is `max_connections` (the `sqlx`
  term).
- **Deferred to the first feature-owned repository**: `query!` macros with
  offline `.sqlx` metadata (adds `sqlx-cli` to the tool manifest and
  `cargo sqlx prepare --check` to the quality job); per-query tracing spans
  (`sqlx` emits `tracing` events, not spans; the Go template used `otelpgx`).
- **No exemption from the static history check**: the check already treats a
  migration added in the change range as an addition, including amendments
  before merge, and a merged migration may already have been applied
  elsewhere, so its correction is a new forward migration.
<!-- template:end postgres:docs-persistence-decisions -->
