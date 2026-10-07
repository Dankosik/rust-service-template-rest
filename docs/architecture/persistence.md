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
| `infra-postgres` (`crates/infra-postgres`) | Admission of the one connection string (`Dsn`) and of a rotated password file (`refresh_password_periodically`), native pool preparation and bounded session admission (`prepare_pool`, `admit_pool`, and convenience `connect`), one-connection attach for the migrator (`connect_session`), readiness participation (`PostgresProbe`), named native query-pool acquisition (`acquire`), the pool, transaction and statement signals (`observed`), the transaction seam and its commit-outcome policy (`in_tx`, `in_tx_with`, `TxError`). | Business rules, when the pool opens or closes, configuration precedence, what runs inside a transaction. |
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
Only a parsed IPv6 literal is normalized: `[::1]` becomes bare `::1` in
`PgConnectOptions`, for both the socket destination and TLS IP verification.
DNS names, IPv4 and every other admitted option keep the driver's value.
`application_name` is added by the template from
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
the driver's own hook for future connections. This changes only the password;
the admitted database username and other connection policy remain fixed.
Missing, unreadable, empty or non-UTF8 content leaves the last-good options in
place and logs `postgres_password_file_unreadable` once per outage. A changed
password logs `postgres_password_reloaded` after updating options, before any
proof that the server accepts it.

Five seconds is the polling cadence, not a cutover deadline. External file
delivery, read duration (the reader has no separate timeout), scheduling and
server acceptance also matter. Until new options are installed and accepted,
new connections can fail with `28P01`. Where supported, arrange provider overlap
for the whole transition; otherwise plan for interrupted new authentication.
Verify a fresh authenticated connection, not only a successful query on an
existing one. Follow the [publication and verification sequence](../configuration-source-policy.md#rotation-and-revocation).

Open sessions retain their authentication. The main pool's 30-minute maximum
lifetime retires aged connections at SQLx pool lifecycle points, including
return and idle maintenance; it does not forcibly interrupt checked-out
sessions or establish a hard revocation deadline. Provider session controls
and the appropriate connection/process lifecycle own emergency termination.

For the resolved SQLx 0.9 rustls backend, an explicit `sslrootcert` file is read
while constructing TLS for a new handshake. Its roots add to bundled WebPKI
roots; supplying a private CA does not make that CA the exclusive trust store.
`sslmode=require` encrypts the connection without certificate/hostname identity
verification; `verify-ca` verifies the chain, and `verify-full` also verifies
the server name. Replacing or removing a root file does not revalidate existing
TLS sessions. Trust removal also needs the relevant session lifecycle and
resumption policy; a file reread alone is not revocation.

New TCP dials use system DNS and race the resolved addresses in the existing
SQLx Tokio branch. The first successful TCP stream reaches TLS/PostgreSQL
opening; all losing attempts are dropped before that continuation. A stalled
first address therefore cannot starve a healthy later address. Resolution with
no addresses retains `InvalidInput`; all failures retain the error for the last
resolver-order address, regardless of completion order. There is no new retry
or timer: the pool's three-second acquire budget, or the direct caller's
deadline, covers resolution, TCP and protocol opening. An already-started
blocking system resolver call can outlive cancellation of its owned wait.

A new dial observes the current resolved addresses; established sockets are not
migrated. The dedicated jobs LISTEN connection retains its existing reconnect
and polling owner outside the query pool.

New TCP dials use system DNS and race the resolved addresses in the existing
SQLx Tokio branch. The first successful TCP stream reaches TLS/PostgreSQL
opening; all losing attempts are dropped before that continuation. A stalled
first address therefore cannot starve a healthy later address. Resolution with
no addresses retains `InvalidInput`; all failures retain the error for the last
resolver-order address, regardless of completion order. There is no new retry
or timer: the pool's three-second acquire budget, or the direct caller's
deadline, covers resolution, TCP and protocol opening. An already-started
blocking system resolver call can outlive cancellation of its owned wait.

A new dial can observe changed DNS; an existing socket is not migrated. The
30-minute lifetime retires pooled connections through return/idle checks, and
the ten-minute idle timeout evicts idle connections. Neither interrupts a busy
checked-out session. With `verify-ca` or `verify-full`, the `sslrootcert` file
is read on each new TLS connection; replacing that file changes subsequent
connections, not existing sessions. Bundled webpki roots follow the binary.
The dedicated jobs LISTEN connection uses its existing reconnect and polling
owner, outside the query pool and its lifetime/idle policy.

## Budgets

Constants in `infra-postgres` and `migrate`, not configuration keys: a
service that needs different ones changes them in one reviewed place. The
two exceptions are the values no constant can know, named below the table.

| Budget | Value | Where it acts |
| --- | --- | --- |
| Acquire (including opening a connection) | 3 s | `PgPoolOptions::acquire_timeout`; pool admission acquires once under this ceiling |
| Pool session admission | 5 s | Absolute client deadline around acquisition and the complete verification on the same connection; partial replies do not restart it |
| Failed convenience `connect` close | 5 s | Cleanup ceiling after failed admission; primary refusal is preserved |
| `statement_timeout` | 8 s | Session default in the startup packet of every pooled connection |
| `idle_in_transaction_session_timeout` | 8 s | Same duration as `statement_timeout` by policy; a separate constant |
| Connection lifetime | 30 min | `PgPoolOptions::max_lifetime`: age threshold for retirement at pool lifecycle points; no forced interruption of checked-out sessions |
| Idle connection timeout | 10 min | `PgPoolOptions::idle_timeout`: a pool sized for a peak returns its server slots after it |
| Idle connection ping | 2 s | Hang guard on the ping a connection idle for over a second gets before it is handed out; a discard logs `postgres_idle_ping_failed` |
| Native connection return | 5 s | Whole SQLx return operation, including callback, ping and graceful close; expiry drops its local connection/slot ownership |
| Password file refresh | 5 s | How often a configured `postgres.password_file` is read again |
| Slow acquisition warning | 1 s | Successful named acquisition exceeding this threshold; code-owned, without a new operator key |
| Slow statement warning | 1 s | `warn` with SQL text and duration; statement logging is otherwise off |
| Readiness probe | health `probe_budget` | The refresher bounds the acquire plus ping |
| Pool close at shutdown | 5 s (`DEPENDENCY_CLOSE`) | Shared with forced background completion accounting, before telemetry flush; close uses only the remaining stage/process time |
| Migration `statement_timeout`, idle-in-transaction | 2 min | Session defaults of the one migration connection |
| Migration `lock_timeout` | 15 s | Also bounds the wait for the advisory session lock |
| Migration deadline | `postgres.migration_deadline`, default 5 min | `tokio::time::timeout` around the whole run; also the `statement_timeout` and `lock_timeout` of a `-- no-transaction` migration |
| Startup history admission | 5 s | One read-only embedded-history check, including pool acquire |

`postgres.max_connections` (1..500, default 4) is the operator-owned
capacity ceiling for each process's pool. Allocate it across the deployment as
[described below](#connection-allocation), then size within that allowance from
measured work and database pressure; the ceiling alone is not a throughput target.
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

Both modes require verification before `connect` returns the native pool. Its
single five-second client timeout covers acquisition and the complete readback;
partial replies do not restart it. Expiry returns the sanitized typed
`ConnectError::SessionVerificationTimeout`, distinct from initial acquire
`ConnectError::Timeout`. The server's eight-second statement timeout cannot
bound a silent network. A verification mismatch or timeout requests pool close
and waits at most five seconds, retaining the original error even if cleanup
expires. Only successful close proves completed local cleanup; expiry does not
prove remote socket termination. Cancellation drops the caller-owned future
without a detached verification or retry task, or a promise of awaited cleanup.

Initial acquisition, verification and rejection cleanup are sequential:
3 + 5 + 5 = 13 seconds of allocated waiting with a runnable, yielding scheduler.
This is not a whole-bootstrap or process-exit deadline. Embedded-history
admission is a separate existing five-second step. The native SQLx return bound,
shutdown close budget and `connect_session` remain separate owners.

The migrator always publishes its own budgets and takes a session advisory
lock, so it connects to the server directly whichever value is set.

Process bootstrap uses synchronous `prepare_pool(&Dsn, &PoolOptions)` and
retains the native pool before awaiting `admit_pool(&PgPool, &PoolOptions)`.
Admission acquires exactly one connection and verifies the required session
settings on it within an absolute 5 s client deadline. The native acquire
ceiling remains 3 s; the 8 s server statement timeout cannot bound a silent
network or extend the client deadline.
A peer that stops replying after acquire therefore cannot hold admission open
indefinitely. `ConnectError::SessionVerificationTimeout` is a sanitized refusal
distinct from acquire `ConnectError::Timeout`; session requirements and
isolation checks stay unchanged.

The existing convenience `connect` delegates to these same operations. On
admission failure it attempts pool close for at most 5 s, so its sequential
waits allocate at most 5 s admission plus 5 s failure cleanup with a runnable,
yielding scheduler. This is not a whole-bootstrap or process-exit deadline.
Incomplete close is recorded separately and cannot replace the primary refusal.
Only successful close proves completed local cleanup; expiry does not prove
remote socket termination. Cancellation drops the caller-owned future without
a detached verification or retry task, or a promise of awaited cleanup.
Service and worker startup instead cancel admission on stop and clean their
retained pool under the process's existing 5 s dependency stage and original
stop deadline. SQLx still owns cancelled connection return under its existing
5 s native bound; a stopped waiter or incomplete close does not establish
physical backend termination. Embedded-history verification remains a separate
5 s admission step. The native SQLx return bound, shutdown close budget and
`connect_session` remain separate owners.

### HTTP attempts

The supported HTTP PostgreSQL paths, idempotency execution and inbound webhook
receipt, use the hardened chain's existing `RequestDeadline`. Each bounds the
complete store or receipt future at `RequestDeadline.at() - 100 ms`, including
acquisition, BEGIN, all statements and user work, bounded response-body capture,
and COMMIT. Time already spent reading the body or handling
the request reduces what remains; database entry never starts a fresh budget.

The 100 ms reserve is for the current bounded in-memory terminal response
mapping, which performs no further database or provider I/O. It reuses the
smallest admitted complete HTTP request budget as a conservative allocation
for that smaller task; it is not a measured response-delivery SLA. Reopen the
allocation if terminal mapping gains asynchronous I/O or unbounded body work,
or measurements show it inadequate. With the default eight-second request
and the full three-second acquire budget spent, at most 4.9 seconds remain
for transaction work and COMMIT, leaving 0.1 seconds for mapping. Native
connection cleanup can continue after that foreground attempt for up to five
seconds; it does not consume the response reserve. The 4.9 seconds is not a
separate statement limit or an additional timeout.

An exhausted cutoff returns the existing 503 unavailable response before the
operation is polled, with no database dispatch. A configured 100 ms HTTP
request, or database entry with 100 ms or less remaining, therefore admits no
database attempt. Positive remaining time permits one best-effort attempt;
there is no promise that BEGIN or COMMIT can finish. The inner cutoff produces
`idempotency_unavailable` (with its existing retry hint) or `service_unavailable`
for webhook receipt; the outer deadline retains `gateway_timeout` 504.
Neither response establishes non-commit. Recover using the same idempotency
key or webhook message identity, including after cancellation during COMMIT
or after acknowledgement while response delivery is pending.
There is no automatic retry.

The eight-second session statement and idle-in-transaction limits remain
server fallbacks for jobs, autocommit work and a lost client. They do not bound
an arbitrary multi-statement transaction. Jobs, maintenance and migration
budgets retain their own owners; the HTTP reserve changes no session setting.
Future request paths must adopt the existing deadline explicitly. Callers
still own meaningful transaction boundaries and effects outside PostgreSQL.

## Connection allocation

For direct PostgreSQL, use **peak simultaneously live process counts**, including
old and replacement replicas during a rolling deployment. Count every service
and jobs-worker group separately, since their pool limits can differ. The units
below are configured PostgreSQL session allocations, not average active queries:

```text
sum(peak_service_replicas * service_pool_max)
+ sum(peak_worker_replicas * worker_pool_max)
+ simultaneous_LISTEN_sessions + direct_migrators + other_direct_sessions
+ other_applications + administrative_and_reserved_allowance
<= PostgreSQL max_connections
```

Readiness already uses each process's shared pool and needs no extra session
in this sum. A worker process with running engines keeps one separate `LISTEN`
session; ordinary jobs and an outbox publisher in that process share it. Count
that listener in addition to its pool, including replacement workers. Dedicated
migrators connect directly and count once per simultaneously running migrator.
Include maintenance tools, consoles and other applications, and preserve the
server's reserved slots and an operational allowance for administration.

The worker's startup validation is a lower bound within this allocation:

| Worker mode, where retained | Minimum `postgres.max_connections` per process |
| --- | --- |
| Ordinary jobs, with `N = jobs.max_workers` | `N + 2` |
| Ordinary jobs plus outbox publisher | `N + 5` |
| Outbox publisher alone | `3` |

The separate LISTEN session is additional to these pool minima. If the global
allocation cannot accommodate a mode's minimum, the proposed deployment does
not fit: revise the simultaneous replica/worker allocation or database allowance
before adopting it. A smaller rejected pool setting is not a sizing solution.
The service pool default remains four; worker configuration must satisfy its
actual mode independently.

For example, a direct database with `max_connections = 100` could allocate:

| Owner | Peak allocation | Sessions |
| --- | --- | ---: |
| Service replicas | Three plus one rolling replacement, pool maximum four | 16 |
| Ordinary-jobs worker replicas | Two plus one replacement, each with four workers and pool maximum six | 18 |
| Worker LISTEN sessions | One per live worker process | 3 |
| Dedicated migrators | Two running simultaneously | 2 |
| Other direct sessions | Maintenance and consoles | 3 |
| Other applications | Their combined allocated maximum | 25 |
| Administrative and reserved allowance | Server reservations and operational access | 15 |
| **Total allocated** | **16 + 18 + 3 + 2 + 3 + 25 + 15** | **82** |
| **Unallocated headroom** | **100 − 82** | **18** |

These are illustrative allocations, not observed usage or an optimal production
pool size. They leave headroom as well as the explicit administrative allowance.
The [silent-connection behavior](#supported-deployments) still matters: a local
slot can be reclaimed before PostgreSQL has terminated its old physical session.
Transient lingering sessions and replacements can therefore exceed the local
pool totals. Database/pooler enforcement and operational headroom remain
necessary; the formula does not promise an instantaneous physical-session cap
through a network failure.

### Through PgBouncer

Keep two separate allocations. Application pool maxima and LISTEN or other
clients entering the pooler count against its **client-connection** limits.
PostgreSQL sees the pooler's **server connections**, whose allocation follows
its configured pools rather than the sum of application clients.

Count peak pooler replicas, including rolling overlap. For each replica,
account for the actual database/effective-user partitions, their normal and
reserve server-pool sizes, and the database/user caps that constrain them.
Apply both caps where they overlap; do not add a shared cap as another pool.
Sum the resulting server allowances across pooler replicas and add direct
consumers outside those allocations, other applications and PostgreSQL's
administrative/reserved allowance. Check this backend total against the same
PostgreSQL maximum, and check client allocations separately against each
pooler's applicable total/database/user client limits. Use the deployed
configuration rather than assumed defaults.

Migrators retain their direct or session-mode connection requirement. LISTEN and
other session-bound consumers need a direct or session-mode path as described under
[Supported Deployments](#supported-deployments); count each once in its actual
direct or pooler server allocation. Transaction pooling neither removes those
owners nor makes an application client equal to one PostgreSQL backend.

### Choosing a pool size within the allocation

Use the existing [signals](#signals) together: acquisition waits/timeouts and
pool occupancy show contention at checkout; request/job latency and database
CPU, I/O, lock waits and transaction age show whether more concurrency would
help. The transaction wait histogram has the coverage stated there; use the
named acquisition events for direct statements and readiness too.

Sustained waits with busy application pools and spare database capacity can
justify a bounded comparison of another pool size. Saturated database resources,
long-held locks or long transactions can become worse with more concurrent
connections; address the limiting work or reduce offered concurrency first.
Low occupancy alongside slow operations points away from pool capacity as the
first explanation. Short waits during the documented cleanup window are not
proof that the configured pool is too small.

For a later sizing experiment, record the workload, replica overlap, pool and
worker settings, and relevant database limits. Keep a representative bounded
workload fixed, change one value within the allocation and worker minima, and
compare acquisition outcomes, useful-work latency, database pressure and
readiness loss/recovery. Stop and restore the previous setting if latency,
timeouts or database pressure worsen. The [validation guide](../validation/postgres.md)
defines how to retain the observation and its limits. Sustained correlated
readiness loss or acquisition pressure under representative load reopens the
sizing/readiness decision; local recovery alone establishes no fleet optimum.

## Readiness

`PostgresProbe` acquires a pooled connection and pings it. It shares the pool
on purpose: readiness is refreshed by the background refresher, never per
request, so a saturated pool fails the probe there and the instance stops
receiving traffic instead of queueing it. The verdict message names the
failure class (`no connection available inside the acquire budget`,
`connection failed`, ...), never the target.

An acquire-budget failure establishes local inability to acquire a connection;
it does not establish a database outage. A connection/query failure identifies
that instance's dependency path, not a fleet-wide diagnosis. Each instance owns
its pool and readiness state. Correlated load can fill several pools together,
causing them to leave rotation together and recover when pressure falls;
pressure confined to one pool does not itself mutate another instance's state.
`health.failure_threshold` requires the
probe to miss the acquire budget in several checks in a row first, and
`http.max_in_flight` bounds how many requests can wait on the pool at once. A
probe on its own connection outside the pool would report reachability only;
it is left out because it costs one more server connection per instance and
its own reconnect and credential path. Reopen that choice if `readiness_lost`
events with the acquire-budget reason recur across instances while PostgreSQL
itself answers.

Responsive saturation and a silent network fault are distinct observations.
After held connections are released, ordinary work and the next successful
background refresh can recover with the same pool maximum. The current default
policy uses a two-second interval, four-second probe budget and three consecutive
failures to withdraw an established ready verdict; one successful refresh
restores it. Failed and timed-out completed rounds preserve process progress;
they do not trigger a restart. The mandatory process progress observer instead
fails a root whose armed completion gap exceeds the existing 16-second freshness
bound. This distinguishes unavailable dependencies from a stuck readiness
driver, as described in [Runtime Lifecycle](runtime-lifecycle.md#readiness-and-liveness).
At current defaults, phase plus serial rounds gives illustrative withdrawal
times of about 6/11/14 seconds for fast/acquire-budget/probe-budget failures,
before any platform delay. These are not production SLOs.
The three-second native acquire may time out during five-second
return cleanup, so an immediate replacement failure does not prove retention.
The local [recovery proof](../validation/postgres.md) records timing under this
policy. Recovery requires newly completed useful database work and a fresh
successful readiness round in the same instance, not only a reachable listener,
cached success or metrics scrape. The existing database consumer composition
owns the two-instance HTTP/gRPC observation; actual-root process fixtures own
bootstrap, no-diagnostics topology and exit behavior. The finite shared-database
composition establishes instance-local isolation, not independent OS scheduling
or fleet capacity. Reopen capacity or readiness choices only with sustained acquisition
waits/timeouts, correlated readiness loss and server/load evidence from a
representative workload; the bounded test does not establish fleet stability.

## Transactions

`in_tx(&pool, async |tx| ...)` opens a transaction and lends the closure an
opaque `&mut Tx`. The handle is a `sqlx` executor, used the way a
connection is: `sqlx::query(..).execute(&mut *tx)`. It exposes no
constructor or transaction-control methods. The boundary commits on `Ok`
and rolls back on `Err`, preserving the closure's error. Dropping the SQLx
transaction queues `ROLLBACK`; the bounded return below flushes it before a
connection can be reused. Cleanup failure does not replace the caller's error
or an acknowledged commit with a different outcome.

### Query-pool checkout and cancellation

The pool remains a native `PgPool`. Named template operations acquire through
`acquire(&pool, operation)` for diagnostics, then borrow the native connection;
`in_tx[_with]` owns application transaction control. Acquisition adds no timeout,
retry, cleanup task or replacement connection type. The former `Checkout` and
`with_connection` cleanup shim has been removed.

Dropping a pooled connection starts SQLx's native return. The temporary
[sqlx-core backport](../../vendor/sqlx-core/PATCHES.md) bounds the whole owned
return future at five seconds, including rollback flushing, buffer shrinking,
ping and graceful-close branches. Expiry drops its connection and capacity
guard without a peer acknowledgement. Healthy return retains reuse. This
assumes the Tokio runtime can progress; it establishes local capacity recovery,
not server rollback, database reachability or COMMIT finality. Return runs after
the foreground operation and cannot change its result. An immediately waiting
acquisition can exhaust its shorter three-second budget before cleanup finishes.

Pending BEGIN keeps the narrower `DiscardOnDrop` protection, armed until BEGIN
acknowledges: SQLx 0.9.0 has not armed its own rollback guard before that reply.
The connection closes under SQLx's existing five-second close-on-drop bound
instead of being recycled with an unacknowledged transaction. Later cancellation
uses native transaction drop and protocol resynchronization. Neither path
establishes non-execution or prevents already buffered protocol dispatch.

Transactions, session verification, readiness, startup history inspection,
idempotency startup and jobs query operations use named acquisition. Raw
`Pool::acquire` bypasses only those diagnostics; the direct-acquire lint keeps
named production paths observable. Native `PgPool` execution and the jobs
listener's private pool receive the library return bound too. The migrator's
dedicated session retains its separate lifecycle owner.

This five-second library-owned policy supersedes the previous application
shim's immediate cancellation disposal, one-second awaited return, and the
older requirement to reclaim within the three-second acquisition budget.
HTTP cutoff and same-identity recovery, transaction truth and pending-BEGIN
protection are preserved. Remove the backport only after an acceptable published
SQLx version supplies equivalent bounded return and passes the affected proof;
its [custody record](../../vendor/sqlx-core/PATCHES.md) owns that condition.

### Transaction truth

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

Pool occupancy, acquisition pressure and statement/transaction outcomes answer
different operator questions. The slow-statement warning adds the SQL text of
a statement that took over a second.

| Signal | Question | Labels and fields |
| --- | --- | --- |
| `db_client_connection_count`, `db_client_connection_max` (gauges) | How full is the pool? | `db.client.connection.pool.name` = `postgres`; `db.client.connection.state` in `idle`/`used` on the count |
| `db_client_connection_wait_time_seconds` (histogram) | Do transactions wait for a connection? | `db.client.connection.pool.name`; recorded for a wait that ended in a timeout too |
| `postgres_pool_acquire_slow` (warn event) | Which named operation acquired successfully after waiting over one second? | `pool="postgres"`, static `operation`, `elapsed_seconds`, `threshold_seconds=1` |
| `postgres_pool_acquire_timeout` (warn event) | Which named operation exhausted native acquisition? | `pool="postgres"`, static `operation`, `elapsed_seconds`, `budget_seconds` from that native pool's options |
| `postgres_transaction_duration_seconds` (histogram) | How long does a transaction hold a connection, and how does it end? | `outcome` in `committed`, `rolled_back`, `acquire_failed`, `begin_failed`, `commit_failed`, `commit_unknown`, `cancelled` |
| `db_client_operation_duration_seconds` (histogram) | How long does each statement take, and how does it fail? | `db.system.name` = `postgresql`; `db.query.summary`; on failure `error.type` (the SQLSTATE, the driver's failure class, or `cancelled` when the caller stopped waiting) |
| `postgres_statement` (client span, named by the summary) | Which statement inside a request or a job took the time? | `db.system.name`, `db.query.summary`; on failure `db.response.status_code` (the SQLSTATE), `error.type`, `otel.status_code` |
| `postgres_transaction` (client span) | Where did a request's time in the database go? | `db.system.name`, `db.namespace`, `server.address`, `server.port`, `postgres.transaction.outcome`; on the boundary's own failure `error.type` (the SQLSTATE, or the driver's failure class) and `otel.status_code` |

The gauge names, the wait histogram and the statement histogram follow the
OpenTelemetry database client conventions; the Prometheus exporter spells dots as underscores. The
wait histogram covers `in_tx`/`in_tx_with` only, including their failed acquire;
the new events do not broaden that histogram or count another attempt. `rolled_back` is the
closure's own `Err` and is not marked as a span error, because whether it is
one is the caller's business rule. A statement gets its span only inside
another span: a loop that polls outside any span (the jobs claim, the
cleanup tasks) would otherwise start a one-span trace per tick, so its
statements are measured and not traced. The statement span carries no
server address; the transaction span around it does. The composition root passes each
histogram's buckets to the recorder, as it does for every other crate.

Acquisition events come from the ordinary `infra_postgres::acquire` entry point
and the same observer around native `connect_with`. They observe the driver's
future without another timeout, retry, connection wrapper or return policy.
Fast success has no event. Only native `PoolTimedOut` emits the timeout event;
`PoolClosed` and statement execution errors keep their existing failure paths.
Dropping an unfinished acquire emits neither success nor an invented timeout.
No DSN, raw error, SQL, parameter, user or request-derived label is included.

Coverage is explicit: `connect`, `check session budgets`, `transaction`,
`readiness`, `check migration history`, and, in retained profiles, `claim jobs`,
`complete jobs batch`, `record job outcome`, `check jobs startup`, and
`check idempotency startup`. Jobs maintenance, idempotency arbitration/cleanup
and webhook receipt work already enter the transaction boundary and inherit
its single acquire observation. Existing statement durations keep their
boundaries, including acquisition inside session/idempotency startup checks.

The adapter turns native SQLx acquire-time and slow-acquire logs off to avoid
repeating these events. Arbitrary callers executing directly through the raw
native `PgPool` are not intercepted. The private jobs LISTEN transport keeps its
existing listen/reconnect signals; dedicated migration sessions and SQLx test
administrative pools are outside shared-pool observation. Native pools still
receive the dependency's bounded-return behavior independently of observation.

Whole cleanup passes also expose `postgres_cleanup_active_passes`, confirmed
batch/row counters, terminated-pass counters and elapsed-second histograms.
The `cleanup` label identifies a retained owner; `outcome` is `completed`,
`budget_exhausted`, `failed` or `cancelled`. Budget exhaustion is a non-error
admission yield only for a policy that supports it; current P0 retains its
unbounded repetition. Direct and concurrent calls are observed; duration
includes admission and database waits. Only known committed batches contribute
progress, including empty terminal batches; an unknown commit is not zero
durable work. The info-level `postgres_cleanup_pass_finished` event attributes
elapsed seconds and confirmed totals to one pass. Counters reset per process;
active gauges aggregate active passes, not backlog or capacity. See
[PostgreSQL maintenance observation](../postgres-maintenance.md) for the full
signal contract, current retention policies, automatic dated populations,
conservative example rules and bounded, timestamped manual catalogue diagnostics.
Row deletion, vacuum space reuse and WAL retention
are separate operator questions.

Each retained table owner samples its earliest eligible timestamp through the
existing read-only transaction helper, without row locks or SKIP LOCKED. Each
attempt includes acquisition and acknowledgement in an eight-second client
budget, with local statement/lock/idle limits of 2 s/100 ms/5 s. A successful
empty result is explicit; failed or clock-invalid reads retain last-good dated
data. No schema, index, pool or configuration key changes.
<!-- template:begin jobs:docs-persistence-maintenance-failed -->
Jobs additionally observes every current failed kind without granting
retention/discard authority.
<!-- template:end jobs:docs-persistence-maintenance-failed -->
Independent 30-second post-attempt delays keep cleanup and sampling progress
separate. These observations are bounded result sets, not exact inventories or
physical-I/O bounds. The guide owns replica expectation, clocks and unknown-state
interpretation; measurement must justify any replacement cleanup policy.

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
- **Silent connections have bounded idle checking and native return.**
  `sqlx` 0.9 sets no TCP keepalive. A connection idle for more than a second
  is pinged before use and discarded when the ping takes more than a second;
  multiple dead idle connections can still exhaust a caller's three-second
  acquire budget. During SQL, the caller's existing deadline ends its wait.
  The native return then has a five-second whole-operation bound, including
  rollback flushing, the final ping and graceful-close branches. Expiry
  drops the abandoned local connection and pool-slot ownership without a peer
  acknowledgement. The same bound applies after successful SQL if the peer
  goes silent before return finishes. Healthy connections remain reusable.
  Pending BEGIN keeps its existing close-on-drop guard and five-second bound.

  Cleanup can outlast the three-second acquire budget: an immediately waiting
  request may time out, while later work succeeds once local capacity is free
  and replacement connectivity permits it. No retry, restart or larger pool
  is part of this recovery. Local capacity release does not establish rollback,
  non-execution or COMMIT finality, and does not prove immediate termination of
  a physical PostgreSQL backend; old sessions can linger beside replacements.
  The caller's deadline/error path and `CommitUnknown` policy remain unchanged.

## Backup and restore custody

The derived service chooses managed backups/PITR or dump/restore, its recovery
point and time objectives, retention and custodian. Retain the database data,
migration history and sequences together; account explicitly for roles, grants,
extension availability and versions, plus separate secret/key custody. A database
dump alone is not a backup of every deployment dependency. Prove the selected
restore on its actual provider/version and record observed results in the
[Production Contract](../production-contract.md#operation-and-recovery).

PostgreSQL recovery does not restore independent broker or object state. Keep
claims/writes/effects fenced while restoring into isolation and reconciling those
stores under the Production Contract. Missing records cannot always be rebuilt
from surviving stores, and readiness is not restore proof.

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
successful version above the newest embedded one is admitted by both the runner
and startup (the same rule). This admits newer migration history; it does not
prove an old release's SQL or application data remains compatible. On failure the
connection is dropped;
`client_connection_check_interval = 1s` makes the server end the session,
its lock and transaction promptly.

Expand the schema before deploying code that uses it. Keep old readers/writers
compatible throughout rolling overlap. Destructive contraction waits until every
incompatible reader/writer and the relevant rollback/restore need has retired
under service policy; prefer an additive repair and roll forward. Startup history
admission and readiness cannot authorize that contraction.

A schema change that changes a warmed prepared statement's argument or result
shape can require recycling PgBouncer server connections so it is prepared again
(see [PgBouncer prepared statements](https://www.pgbouncer.org/config.html#max_prepared_statements)).
Use the deployed pooler's controlled reconnect procedure for that change; do not
require `RECONNECT` for every migration. This does not make incompatible old SQL
compatible. Migration and LISTEN paths retain their direct/session-mode rules and
the worker retains polling fallback under [Supported Deployments](#supported-deployments).

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
extra byte-encoding policy that `jsonb` would require. These statements use
checked query macros and root `.sqlx/` metadata, and `observed` records their
statement timing and spans under the shared [statement policy](#statements).
Startup admits the profile schema through the general migration-history
check ([Migrations](#migrations)); the store probes no table, column, or type
and keeps only its writable-session check.

The canonical profile migration is ordinary embedded history. The static
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
policy this document records. Enqueue inserts once and may issue its debounced
notification on the same transaction. UTF-8 is a
schema precondition that the canonical migration enforces and the
worker's startup check verifies. The jobs worker pool selects session-default
`READ COMMITTED` when it opens each physical connection, including
replacements, and startup refuses another default. Claim and outcome
statements are individual autocommit statements. Enqueue and
`complete_in_tx` remain caller-owned explicit transactions, preserving their
isolation and commit-outcome meaning.

Retention and sampling each run one statement in a short `in_tx` transaction
whose first statement installs a transaction-local `statement_timeout` (five
seconds for retention and two for sampling). PostgreSQL restores the session limit itself on commit,
rollback, or a dropped connection, so no session returns to the pool with an
altered setting.

The worker's sessions carry a derived `application_name` of the form
`{service_name}-jobs-worker`, with the service name cut to at most 51 bytes
on a character boundary so the suffix survives PostgreSQL's 63-byte limit;
no key controls it. Size `postgres.max_connections` for the worker as at
least `jobs.max_workers + 2` (one connection per concurrent attempt plus the
engine's statements and the readiness probe); the worker refuses less. The
statements use checked query macros with root `.sqlx/` metadata and
`observed` instrumentation; the jobs database suite owns their observed behavior. The canonical migrations include JSONB payloads, C-collated text
unique keys, trace state, and additive `recovery_history` with a separate
concurrent failed-kind index. They are ordinary embedded history. Failed rows
remain until explicit redrive/discard; only completed rows expire after 24 hours.
The jobs operator locks one failed identity/version in the caller's transaction;
redrive archives the cycle and obtains a fresh claim-generation sequence value
before rejoining live uniqueness. An exact live-key conflict rolls back the
archive/reset. Successful provider results remain provisional until commit is
acknowledged; uncertain commit never authorizes automatic retry. Inspection is
payload-free and read-only. The short-lived operator pool uses one connection,
`READ COMMITTED`, fixed `application_name=jobs-worker-operator`, the existing
session budgets and embedded-history admission; mutation also requires a
writable session. No operator code performs startup DDL or resets the sequence.
See the

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
  before-connect hook. Five seconds is a read cadence; delivery, read time and
  provider acceptance prevent an unconditional cutover bound.
- **SQLx owns the five-second whole-return bound** (2026-10-04).
  The bounded idle ping remains the template's hook. A release hook cannot
  bound the driver's later ping or early close branches, so the template
  carries a temporary backport in the published sqlx-core 0.9.0 dependency.
  There is no application pool/Executor facade or extra release round trip.
  [Source provenance and exact patch](../../vendor/sqlx-core/PATCHES.md) record
  the verified archive, upstream reference, source-only locked resolution and
  the whole-return and TCP-candidate deltas. The PostgreSQL profile owns the
  excluded dependency and its Cargo/Docker carrier together. Retire each patch
  only when an acceptable published SQLx release supplies its equivalent:
  bounded return/cancellation/reuse/finality, or candidate progress/loser
  cleanup/resolver-order failure respectively. Remove their shared carrier
  when both replacements are established.
- **`Pool::begin` and direct acquisition are lint errors outside their owners**
  (`clippy.toml` `disallowed-methods`). The pool remains plain SQLx; transaction
  entry must retain commit-outcome policy, and named acquisition retains
  operation diagnostics. A test that needs raw ownership says so with `#[expect]`.
- **`transaction_timeout` is not set.** It exists from PostgreSQL 17, and
  publishing an unknown parameter fails the connection on 14 to 16. Current
  HTTP paths enforce the complete attempt cutoff above; server statement and
  idle limits remain fallbacks, not a transaction-duration bound. Reopen when
  17 is the minimum supported server.
- **Waiting on `sqlx` after 0.9.0** (merged upstream, unreleased on
  2026-10-02): rollback of a `BEGIN` cancelled in flight (`DiscardOnDrop`
  can then go), `TCP_NODELAY`, a pool `num_idle` underflow that can spin a
  core, and invalidation of stale prepared statements. TCP keepalive remains
  upstream work. The local whole-return backport above is separate from those
  changes and makes no upstream release claim.
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
  retirement policy is stated instead of inherited. It is not a forced
  session-revocation timer.
- **No `retryable` helper**: it had no caller outside tests, and a
  classifier with that name invites the retry loop this adapter refuses to
  own. `sqlstate` and `transient` remain.
- **No exemption from the static history check**: the check already treats a
  migration added in the change range as an addition, including amendments
  before merge, and a merged migration may already have been applied
  elsewhere, so its correction is a new forward migration.
<!-- template:end postgres:docs-persistence-decisions -->
