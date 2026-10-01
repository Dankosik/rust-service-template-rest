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
| `infra-postgres` (`crates/infra-postgres`) | Admission of the one connection string (`Dsn`), the pool with the template's session budgets (`connect`), one-connection attach for the migrator (`connect_session`), readiness participation (`PostgresProbe`), the pool and transaction signals, the transaction seam and its commit-outcome policy (`in_tx`, `in_tx_with`, `TxError`, `retryable`). | Business rules, when the pool opens or closes, configuration precedence, what runs inside a transaction. |
| `migrate` (`crates/migrate`) | The embedded migration set (`MIGRATOR`), the runner over one dedicated connection (`run`), read-only embedded-history verification (`verify_history`), the shared history rule, the failure stages, the terminal record; the `migrate` binary. | Schema content, the pool, readiness. |
| `migrations/` | Forward-only SQL files, one transaction each, `<version>_<snake_case>.sql` ([rules](../../migrations/README.md)). | Access code; a repository adapts to the schema, never the reverse. |
| `service-config` (`postgres` section) | `postgres.enabled`, `postgres.dsn` (secret, environment only), `postgres.max_connections`. | DSN shape (the adapter refuses what the driver would accept). |
| `service` bootstrap | Opening the pool before readiness admission when the profile is enabled, verifying the embedded migration history, registering the probe and the gauge task, partial-startup cleanup, closing the pool in the dependency-close stage. | Pool mechanics, migration execution. |
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
`verify-ca`, or `verify-full`. The only other parameter is an optional
`sslrootcert` with an absolute path to the CA bundle of a private
certificate authority (RDS, Cloud SQL, Azure, an in-house CA); it is
admitted only with `verify-ca` or `verify-full`, because `sqlx` ignores it
under `require`, and it adds to the bundled webpki roots. `Dsn::admit`
refuses, without ever quoting the value: an empty string, a non-empty
`PGSSLROOTCERT`, `PGSSLCERT`, `PGSSLKEY`, or `PGOPTIONS` (the libpq
variables `sqlx` would still merge into an explicit URL; the others are
overwritten by a required component), another scheme, an unparsable URL, a
URL fragment, a missing component, `allow`/`prefer`, any other parameter, a
string the driver cannot turn into connect options, and, read back from
the driver's own parse, a Unix socket host or a comma-separated host list.
The result is exactly what the operator wrote; `application_name` is added by the template from
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
| Readiness probe | health `probe_budget` | The refresher bounds the acquire plus ping |
| Pool close at shutdown | 5 s (`DEPENDENCY_CLOSE`) | After background tasks joined, before the telemetry flush |
| Migration `statement_timeout`, idle-in-transaction | 2 min | Session defaults of the one migration connection |
| Migration `lock_timeout` | 15 s | Also bounds the wait for the advisory session lock |
| Migration deadline | 5 min | `tokio::time::timeout` around the whole run |
| Startup history admission | 5 s | One read-only embedded-history check, including pool acquire |

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

`in_tx(&pool, async |tx| ...)` opens a transaction and lends the closure an
opaque `&mut Tx`. The handle is a `sqlx` executor, used the way a
connection is: `sqlx::query(..).execute(&mut *tx)`. It exposes no
constructor or transaction-control methods. The boundary commits on `Ok`
and rolls back on `Err`, returning the closure's error at once: dropping the
sqlx transaction queues the `ROLLBACK`, and the pool's return ping sends it
with its own round trip instead of the caller waiting for one. A rollback
the server rejects fails that ping, and the pool closes the connection
instead of reusing it. A guard discards a connection whose `BEGIN` future
was cancelled, which sqlx 0.9 would return to the pool inside an open
transaction.

PostgreSQL answers `COMMIT` in an aborted transaction with a silent
`ROLLBACK` and `sqlx` does not check the command tag, so a closure that
swallowed a failed statement and returned `Ok` would look committed (pgx
reports the same case as `ErrTxCommitRollback`). The handle closes that gap
by itself: a statement run through it withdraws the proof that the
transaction is alive when it starts and restores it only when the server
answered the whole statement without an error, so a failed, dropped, or
half-read statement leaves it withdrawn. With the proof in hand the boundary
sends a bare `COMMIT`. Without it the boundary first runs `SELECT 1`, which
an aborted transaction answers with `25P02`, and reports
`TxError::CommitFailed` without sending the commit. Read-only transactions
skip the check: nothing they did can be lost.

`infra_postgres::connection(tx)` lends the connection itself for what the
executor does not offer, a savepoint (`connection(tx).begin()`) or an API
that takes a connection. The boundary cannot see what runs there, so the
borrow withdraws the proof until a later statement through the handle
succeeds. A closure that expects a statement to fail runs it under a
savepoint.
`in_tx_with(&pool, TxOptions { isolation, read_only }, work)` renders the `BEGIN`
statement for `Connection::begin_with`. `Isolation::ServerDefault` omits the
isolation clause (server `default_transaction_isolation`);
`Isolation::ReadCommitted` always sends `BEGIN ISOLATION LEVEL READ COMMITTED`.

Commit-outcome policy: a commit error whose SQLSTATE class is `23`
(integrity constraint violation, which a deferred constraint raises at
commit) or `40` (transaction rollback) except `40003` is
`TxError::CommitFailed`, nothing was written, and the caller may retry. So
is any failure of the check ahead of the commit, because the commit was not
sent. Every other commit error, including a broken connection, is
`TxError::CommitUnknown`: the server may have committed, and the caller must
reconcile against the operation's own identity instead of retrying blindly.
`retryable(&sqlx::Error)` is `40001` or `40P01`; there is deliberately no
retry loop, because whether a retry is safe depends on what the caller
already did.

## Signals

Three operator questions, one signal each. Statements are not instrumented:
the slow-statement warning names the ones that matter, and per-query spans
stay deferred (see the decisions below).

| Signal | Question | Labels and fields |
| --- | --- | --- |
| `db_client_connection_count`, `db_client_connection_max` (gauges) | How full is the pool? | `db.client.connection.pool.name` = `postgres`; `db.client.connection.state` in `idle`/`used` on the count |
| `db_client_connection_wait_time_seconds` (histogram) | Do transactions wait for a connection? | `db.client.connection.pool.name`; recorded for a wait that ended in a timeout too |
| `postgres_transaction_duration_seconds` (histogram) | How long does a transaction hold a connection, and how does it end? | `outcome` in `committed`, `rolled_back`, `acquire_failed`, `begin_failed`, `commit_failed`, `commit_unknown`, `cancelled` |
| `postgres_transaction` (client span) | Where did a request's time in the database go? | `db.system.name`, `db.namespace`, `server.address`, `server.port`, `postgres.transaction.outcome`; on the boundary's own failure `error.type` (the SQLSTATE, or the driver's failure class) and `otel.status_code` |

The gauge names and the wait histogram follow the OpenTelemetry database
client conventions; the Prometheus exporter spells dots as underscores. The
wait histogram covers `in_tx` only: a statement a crate runs straight on the
pool acquires inside the driver, which reports no wait. `rolled_back` is the
closure's own `Err` and is not marked as a span error, because whether it is
one is the caller's business rule. The composition root passes each
histogram's buckets to the recorder, as it does for every other crate.

## Supported Deployments

The proven target is one PostgreSQL server reached directly, or through a
TCP load balancer that does not speak the protocol; the database suite runs
against exactly that. What the adapter does on a connection decides what
else can work:

- **A connection pooler in front (PgBouncer, a managed pooler, RDS Proxy) is
  not a tested target.** Every connection publishes its session budgets in
  the startup packet's `options`. PgBouncer refuses a startup parameter it
  does not track unless `ignore_startup_parameters` lists it, and listing
  `options` makes it drop the budgets instead of applying them, so the
  service would run without `statement_timeout` and
  `idle_in_transaction_session_timeout`. In transaction pooling the driver's
  named prepared statements additionally need `max_prepared_statements`. A
  service that must sit behind a pooler sets the budgets on the database role
  (`ALTER ROLE ... SET`) and proves the path with the database suite before
  relying on it.
- **One host.** The DSN admits no host list and no
  `target_session_attrs`; failover is the endpoint's job (a managed
  endpoint, a virtual IP, DNS). After a failover a session on a server that
  became read-only fails its write with `25006`, which `transient` reports.
- **Password authentication with a static password.** Client certificates
  (`sslcert`/`sslkey`) and passwordless or short-lived credentials (IAM
  tokens) are refused by admission or have no path here: the pool is
  opened once with the admitted connect options and nothing renews them.
- **A silently dropped network path is bounded by the server, not the
  client.** `sqlx` 0.9 sets no TCP keepalive and its return-to-pool ping has
  no timeout, so a connection whose peer vanished without a reset holds its
  pool slot until the kernel gives up. Callers still fail inside the acquire
  budget and readiness fails with them; the slot itself comes back late.

## Migrations

`crates/migrate` embeds `migrations/` with `sqlx::migrate!` (the image needs
no migration directory). The runner follows the `sqlx migrate run` sequence
over the `Migrate` trait on one connection whose session defaults are the
migration budgets above, under a `tokio::time::timeout`: lock before reading
history, ensure the history table, refuse a failed row, compare, apply each
pending migration in one transaction with its history row, unlock. A changed
checksum (`VersionMismatch`) or an unknown applied version inside the
embedded range (`VersionMissing`) fails before anything is applied. A
version above the newest embedded one is admitted by both the runner and
startup (the same rule), so a rolled-back release's migrate job and service
both succeed. On failure the connection is dropped;
`client_connection_check_interval = 1s` makes the server end the session,
its lock and transaction promptly.

Source rules beyond the resolver's are a unit test over the embedded set
(`cargo test -p migrate`, part of `make migration-check`): positive version,
simple forward-only files (no `.up.sql`/`.down.sql`), no
`-- no-transaction`, lowercase `snake_case` description.
`scripts/ci/migration-history-check.sh`
refuses a pull request that modifies, deletes, or renames an existing
migration or adds one older than the newest the base has.

The `migrate` binary loads the same configuration as the service, requires
`postgres.enabled = true`, and writes `migration_starting` and one terminal
`migration_run` record (`before`, `target`, `after`, `applied_count`,
`duration_ms`, `outcome` in `success`/`no_change`/`error`). A version field
is omitted when there is none or it was not observed. On error the record
adds `stage` in `config`/`signals`/`connect`/`lock`/`history`/`execute`/
`deadline`/`interrupted` and `error`, which names the failing migration
version for `execute`. It exits 1 on failure. In the image it runs as
`--entrypoint /migrate`; a stop signal drops the run.

The service, and the jobs worker when that pack is retained, never run
migrations at startup. After the pool opens they call
`migrate::verify_history`, which applies the same rule as the runner:
read-only history reads, bounded with its acquire to five seconds, that
require every embedded migration to be applied successfully with its
checksum. An absent or incomplete history is `Pending`
(run the migrator first); a failed row, a checksum mismatch, or an applied
version inside the embedded range that the binary does not embed is
`Mismatch`. A version above the newest embedded migration belongs to a later
release that already migrated the database and is admitted, as Flyway admits
future migrations by default, so a rolled-back binary or a replica restarted
mid-rollout still starts. A migration therefore keeps the previous release
working (expand before contract), which rolling deployment already requires.
The check replaces per-profile table, column, and type probes and does not
prove that an operator left the schema unaltered.

## Proof

Unit tests need no Docker: admission, budgets rendering, commit
classification, source rules, stage mapping. Database-backed proof lives in
`test/tests/postgres.rs` behind the `integration` feature and runs through
`ALLOW_HEAVY=1 make test-integration-db`: session defaults observed with
`SHOW`, probe verdicts including pool exhaustion, commit and rollback,
`CommitFailed` from a deferred constraint and from a swallowed statement
failure, a serialization failure,
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
against it; no other crate names the table. `infra-postgres` owns the opaque
`Tx` and the transaction lifecycle.
`infra_http::idempotency` re-exports the same type, so a feature's port can
name it without a provider dependency, and the feature's provider adapter
runs its statements through it as a `sqlx` executor. The store
arbitrates and persists within that one explicit READ COMMITTED transaction;
a storable 2xx effect and record commit together.
Adapters never issue transaction-control SQL or name the profile table.

New records use native `http_idempotency_header_pair[]` values (`name text`,
`value bytea`) for the seven byte-safe replay headers and retain trusted caller
identity metadata plus scope digest. SQLx 0.9.0's narrow `derive` feature
provides the composite `Type`/`Encode`/`Decode` and array support. Native
`bytea` preserves every header value byte without a binary format or the
extra byte-encoding policy that `jsonb` would require. No query macros or
offline metadata are introduced for these constant statements.
Startup admits the profile schema through the general migration-history
check ([Migrations](#migrations)); the store probes no table, column, or type
and keeps only its writable-session check.

This retargets three persistence deferrals: `query!` with offline `.sqlx`
metadata and `sqlx-cli`, and per-query tracing spans, move from "the first
repository" to the first *feature-owned* repository, because the store's own
statements are template-owned constants proven by the retained database
suite. The canonical profile migration is ordinary embedded history. The static
source gate permits only the reviewed pre-adoption four-file rewrite; runtime
history never recognizes the former migration set.
<!-- template:end http-idempotency:docs-persistence-http-idempotency -->
<!-- template:begin jobs:docs-persistence-jobs -->

## Background jobs profile

`crates/infra-jobs` owns one table, `background_jobs`, and every statement
against it; no other crate names the table. Enqueue (`infra_jobs::enqueue`)
accepts the caller's `&mut Tx` inside the caller's transaction,
under the caller's isolation level, with no transaction-control SQL; the job
commits or rolls back with the caller's write under the commit-outcome
policy this document records. The insert is its only statement: UTF-8 is a
schema precondition that the canonical migration enforces and the
worker's startup check verifies. The jobs worker pool selects session-default
`READ COMMITTED` when it opens each physical connection, including
replacements, and startup refuses another default. Claim, outcome, retention,
and sample statements are individual autocommit statements. Enqueue and
`complete_in_tx` remain caller-owned explicit transactions, preserving their
isolation and commit-outcome meaning.

Retention and sampling temporarily set their existing server statement limits
(one and two seconds respectively) on an acquired session, execute one atomic
statement, then acknowledge reset before returning the connection. Cancellation
or an unacknowledged SET, statement, or RESET closes that connection rather
than returning a session with altered settings to the pool.

The worker's sessions carry a derived `application_name` of the form
`{service_name}-jobs-worker`, with the service name cut to at most 51 bytes
on a character boundary so the suffix survives PostgreSQL's 63-byte limit;
no key controls it. Size `postgres.max_connections` for the worker as at
least `jobs.max_workers + 2` (one connection per concurrent attempt plus the
engine's statements and the readiness probe); the worker refuses less. The
statements are template-owned constants proven by the jobs database suite,
so they adopt no `query!` (the deferral stays at the first feature-owned
repository). The canonical migration includes JSONB payloads, C-collated text
unique keys, and trace state. It is ordinary embedded history. See the
[guide](../background-jobs.md) and
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
  `webpki-roots` (no native-roots variant); a private CA is added through
  the DSN's `sslrootcert`. `aws-lc-sys` lists `cmake` as a build dependency,
  but Linux `gnu`/`aarch64` and `x86_64` use the `cc` builder with
  pregenerated bindings, not cmake-the-tool. The slim builder already
  compiles C through `cc`. `ring` stays in the lockfile as an optional
  dependency of `rustls-webpki` and `quinn-proto` and is not selected on
  the Linux targets cargo-deny evaluates. ISC plus CDLA-Permissive-2.0
  remain on the license allow list for `rustls-webpki` and `webpki-roots`.
- **`Dsn` is template-owned** because no crate refuses what the policy
  refuses: `sqlx` seeds every `PgConnectOptions` from the libpq environment
  (there is no environment-free constructor), reads `.pgpass` when the URL
  has no password, accepts sockets, `allow`/`prefer`, and client key files,
  and warns with key and value on an unknown parameter. `Dsn` checks the
  URL text only for what must be refused before `sqlx` parses it and reads
  the rest back through `PgConnectOptions` getters instead of repeating the
  driver's parser.
- **Session defaults through `PgConnectOptions::options`**: `SHOW` returned
  the published values, `lock_timeout` cancels a waiting `pg_advisory_lock`
  with `55P03`, `statement_timeout` cancels with `57014`.
  The pool pings a connection before handing it out only after more than
  one second idle, as pgx does; the `sqlx` default (`test_before_acquire`)
  pings on every acquire and doubled the round trips of a single-statement
  request. The idle ping still discards a connection the server or a proxy
  closed while it sat in the pool. On release the pool shrinks a
  connection's read and write buffers back to the driver's default: sqlx
  keeps them at the largest message the connection ever carried, so one
  1 MiB body per connection kept about 3 MiB resident per connection.
- **`in_tx` takes an `AsyncFnOnce`** (edition 2024): the closure borrows the
  opaque provider-owned `Tx`, the future is `Send` when the closure's is, and callers pass
  their own error type through `E: From<TxError>`. The Go template joined
  the callback error with the rollback error; Rust returns the callback
  error, and a rollback the server rejects closes the connection.
- **`Tx` implements `sqlx::Executor` and tracks the transaction's state
  itself.** The earlier seam handed out the connection and let a closure
  assert `statement_succeeded(tx)` to skip the check ahead of `COMMIT`; a
  wrong assertion would have reported a rolled-back transaction as
  committed, and every caller that forgot it paid for the check. Observing
  each statement's result removes both. The check that remains is a plain
  `SELECT 1` followed by the driver's own `commit`; the earlier
  `SELECT 1; COMMIT; BEGIN` in one message saved its round trip by relying
  on the driver's transaction-depth counter and its return ping, and is no
  longer worth that coupling now that the check is the rare path.
  `Executor::describe` exists only when the driver's offline support is on,
  which `sqlx`'s `macros` feature enables, so `infra-postgres` enables it to
  see one trait whichever workspace crates are built together; `migrate`
  already brings the macros into every binary.
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
