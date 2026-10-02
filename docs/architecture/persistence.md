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
| `infra-postgres` (`crates/infra-postgres`) | Admission of the one connection string (`Dsn`) and of a rotated password file (`refresh_password_periodically`), the pool with the template's session budgets and their verification (`connect`), one-connection attach for the migrator (`connect_session`), readiness participation (`PostgresProbe`), the pool, transaction and statement signals (`observed`), the transaction seam and its commit-outcome policy (`in_tx`, `in_tx_with`, `TxError`). | Business rules, when the pool opens or closes, configuration precedence, what runs inside a transaction. |
| `migrate` (`crates/migrate`) | The embedded migration set (`MIGRATOR`), the runner over one dedicated connection (`run`), read-only embedded-history verification (`verify_history`), the shared history rule, the failure stages, the terminal record; the `migrate` binary. | Schema content, the pool, readiness. |
| `migrations/` | Forward-only SQL files, one transaction each unless marked `-- no-transaction`, `<version>_<snake_case>.sql` ([rules](../../migrations/README.md)). | Access code; a repository adapts to the schema, never the reverse. |
| `service-config` (`postgres` section) | `postgres.enabled`, `postgres.dsn` (secret, environment only), `postgres.password_file`, `postgres.session_budgets`, `postgres.max_connections`. | DSN shape (the adapter refuses what the driver would accept). |
| `service` bootstrap | Opening the pool before readiness admission when the profile is enabled, verifying the embedded migration history, registering the probe, the gauge task and the password refresh task, partial-startup cleanup, closing the pool in the dependency-close stage. | Pool mechanics, migration execution. |
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

The password has one alternative source, for a platform that rotates it.
`postgres.password_file` names a file that holds the password alone (one
trailing line break is not part of it); the URL then carries no password,
and one in both places is refused, so there is still exactly one source. The
file is read at admission by every binary, the migrator included. In the
service, and the jobs worker where that pack is retained, a task reads it
again every five seconds and
hands a changed password to the pool through `Pool::set_connect_options`,
the driver's own hook for it; connections already open keep their
authenticated session and leave at the pool's maximum lifetime. While the
file is unreadable or empty the pool keeps the last password and logs
`postgres_password_file_unreadable` once per outage; a change logs
`postgres_password_reloaded`. A connection opened between a rotation and the
next read is refused with `28P01`, so a rotation needs either an overlap in
which both passwords work or five seconds of tolerance for new connections.

## Budgets

Constants in `infra-postgres` and `migrate`, not configuration keys: a
service that needs different ones changes them in one reviewed place. The
two exceptions are the values no constant can know, named below the table.

| Budget | Value | Where it acts |
| --- | --- | --- |
| Acquire (including opening a connection) | 3 s | `PgPoolOptions::acquire_timeout`; the startup connection draws it too |
| `statement_timeout` | 8 s | Session default in the startup packet of every pooled connection |
| `idle_in_transaction_session_timeout` | 8 s | Same duration as `statement_timeout` by policy; a separate constant |
| Connection lifetime | 30 min | `PgPoolOptions::max_lifetime`: how long a session outlives a rotated password, a changed role default, or a moved DNS answer |
| Idle connection timeout | 10 min | `PgPoolOptions::idle_timeout`: a pool sized for a peak returns its server slots after it |
| Idle connection ping | 1 s | Bounds the ping a connection idle for over a second gets before it is handed out |
| Password file refresh | 5 s | How often a configured `postgres.password_file` is read again |
| Slow statement warning | 1 s | `warn` with SQL text and duration; statement logging is otherwise off |
| Readiness probe | health `probe_budget` | The refresher bounds the acquire plus ping |
| Pool close at shutdown | 5 s (`DEPENDENCY_CLOSE`) | After background tasks joined, before the telemetry flush |
| Migration `statement_timeout`, idle-in-transaction | 2 min | Session defaults of the one migration connection |
| Migration `lock_timeout` | 15 s | Also bounds the wait for the advisory session lock |
| Migration deadline | `postgres.migration_deadline`, default 5 min | `tokio::time::timeout` around the whole run; also the `statement_timeout` and `lock_timeout` of a `-- no-transaction` migration |
| Startup history admission | 5 s | One read-only embedded-history check, including pool acquire |

`postgres.max_connections` (1..500, default 4) is the operator-owned
capacity value: size it from the database's `max_connections` divided across
every instance and job that shares it, not from the service's concurrency.
`postgres.migration_deadline` (`1s`..`24h`, default `5m`) is read by the
`migrate` binary alone. A concurrent index build takes as long as the table
is large, which differs between the databases one image is deployed to, so
the run that carries one sets `APP__POSTGRES__MIGRATION_DEADLINE` on the
migration job. A transactional migration keeps its two-minute statement
budget and 15-second lock budget whatever the deadline is.

A budget the session does not carry is not a budget. Opening the pool reads
the effective `statement_timeout`, `idle_in_transaction_session_timeout`,
and, where the process requires one, `default_transaction_isolation` back
from the server and refuses to start unless each timeout is a limit no
looser than the template's (`ConnectError::SessionBudget`,
`ConnectError::SessionIsolation`). A stricter limit is the operator's to
choose. `postgres.session_budgets` says where the values come from:

- `startup` (default): the service publishes them in the startup packet of
  every connection. Nothing has to be configured on the server.
- `server`: the service publishes nothing, and the database role or the
  database carries them (`ALTER ROLE app SET statement_timeout = '8s'`, the
  same for `idle_in_transaction_session_timeout`). For a pooler that refuses
  startup parameters; see [Supported Deployments](#supported-deployments).

The migrator always publishes its own budgets and takes a session advisory
lock, so it connects to the server directly whichever value is set.

## Readiness

`PostgresProbe` acquires a pooled connection and pings it. It shares the pool
on purpose: readiness is refreshed by the background refresher, never per
request, so a saturated pool fails the probe there and the instance stops
receiving traffic instead of queueing it. The verdict message names the
failure class (`no connection available inside the acquire budget`,
`connection failed`, ...), never the target.

The cost is that pool saturation is usually shared: instances under the same
load fill their pools together, leave rotation together, and return together
once the pause empties the pools. `health.failure_threshold` requires the
probe to miss the acquire budget in several checks in a row first, and
`http.max_in_flight` bounds how many requests can wait on the pool at once. A
probe on its own connection outside the pool would report reachability only;
it is left out because it costs one more server connection per instance and
its own reconnect and credential path. Reopen that choice if `readiness_lost`
events with the acquire-budget reason recur across instances while PostgreSQL
itself answers.

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
A serialization failure (`40001`) or a deadlock (`40P01`) reaches the caller
with its SQLSTATE (`sqlstate(&sqlx::Error)`). There is deliberately no retry
loop and no helper that suggests one: whether a rerun is safe depends on
what the caller already did.

## Statements

A statement whose text is fixed is written with `sqlx::query!`,
`query_as!`, or `query_scalar!`. The macro checks the SQL, the parameter
types and the result columns at compile time against `.sqlx/`, the
statement metadata committed at the repository root, so a build needs no
database (`.cargo/config.toml` sets `SQLX_OFFLINE`). A column the database
cannot prove non-null is spelled `AS "name!"`, a custom type
`AS "name: Type"`, and a parameter the server cannot infer carries a cast in
the SQL (`$1::interval`, `$2::text::jsonb`). `make sqlx-prepare` regenerates
`.sqlx/` against a throwaway PostgreSQL with the migrations applied; run it
after changing a statement or adding a migration. A statement changed
without it fails the build, and `make sqlx-check` in CI refuses metadata
that no longer matches the statements or the schema. SQL assembled at run
time stays a plain `sqlx::query` behind `AssertSqlSafe`, with its review
reason.

Every statement runs through `infra_postgres::observed("<summary>", ...)`,
which records its duration and gives it a client span. The summary is a
short literal phrase that names what the statement does (`"claim jobs"`,
`"write idempotency record"`), one per statement, never built from a value.

## Signals

Four operator questions, one signal each. The slow-statement warning adds
the SQL text of a statement that took over a second.

| Signal | Question | Labels and fields |
| --- | --- | --- |
| `db_client_connection_count`, `db_client_connection_max` (gauges) | How full is the pool? | `db.client.connection.pool.name` = `postgres`; `db.client.connection.state` in `idle`/`used` on the count |
| `db_client_connection_wait_time_seconds` (histogram) | Do transactions wait for a connection? | `db.client.connection.pool.name`; recorded for a wait that ended in a timeout too |
| `postgres_transaction_duration_seconds` (histogram) | How long does a transaction hold a connection, and how does it end? | `outcome` in `committed`, `rolled_back`, `acquire_failed`, `begin_failed`, `commit_failed`, `commit_unknown`, `cancelled` |
| `db_client_operation_duration_seconds` (histogram) | How long does each statement take, and how does it fail? | `db.system.name` = `postgresql`; `db.query.summary`; on failure `error.type` (the SQLSTATE, the driver's failure class, or `cancelled` when the caller stopped waiting) |
| `postgres_statement` (client span, named by the summary) | Which statement inside a request or a job took the time? | `db.system.name`, `db.query.summary`; on failure `db.response.status_code` (the SQLSTATE), `error.type`, `otel.status_code` |
| `postgres_transaction` (client span) | Where did a request's time in the database go? | `db.system.name`, `db.namespace`, `server.address`, `server.port`, `postgres.transaction.outcome`; on the boundary's own failure `error.type` (the SQLSTATE, or the driver's failure class) and `otel.status_code` |

The gauge names, the wait histogram and the statement histogram follow the
OpenTelemetry database client conventions; the Prometheus exporter spells dots as underscores. The
wait histogram covers `in_tx` only: a statement a crate runs straight on the
pool acquires inside the driver, which reports no wait. `rolled_back` is the
closure's own `Err` and is not marked as a span error, because whether it is
one is the caller's business rule. A statement gets its span only inside
another span: a loop that polls outside any span (the jobs claim, the
cleanup tasks) would otherwise start a one-span trace per tick, so its
statements are measured and not traced. The statement span carries no
server address; the transaction span around it does. The composition root passes each
histogram's buckets to the recorder, as it does for every other crate.

## Supported Deployments

The proven target is one PostgreSQL server reached directly, or through a
TCP load balancer that does not speak the protocol; the database suite runs
against exactly that. What the adapter does on a connection decides what
else can work:

- **PostgreSQL 14 or later.** The migrator sets
  `client_connection_check_interval` and a retained profile's schema may use
  `lz4` compression, both from 14; the database suite runs against 18.
- **PgBouncer 1.26 or later in transaction mode, with the session budgets
  tracked**, is proven by the database suite
  (`env/pgbouncer/pgbouncer.ini`):

  ```ini
  pool_mode = transaction
  track_extra_parameters = statement_timeout, idle_in_transaction_session_timeout, default_transaction_isolation
  ```

  From 1.26 PgBouncer applies a tracked parameter that PostgreSQL does not
  report to the server connection whenever the client becomes active, and
  resets it for clients that did not send it, so every transaction runs
  under the budgets the service published. 1.24 and 1.25 accept the same
  configuration and silently drop the values (observed with 1.24.1 and
  1.25.2: `SHOW statement_timeout` answers `0`), and so does any version
  that lists the parameters in `ignore_startup_parameters`; the check at
  pool opening refuses both. The driver's named prepared statements need
  `max_prepared_statements` above zero, the default since 1.24.
- **A pooler that refuses startup parameters** (a managed PgBouncer without
  that setting, RDS Proxy, another vendor's pooler) is reached with
  `postgres.session_budgets = "server"` and the budgets set on the role or
  the database. The suite proves this path through the same PgBouncer; a
  specific vendor's pooler is the deploying service's to prove. A session
  takes role and database defaults when it starts, so after changing them
  recycle the pooler's server connections before restarting the service.
- **What still needs a session of its own connects directly or through a
  session-mode pooler:** the migrator (advisory lock, its own budgets) and,
  where the jobs pack is retained, its worker's `LISTEN` connection, which
  through a transaction-mode pooler receives nothing and leaves pickup to
  the poll interval.
- **The migration job may use a role of its own.** It reads the same keys,
  so its `APP__POSTGRES__DSN` can name the role that owns the schema and a
  direct endpoint, while the service's names a role with data privileges
  only and the pooler. The template creates no roles and grants nothing:
  with two roles, the owner's default privileges
  (`ALTER DEFAULT PRIVILEGES ... GRANT ... TO <service role>`) are the
  deployment's to set before the first migration.
- **One host.** The DSN admits no host list and no
  `target_session_attrs`; failover is the endpoint's job (a managed
  endpoint, a virtual IP, DNS). After a failover a session on a server that
  became read-only fails its write with `25006`, which `transient` reports.
- **Password authentication.** The password is static in the DSN or
  rotated through `postgres.password_file`, which covers a secrets manager's
  agent, a mounted Kubernetes secret, and a sidecar that writes short-lived
  tokens. Client certificates (`sslcert`/`sslkey`) are refused by admission,
  and the adapter mints no cloud IAM token itself.
- **A silently dropped network path is bounded for idle connections only.**
  `sqlx` 0.9 sets no TCP keepalive. A connection idle for more than a second
  is pinged before use and discarded when the ping takes more than a second,
  so a pool whose peers vanished without a reset (a load balancer's idle
  cut-off, a failed node) sheds one dead connection per second of a
  caller's acquire budget: with three or more of them the first caller
  still times out, and the ones after it get fresh connections. A connection
  that dies while a statement runs is different: the caller's own deadline
  ends the wait, and the driver's return-to-pool ping, which has no timeout,
  holds the pool slot until the kernel gives up on the socket.

## Migrations

`crates/migrate` embeds `migrations/` with `sqlx::migrate!` (the image needs
no migration directory). The runner follows the `sqlx migrate run` sequence
over the `Migrate` trait on one connection whose session defaults are the
migration budgets above, under a `tokio::time::timeout`: lock before reading
history, ensure the history table, refuse a failed row, compare, apply each
pending migration in one transaction with its history row, unlock. Each
applied migration is logged as `migration_applying` and `migration_applied`
(`migration.version`, `migration.transaction`, `migration.duration_ms`), so a
long run shows where it is. A changed
checksum (`VersionMismatch`) or an unknown applied version inside the
embedded range (`VersionMissing`) fails before anything is applied. A
version above the newest embedded one is admitted by both the runner and
startup (the same rule), so a rolled-back release's migrate job and service
both succeed. On failure the connection is dropped;
`client_connection_check_interval = 1s` makes the server end the session,
its lock and transaction promptly.

A file that starts with `-- no-transaction` runs outside a transaction, for
the statement PostgreSQL refuses inside one (`CREATE INDEX CONCURRENTLY`).
Nothing rolls it back and its history row follows it, so the file holds one
statement that is safe to rerun (`IF NOT EXISTS`); PostgreSQL itself refuses
a concurrent build that shares the file with another statement. The runner
does two things for it. It raises the session `statement_timeout` and
`lock_timeout` to the deadline for that statement and restores them
afterwards: the build holds no lock that blocks writes, its duration follows
the table size, and it waits for every transaction older than itself, a wait
`lock_timeout` would otherwise cut at 15 seconds and leave an invalid index.
And it refuses to start while `pg_index` holds an invalid index
(`RunError::InvalidIndexes`, stage `execute`, the index names in the error):
that is what a failed or interrupted concurrent build leaves, and
`IF NOT EXISTS` would otherwise record it as built. The operator drops it
with `DROP INDEX CONCURRENTLY` and runs the job again. The check covers the
whole database, so a `REINDEX CONCURRENTLY` in progress also defers the run;
the index of a partitioned table, invalid until every partition is attached,
is not counted.

Source rules beyond the resolver's are a unit test over the embedded set
(`cargo test -p migrate`, part of `make migration-check`): positive version,
simple forward-only files (no `.up.sql`/`.down.sql`), lowercase `snake_case`
description. `scripts/ci/migration-history-check.sh`
refuses a pull request that modifies, deletes, or renames an existing
migration or adds one older than the newest the base has, and lints the
added files with Squawk: transactional files as wrapped in a transaction,
`-- no-transaction` files as not, where the linter also requires the
rerunnable spelling. A finding fails the check; an intended operation
carries `-- squawk-ignore <rule>` with its reason above the statement. Files
the base already has are not linted, because an applied file cannot change.

The `migrate` binary reads the same configuration sources as the service but
decodes only `app`, `log`, `observability`, and `postgres`, so it needs no
other section's secrets. It requires `postgres.enabled = true`, and writes `migration_starting` and one terminal
`migration_run` record (`before`, `target`, `after`, `applied_count`,
`duration_ms`, `outcome` in `success`/`no_change`/`error`);
`migration_starting` carries the effective `migration.deadline_ms`. A version field
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
failure, a serialization failure, the refusal of a session without the
budgets and the admission of one whose database carries them, the same
transaction behavior through PgBouncer in transaction mode with published
and with server-carried budgets, a rotated password file reaching new
connections, a silent idle connection replaced inside the acquire budget,
read-only refusal, apply-then-no-change, edited and removed history, lock
contention, the deadline, source and connect failures. Each test gets its
own database from `#[sqlx::test]`. `ALLOW_HEAVY=1 make migration-validate`
rehearses the runtime image: `/migrate` against a fresh compose database,
replay is `no_change`, then the lifecycle check with the profile enabled.
`ALLOW_HEAVY=1 make sqlx-check` proves `.sqlx/` against the statements and
a database that holds exactly the embedded migrations.
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
replacements, and startup refuses another default. Claim and outcome
statements are individual autocommit statements. Enqueue and
`complete_in_tx` remain caller-owned explicit transactions, preserving their
isolation and commit-outcome meaning.

Retention and sampling each run one statement in a short `in_tx` transaction
whose first statement is `SET LOCAL statement_timeout` (one and two seconds
respectively). PostgreSQL restores the session limit itself on commit,
rollback, or a dropped connection, so no session returns to the pool with an
altered setting.

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
  `migrate`; `macros` where a checked statement, `migrate!` or
  `#[sqlx::test]` is used) over
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
- **Session budgets are verified, and a pooler is supported two ways**
  (2026-10-02, PgBouncer 1.24.1, 1.25.2, and 1.26.0 against `postgres:18`).
  Publishing in the startup packet stays the default because it needs
  nothing on the server. Per-transaction `SET LOCAL` was rejected: it adds a
  round trip to every transaction and leaves statements run straight on the
  pool unbounded. A session-level `SET` after connect was rejected because a
  transaction pooler hands the next transaction another server connection.
  That leaves the two sources the key names, and reading the effective
  values back is what makes either one safe: it is the only way to notice a
  pooler that accepted the parameters and dropped them. The driver's
  `extra_float_digits` startup parameter is no longer sent
  (`PgConnectOptions::extra_float_digits(None)`): PostgreSQL 12 and later
  already print the shortest exact float by default, and a pooler refuses
  the parameter unless it is told to ignore it.
- **`postgres.password_file` over provider-specific credential code**: a
  file is the interface a secrets manager's agent, a Kubernetes secret
  volume, and a token sidecar already share, and `Pool::set_connect_options`
  is the driver's documented way to rotate. Minting IAM tokens in-process
  would add a cloud SDK to every service for one provider; client
  certificates would need a TLS fixture in the database suite and have no
  consumer yet. Reopen either with the first service that needs it. The
  refresh polls instead of reacting to `28P01` because the driver has no
  before-connect hook; five seconds bounds the window either way.
- **The idle ping is bounded; the return ping is the driver's.** The ping
  before handing out an idle connection is the template's hook, so it
  carries a one-second timeout. The ping `sqlx` sends when a connection
  returns to the pool is not reachable from a hook without paying a second
  round trip on every release.
- **`Pool::begin` is a lint error outside the adapter** (`clippy.toml`
  `disallowed-methods`): the pool is a plain `sqlx` pool, so nothing else
  kept a caller from opening a transaction that skips the commit-outcome
  policy and the transaction signals. A test that needs a raw lock holder
  says so with `#[expect]`.
- **`transaction_timeout` is not set.** It exists from PostgreSQL 17, and
  publishing an unknown parameter fails the connection on 14 to 16. A
  request's transaction is already bounded by the request deadline on the
  client and by the two session budgets on the server; reopen when 17 is
  the minimum supported server.
- **Waiting on `sqlx` after 0.9.0** (merged upstream, unreleased on
  2026-10-02): rollback of a `BEGIN` cancelled in flight (`DiscardOnDrop`
  can then go), `TCP_NODELAY`, a pool `num_idle` underflow that can spin a
  core, and invalidation of stale prepared statements. TCP keepalive and a
  timeout on the return ping are still open there.
- **Embedded migrations, forward-only**: `sqlx::migrate!` replaces the Go
  template's runtime directory with its symlink and nesting checks; a
  `build.rs` `rerun-if-changed=../../migrations` is required because the
  macro tracks the files it embedded, not the directory. Reversible pairs
  would ship `.down.sql` into production for a path the Go template also
  refused to expose. The template ships no feature migration;
  `migrations/README.md` states the rules and the runner proves the
  empty-history path.
- **`-- no-transaction` is admitted, guarded, and bounded by one key**
  (2026-10-02; it was refused before). Without it an index on a table that
  already holds rows can only be built under a lock that blocks writes. The
  alternatives were a statement splitter in the runner (it would parse SQL),
  dropping invalid indexes automatically (an index is also invalid while
  another session builds or reindexes it), and budgets written into the file
  (a concurrent build cannot share its file with a `SET`). The deadline is a
  key because the same image meets a small table in one database and a large
  one in another; the other budgets stay constants.
- **Squawk 2.66 lints added migrations** (through `npx`, as Redocly). It
  knows which PostgreSQL DDL takes which lock; `sqlx` checks none of it and
  the rehearsal runs against an empty database, where every statement is
  fast. `require-lock-timeout` and `require-statement-timeout` are off
  because the session carries both, `prefer-bigint-over-smallint` because
  bounded counts are `smallint` here. Reopen the choice if Node stops being
  a prerequisite: the same linter is a crate (`squawk`), at the cost of
  building it.
- **A pending migration older than an applied one is applied, not refused**,
  as `sqlx` does and unlike Flyway's default. The static check owns the
  order: it refuses such a file in the pull request, and on a push it
  compares with the commit the push replaced, so a branch merged behind its
  base is caught there. A runtime refusal would also stop the rolled-back
  release this history rule exists to admit.
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
- **Statements are checked at compile time** (2026-10-02; this replaces the
  earlier deferral to the first feature-owned repository, because four
  crates already carried SQL that only the database suite checked).
  `sqlx::query!` with offline metadata in one `.sqlx/` at the workspace
  root: per-crate directories were tried and rejected, because
  `cargo sqlx prepare` in a crate also records every statement of the
  workspace crates it depends on. `SQLX_OFFLINE` is forced in
  `.cargo/config.toml` because the database tests export `DATABASE_URL`,
  which would otherwise send every build to that database. `sqlx-cli` is
  pinned in `tools/versions.env` at the `sqlx` version and built from
  crates.io (it publishes no binaries; `rustls`, PostgreSQL driver only).
  The check runs in the `integration` job, which already has the database,
  and a migration now selects that job. An initialized service that drops a
  pack keeps that pack's metadata files: unused metadata is inert, the check
  only warns about it, and the next `make sqlx-prepare` prunes it. The claim
  row decodes into owned strings instead of borrowing from the row; that is
  two small allocations per claimed job.
- **Statement spans and `db.client.operation.duration` come from one
  explicit wrapper, `observed`** (2026-10-02; the Go template used
  `otelpgx`). `sqlx-tracing` 0.2.1 pins `sqlx` 0.8. `sqlx-otel` 0.5.0
  supports 0.9 but depends on `opentelemetry` 0.32 beside the workspace's
  0.33, replaces `PgPool` with its own pool type, and reports through the
  OpenTelemetry meter instead of the `metrics` recorder every other crate
  uses. Its one idea worth keeping is that the caller names the statement,
  because nothing parses SQL; `observed` is that and nothing else. The cost
  is one span and one histogram record per statement and was not measured;
  reopen with a benchmark if a hot path shows it.
- **Connection lifetime and idle timeout are named constants** with the
  driver's own defaults (30 and 10 minutes): password rotation and
  `session_budgets = "server"` both rely on sessions being replaced, so the
  bound is stated instead of inherited.
- **No `retryable` helper**: it had no caller outside tests, and a
  classifier with that name invites the retry loop this adapter refuses to
  own. `sqlstate` and `transient` remain.
- **No exemption from the static history check**: the check already treats a
  migration added in the change range as an addition, including amendments
  before merge, and a merged migration may already have been applied
  elsewhere, so its correction is a new forward migration.
<!-- template:end postgres:docs-persistence-decisions -->
