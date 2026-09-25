# Background jobs

Select `JOBS=postgres` (or `--jobs postgres`) during initialization. It
requires `DATABASE=postgres` and combines with every `AUTHN`,
`OUTBOUND_HTTP`, and `HTTP_IDEMPOTENCY` choice. The default `none` removes
the pack: the `infra-jobs` and `jobs-worker` crates, the `jobs`
configuration section, the migration, the tests, the `/jobs-worker` image
entrypoint, this guide, and the architecture leaf
[Async Architecture](architecture/async.md).

Retaining the pack changes nothing by itself. The service makes no jobs
query, starts no jobs task, adds no readiness probe, and runs no jobs
startup check. It touches the job table only when business code calls
`enqueue`. Nothing starts a worker, and `/jobs-worker` runs only where an
operator deploys it.

What the pack gives is a durable job enqueued in the same PostgreSQL
transaction as the business write, so the job exists exactly when that write
committed, and a separate worker process that runs each committed job at
least once. The worker bounds every attempt with a timeout, retries with
persisted backoff inside a bounded budget, recovers the jobs of a lost
worker, ends failures in explicit terminal states, and starts, reports
readiness, drains, and exits under the same rules as the service.

This assumes a plain operation and a persistence port. See
[First Production Feature](first-production-feature.md) for that base
workflow and [Persistence Architecture](architecture/persistence.md) for the
transaction seam the sections below join.

## Define a job kind

A job kind is a payload type that implements `infra_jobs::JobKind`: serde
`Serialize` and `DeserializeOwned` (an owned `Deserialize` derive satisfies
that bound), `Send`, `Sync`, `'static`, and `const NAME: &'static str`.
Declare it in the adapter crate that enqueues it
(`crates/infra-<provider>`), never in a feature crate. Features never depend
on `infra-jobs`. The adapter calls the feature's use case; the feature does
not reach the job table.

The name is 1 to 64 characters of lowercase ASCII letters, digits, `.`,
`_`, or `-`, starting with a letter, for example `widgets.welcome`. The
name is stored with every job and is the kind's durable identity. A worker
claims only kinds it registers, so renaming a kind strands its live jobs
under the old name. They stay live, and visible as unregistered, until a
worker registers that name. Retention does not delete them while they are
live ([Retries, failures, and retention](#retries-failures-and-retention)).

The payload serializes to JSON of at most 262 144 bytes (256 KiB). It
carries identifiers, not business data copies or secrets
([Metrics, records, and data custody](#metrics-records-and-data-custody)).
A kind is not a queue, a priority, or a tenant. Jobs have no ordering
guarantee between them.

```rust,ignore
// crates/infra-widgets/src/jobs.rs
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Welcome {
    pub widget_id: i64,
}

impl infra_jobs::JobKind for Welcome {
    const NAME: &'static str = "widgets.welcome";
}
```

## Enqueue in the transaction

`infra_jobs::enqueue(conn, &payload, options)` takes the caller's open
transaction connection: the `&mut PgConnection` an `infra_postgres::in_tx`
or `in_tx_with` closure receives, including a handler's own transaction. It
never opens, commits, or rolls back a transaction, issues no
transaction-control SQL (`COMMIT`, `ROLLBACK`, `SAVEPOINT`), and never
acquires another connection. The caller does not name `background_jobs`;
`enqueue` owns its statement.

`EnqueueOptions` has `delay` and `unique_key`, and implements `Default`.
`delay` defaults to zero, is at most 36 500 days, and is measured on the
database clock from the enqueue statement (the job may be claimed after that
time; no worker observes it before the transaction commits). A
sub-microsecond part is dropped. `unique_key` is `Option<&str>`: absent
means the job has no unique key, and a present key is 1 to 255 bytes of
UTF-8 without control characters. The producer sets no attempt budget or
timeout. Those are the kind's policy in the worker
([Register kinds](#register-kinds)).

`Enqueued` is `#[must_use]`: `Created(JobId)` or `Duplicate`. The `JobId` is
the job's stable identifier, a UUID printed lowercase and hyphenated. The
handler receives the same id.

Validation returns before any statement is sent. Nothing is written, and the
caller's transaction stays usable:

| Variant | When |
| --- | --- |
| `InvalidKind` | the kind name does not match the grammar |
| `InvalidUniqueKey` | the unique key is empty, longer than 255 bytes, or contains a control character |
| `InvalidDelay` | the delay exceeds 36 500 days |
| `Serialize` | the payload cannot be serialized as JSON |
| `PayloadTooLarge` | the serialized payload exceeds 262 144 bytes |

`EnqueueError::Database` means the statement failed, for example a missing
jobs schema or a read-only session.
The caller's transaction is aborted, as after any failed statement. Classify
it with `infra_postgres::retryable` and retry the whole transaction only
when that says so.

The job exists exactly when the caller's transaction commits. A rollback, or
a `CommitFailed` commit, leaves no job. After `CommitUnknown` the job exists
exactly when the business write does, and the caller reconciles as the
persistence commit-outcome policy already requires
([Transactions](architecture/persistence.md#transactions)). No worker
observes an uncommitted job.

**Uniqueness.** With a unique key, at most one live job exists per kind and
key. A live job is `pending` (waiting for its not-before time, including a
scheduled retry) or `running` (held by a claim). A terminal job
(`completed`, or `failed`) does not hold its key. When an uncommitted
concurrent transaction is writing the key's holder (inserting it or changing
its state), enqueue first waits for that transaction inside the session's
statement budget ([Budgets](architecture/persistence.md#budgets)). A wait
that exceeds that budget fails the statement (`EnqueueError::Database`) and
aborts the caller's transaction. The caller's isolation level then decides:

| Caller isolation | The key's holder, after any wait | Enqueue result |
| --- | --- | --- |
| any | none, or only terminal jobs (including one that became terminal after the caller's snapshot) | the job is written |
| `READ COMMITTED` | a live job | `duplicate`: nothing written, the transaction stays usable |
| `REPEATABLE READ`, `SERIALIZABLE` | a live job whose current row version the caller's snapshot sees | `duplicate`, as above |
| `REPEATABLE READ`, `SERIALIZABLE` | a live job inserted or written after the caller's snapshot by another transaction, the engine's own included | fails with the serialization failure `40001`, which aborts the caller's transaction and which the existing `retryable` policy classifies |

`duplicate` in the table is `Enqueued::Duplicate`. The engine's own writes
that touch a live job are the claim, the claim upkeep every 10 s while the
job runs, the release of an attempt at the end of a drain, and scheduling a
retry. So under `REPEATABLE READ` or `SERIALIZABLE`, an enqueue whose unique
key is held by a running job meets `40001` whenever an upkeep write followed
the caller's snapshot; `READ COMMITTED` never does. An ordinary `in_tx`
closure runs at the server's default isolation; `in_tx_with` names a level.

Uniqueness is not effect idempotency. It suppresses duplicate enqueues while
a job is live. It neither stops a job from running more than once nor stops
a later enqueue once the holder is terminal
([Make effects idempotent](#make-effects-idempotent)).

```rust,ignore
// crates/infra-widgets/src/lib.rs
infra_postgres::in_tx(pool, async |conn| {
    let widget_id: i64 = sqlx::query_scalar(
        "INSERT INTO widgets (name) VALUES ($1) RETURNING id",
    )
    .bind(name)
    .fetch_one(&mut *conn)
    .await?;
    let key = widget_id.to_string();
    match infra_jobs::enqueue(
        conn,
        &Welcome { widget_id },
        infra_jobs::EnqueueOptions {
            unique_key: Some(&key),
            ..Default::default()
        },
    )
    .await?
    {
        infra_jobs::Enqueued::Created(_) | infra_jobs::Enqueued::Duplicate => {
            Ok(widget_id)
        }
    }
})
.await
```

The closure's error converts `infra_postgres::TxError`, as every `in_tx`
closure's error must, and `infra_jobs::EnqueueError`. `Duplicate` leaves the
transaction usable, so the widget insert can still commit.

<!-- template:begin jobs-http-idempotency:docs-background-jobs-idempotency -->

## Enqueue from an idempotent operation

An HTTP-idempotent operation's persistence adapter enqueues through
`infra_jobs::enqueue(connection(tx), &payload, options)`, where `connection`
is `infra_idempotency_store::connection`. Such an adapter also depends on
`infra-jobs`, besides the feature and `infra-idempotency-store`. The
`POST /widgets` adapter inserts the widget and enqueues `widgets.welcome`
with the widget id as the unique key on that connection.

The job commits together with the business write and the success record, or
not at all. A replayed, refused, in-progress, or rolled-back attempt
enqueues nothing. The boundary's transaction is always explicit
`READ COMMITTED`, so a live holder of the unique key gives `Duplicate`
(after waiting for an uncommitted writer of the holder), never `40001`, and
the transaction stays usable. Treat `Duplicate` as success for that
transaction: turning it into an error would roll the widget back.

An `EnqueueError::Database` aborts the boundary's transaction, so the
adapter returns an error and the work answers non-2xx, which rolls back. The
adapter still issues no transaction-control SQL and names neither
`http_idempotency_records` nor `background_jobs`: the enqueue seam owns its
statement. The rest of the boundary's rules are in
[HTTP idempotency](http-idempotency.md#do-the-work-inside-the-transaction).

```rust,ignore
// crates/infra-widgets/src/lib.rs
use infra_idempotency_store::connection;

let widget_id: i64 = sqlx::query_scalar(
    "INSERT INTO widgets (name) VALUES ($1) RETURNING id",
)
.bind(&input.name)
.fetch_one(connection(tx))
.await?;
let key = widget_id.to_string();
match infra_jobs::enqueue(
    connection(tx),
    &Welcome { widget_id },
    infra_jobs::EnqueueOptions {
        unique_key: Some(&key),
        ..Default::default()
    },
)
.await?
{
    infra_jobs::Enqueued::Created(_) | infra_jobs::Enqueued::Duplicate => {}
}
```

<!-- template:end jobs-http-idempotency:docs-background-jobs-idempotency -->

## Write a handler

A handler implements `infra_jobs::Handler<K>`. Any
`Fn(Job<K>) -> Future<Output = Result<(), JobError>>` that is
`Send + Sync + 'static`, whose future is `Send + 'static`, does. A named
`async fn` is such a function.

`Job<K>` gives `id()`, `kind()` (`K::NAME`), `attempt()` (attempts used,
including this one), `payload()`, `cancellation()`, and `pool()` (the
worker's pool).

**Name the closure's parameter.** A closure handler must name its parameter
type, `|job: Job<Welcome>| async move { .. }`, or be a named `async fn`. An
unannotated closure does not infer its kind and fails to compile with E0282
(type annotations needed).

`Ok(())` completes the job. `JobError::retryable(e)` and
`JobError::permanent(e)` build the other two results; both take
`impl Display`. `?` on any `std::error::Error` (`Send + Sync + 'static`)
is retryable. A panic is caught, never ends the worker, and counts as a
retryable failure. What each result does to the job is under
[Retries, failures, and retention](#retries-failures-and-retention).

`job.cancellation()` fires at the kind's timeout, at the end of a drain, and
when the claim is lost. The handler task is also aborted, which takes effect
at its next await point. Work started with `tokio::task::spawn_blocking` is
not stopped by that abort, so check the token inside blocking loops
(`job.cancellation().is_cancelled()`), and expect the job to run again while
such work may still be running
([Make effects idempotent](#make-effects-idempotent)).

Reach the database through `job.pool()` with `infra_postgres::in_tx`, where
the handler may enqueue follow-up jobs atomically with its own writes. Hold
at most one pooled connection at a time: the worker's pool reserves one
connection per concurrent attempt. The session budgets apply:
`statement_timeout` and `idle_in_transaction_session_timeout` 8 s, acquire
3 s ([Budgets](architecture/persistence.md#budgets)). A handler that needs a
longer statement raises the limit for its own transaction only, with
`SET LOCAL statement_timeout = '...'` as that transaction's first statement.

Error messages become stored summaries. Never quote payload values in them
([Metrics, records, and data custody](#metrics-records-and-data-custody)).

```rust,ignore
// crates/infra-widgets/src/jobs.rs
pub async fn welcome(job: infra_jobs::Job<Welcome>) -> Result<(), infra_jobs::JobError> {
    record_welcome(job.id(), job.payload().widget_id).await?;
    Ok(())
}

pub fn welcome_handler(client: Client) -> impl infra_jobs::Handler<Welcome> {
    move |job: infra_jobs::Job<Welcome>| {
        let client = client.clone();
        async move {
            client.send_welcome(job.id(), job.payload().widget_id).await?;
            Ok(())
        }
    }
}
```

The closure captures a provider client and clones it per call. Name the
effect pattern beside the handler.

## Make effects idempotent

At least once: every committed job of a registered kind runs at least once
while a worker for its kind runs and its database is available, until it
completes or fails terminally. A job may run more than once: after a lost
claim (the worker stopped extending it, so the job is claimable again 30 s
after the last extension), after a timeout of non-cooperative work, and when
its effect committed but its outcome was not recorded. A claim is permission
to run one attempt, not proof that the attempt is the only one.

A handler makes each effect idempotent, keyed on the job identifier or on a
business key. `job.id()` is stable across attempts, and a UUID, so it is
unique across environments that share a provider account and across database
rebuilds. Use one of three patterns, and name the pattern each kind uses
next to its handler:

1. A unique effect write in its own transaction. Record the effect under a
   unique constraint on the job id (or the business key) in the same
   transaction as the effect, so a second run finds it and does nothing.
2. A provider idempotency key derived from the job identifier (for example
   `welcome-<job id>`), so the provider deduplicates a repeated call.
3. Reconciliation. Before repeating an effect, read the target's state by
   the job id or business key and skip what already happened.

Completing the job inside the handler's own transaction is not offered, so a
database-only effect also needs the first pattern: the worker records the
outcome after the handler returns. Enqueue uniqueness does not replace any
of this ([Enqueue in the transaction](#enqueue-in-the-transaction)).

## Register kinds

The shipped `crates/jobs-worker/src/main.rs` calls
`jobs_worker::run(std::env::args_os(), None)`. The binary refuses with:

```text
no job kind is registered: register this service's job kinds in crates/jobs-worker/src/main.rs
```

A service writes a `register` function of type `jobs_worker::Register` in
that `main.rs` (the sketch below shows it), passes `Some(register)`, and
adds its adapter crates to `crates/jobs-worker`'s dependencies.

`Kinds::register(policy, handler)` registers each kind once and returns
`&mut Self`. `Policy { max_attempts, timeout }` is code, not configuration.
`Policy::default()` is 25 attempts and 60 s. The bounds are 1 to 25 attempts
and 1 s to 1 h. The registered policy of the worker that claims a job
governs that claim.

`Support` gives `config()`, `tracker()`, and `shutdown()` for building
provider clients. `shutdown()` is a new child of the worker's root
cancellation token, cancelled at the background-join stage. Registration
runs after configuration is loaded and checked and before any database I/O.
Kinds and handlers live in adapter crates and call the feature's use cases.

Failures refuse startup with exit `1`. The function's own error is
`job kind registration failed: ...`. A duplicate kind, an invalid name, or
an invalid policy is `job kinds are invalid: ...`. A registration that
adds no kind gives `job kinds are invalid: no job kind is registered`.

```rust,ignore
// crates/jobs-worker/src/main.rs
fn register(
    kinds: &mut infra_jobs::Kinds,
    support: &jobs_worker::Support<'_>,
) -> Result<(), jobs_worker::BuildError> {
    let client = infra_widgets::client(
        support.config(),
        support.tracker(),
        support.shutdown(),
    )?;
    kinds.register(infra_jobs::Policy::default(), infra_widgets::welcome_handler(client));
    Ok(())
}

fn main() -> std::process::ExitCode {
    jobs_worker::run(std::env::args_os(), Some(register))
}
```

## Configure and size the worker

`jobs.max_workers` (`APP__JOBS__MAX_WORKERS`) is the most attempts one worker
process runs at once. The default is `1`. The range is `1` to `500`. Every
binary validates the `jobs` section. Only the worker uses it.

The worker refuses `postgres.max_connections` below `jobs.max_workers + 2`:

```text
configuration is invalid: postgres.max_connections: must be at least jobs.max_workers + 2 (N) for the jobs worker
```

That is one connection per concurrent attempt plus at most two for claiming,
claim upkeep, outcome recording, maintenance, and the readiness probe, so
claim upkeep never waits behind handler work. The bound is per worker
process. Size the database's own connection limit across every service
instance, every worker instance, and `migrate`.

The worker reuses `http.grace_period` and `http.drain_timeout`, validated as
the service validates them, plus its own fixed 17 s teardown tail and no
readiness propagation delay. `http.grace_period` must be at least
`http.drain_timeout` plus 17 s, or startup refuses and names both values:

```text
http.grace_period (..) must be >= http.drain_timeout (..) plus the 17s jobs worker teardown tail (release, listeners, background join, dependency close, telemetry flush)
```

The tail is release 2 s, listeners 2 s, background join 3 s, dependency
close 5 s, and telemetry flush 5 s. Defaults: 25 s + 17 s = 42 s inside
45 s, so the platform grace settings the service needs fit the worker too
([Runtime Budget Policy](configuration-source-policy.md#runtime-budget-policy)).

No other jobs key exists. Poll, claim, upkeep, retry, retention, and stage
values are constants of the pack. Attempt budget and timeout are per-kind
code ([Register kinds](#register-kinds)).

A worker beside `make run` needs its own `APP__HTTP__ADDR` and
`APP__OBSERVABILITY__METRICS__ADDR`, so it does not bind the service's
listeners, plus the PostgreSQL variables from the comment in
`env/config/local.toml` (`APP__POSTGRES__ENABLED=true` and
`APP__POSTGRES__DSN='postgres://app:app@127.0.0.1:5432/app?sslmode=disable'`),
the same ones `make run` uses. Migrate the schema first
([Migrations](../migrations/README.md)). The template source's worker
refuses because it registers no kind.

```sh
APP__HTTP__ADDR=127.0.0.1:8081 APP__OBSERVABILITY__METRICS__ADDR=127.0.0.1:9091 cargo run --locked -p jobs-worker -- --config env/config/local.toml
```

The same command is recorded in
[Build, Test, And Development Commands](build-test-and-development-commands.md).

## Run and stop the worker

Run the same image with `--entrypoint /jobs-worker` (the platform's command
override). `/service` stays the image's default entrypoint. Use the same
configuration and `APP__*` variables as the service, and
`APP__POSTGRES__ENABLED=true` with the DSN.

Startup refusals exit `1` with one sanitized line. Before any database I/O:
invalid configuration, PostgreSQL disabled, no or invalid kinds, the pool
bound, and the grace budget. After it begins: the database cannot be reached
inside the acquire budget, the jobs schema is missing, the session is
read-only or recovering, a listener cannot bind, or readiness admission
fails. Messages name the failure class, never the DSN.
[Jobs worker](architecture/runtime-lifecycle.md#jobs-worker) has the full
startup order and texts.

`http.addr` serves only `GET /health/live` (process-only, `200` while
running) and `GET /health/ready`, under the service's server bounds. There
is no API route and no OpenAPI document. `/metrics` stays on the diagnostics
listener (`observability.metrics.addr`) exactly as for the service. One
probe configuration fits both entrypoints.

The worker is ready only after startup completed and claiming began, and
then follows the cached PostgreSQL probe verdict. It is not ready at the
first stop signal. The health listener keeps answering, not ready, until
the drain ends.

Records: `jobs_worker_starting` (the non-secret facts, including
`jobs.kinds` — registered names joined by `,` — and `jobs.max_workers`, plus
`service.name`, `app.env`, `app.version`, `app.commit`, `http.addr`,
`http.drain_timeout`, `http.grace_period`, `observability.metrics.addr`,
`postgres.max_connections`, `log.level`, and `tracing.exporter`; no DSN and
no secret), `jobs_claiming_started`, and `jobs_worker_ready`.

The worker derives `{service_name}-jobs-worker` from
`observability.otel.service_name` for its records, traces, and metrics.
PostgreSQL `application_name` is the longest prefix of the service name of
at most 51 bytes that ends on a character boundary, followed by
`-jobs-worker`, so the suffix survives PostgreSQL's 63-byte limit. No key
controls it.

`SIGTERM` or `SIGINT` turns readiness off and stops claiming at once.
In-flight attempts may finish within `http.drain_timeout`. A second signal
ends that wait. The attempts still running at the end are cancelled and
released for an immediate claim, with their budget unit given back. A
release that cannot be recorded leaves the job to claim-expiry recovery with
the unit used. The listeners then close, background tasks are joined, the
pool closes, and telemetry flushes, all inside the one grace deadline
started at the first signal.

| Code | When |
| --- | --- |
| `0` | a stop signal, every stage completed inside its budget, and no attempt was cancelled at the drain's end |
| `3` | after a stop signal, an attempt was cancelled at the drain's end (including an end forced by a second signal) or a stage overran |
| `1` | a startup refusal, or the engine stopped without a stop signal |

A database outage after startup does not end the process. Claiming and
outcome writes retry every 1 s, claim upkeep every 10 s, and retention and
gauge samples on their own cadence. Readiness follows the probe. Jobs whose
outcome could not be recorded are recovered when their claim expires.

## Metrics, records, and data custody

Names are fixed. `kind` is a registered kind's name. Gauges also use
`<unregistered>` for jobs of kinds this worker does not register, so
stranded jobs are visible. Attempts exist only for registered kinds.

| Signal | Name | Labels | Meaning |
| --- | --- | --- | --- |
| Counter | `jobs_attempts_total` | `kind`, `outcome` | one increment per claimed attempt, and one for an exhaustion recorded at claim without running the handler |
| Histogram | `jobs_attempt_duration_seconds` | `kind` | handler run time, from start to completion, cancellation, or abort, of every attempt that ran its handler; a claim-time exhaustion has no duration |
| Gauge | `jobs_live_jobs` | `kind`, `state` | live jobs, sampled every 10 s. `state` is `available` (pending and due), `scheduled` (pending, not yet due), or `running` |
| Gauge | `jobs_oldest_available_age_seconds` | `kind` | age of the oldest available job; 0 when none |
| Counter | `jobs_worker_operation_failures_total` | `operation` | engine statements that failed. `operation` is `claim`, `extend`, `record`, `release`, `reconcile`, `retention`, or `sample` |

Histogram buckets, in seconds: 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1,
2.5, 5, 10, 30, 60, 120, 300, 600, 1800, 3600.

Each sample publishes every registered kind and `<unregistered>`, with zero
when that label has no jobs, so a series does not keep a stale value. With
several workers, each publishes the database-wide gauges. Aggregate them
with `max`, not `sum`.

| `outcome` | Meaning |
| --- | --- |
| `completed` | success recorded |
| `retry` | retryable error, panic, or undecodable payload recorded with budget left |
| `timeout` | timeout recorded with budget left |
| `exhausted` | any retryable failure or timeout on the last unit, and every exhaustion at claim of a job whose budget is already spent. An attempt that spends the last unit counts here, not under its cause; the cause is in the `job_failed` summary |
| `permanent` | permanent failure recorded |
| `released` | cancelled at the end of the drain and released |
| `superseded` | a newer claim holds the row, proven by an extension that did not return the claim while the handler ran, or by reconciliation after a write or release that changed nothing |
| `lost` | the attempt was cancelled at its local claim deadline (`extension`), its outcome could not be written and attributed before that deadline (`record`), or its drain-end release could not be attributed (`release`). The job waits for claim-expiry recovery unless one of its writes took effect unseen, in which case the database already holds that outcome |

An outcome is recorded only when the database proves it, and each admitted
attempt is recorded once.

| Record | Level | Fields |
| --- | --- | --- |
| `job_failed` | `warn` | `job.id`, `job.kind`, `job.attempts`, `job.failure_reason` (`permanent`, `exhausted`), `error` (the summary) |
| `job_attempt_failed` | `info` | `job.id`, `job.kind`, `job.attempt`, `failure` (`error`, `panic`, `payload`, `timeout`), `retry_in_ms`, `error` |
| `job_attempt_completed` | `debug` | `job.id`, `job.kind`, `job.attempt`, `duration_ms` |
| `job_attempt_superseded` | `info` | `job.id`, `job.kind`, `job.attempt` |
| `job_claim_lost` | `warn` | `job.id`, `job.kind`, `job.attempt`, `reason` (`extension`, `record`, `release`) |
| `jobs_operation_failed` / `jobs_operation_recovered` | `warn` / `info` | `operation`, `failure` (`acquire`, `begin`, `statement`, `commit`, `commit_unknown`, `timed_out`); the first failure after a success, and the first success after failures, per operation |

The counter counts every failed engine statement. Those two records mark the
edges, not each failure.

Each attempt runs in a `job_attempt` span at `info`, with `job.id`,
`job.kind`, `job.attempt`, and `otel.kind = "consumer"`. When the enqueue
ran inside a valid trace, the span links to that context. Enqueue stores a
W3C `traceparent` only (`00-{trace_id}-{span_id}-{flags:02x}`), and only when
the current span's context is valid. No `tracestate`, no baggage.

**Data custody.** Payloads carry identifiers, not secrets or unnecessary
personal data. Handler error messages must not quote payload values. The
stored error summary is the error's display text with control characters
(including NUL and newlines) replaced by spaces, then cut to 1024 bytes on a
character boundary. Only the latest is kept. Fixed texts replace content the
engine must not store:

| Case | Stored summary |
| --- | --- |
| Timeout | `attempt timed out after {timeout}` (`humantime` format) |
| Panic | `handler panicked` (the panic payload may quote data) |
| Undecodable payload | `payload does not decode as {kind}: {category} error at line {line} column {column}` |
| Budget spent at claim, with no earlier failure | `attempt budget spent` |

No log, metric label, span attribute, or stored summary carries the payload.
`Job`'s `Debug` prints only id, kind, and attempt. A payload lives as long
as its job.

## Retries, failures, and retention

| Attempt ends with | Job becomes | Budget unit |
| --- | --- | --- |
| handler success | `completed` | used |
| retryable error, panic, timeout, or a payload that does not decode | `pending` after backoff while budget remains; otherwise `failed` with reason `exhausted` | used |
| permanent error | `failed` with reason `permanent` | used |
| cancellation at the end of the drain | `pending`, claimable at once | given back |
| claim lost | unchanged by this attempt | kept (used when the attempt was claimed) |

A claim of a job whose budget is already spent — after a lost attempt, or
when the claiming worker registers a smaller budget — ends it `failed` with
reason `exhausted` without running the handler. That claim uses no budget
unit, records `exhausted` with no attempt duration, and writes
`job_failed`.

**Backoff.** After the n-th used unit fails, the job waits `n^4` seconds
with a ±10 % jitter that is fixed per job and attempt, on the database
clock. The 25th used unit of the default budget fails terminally and does
not wait.

| n | Wait |
| --- | --- |
| 1 | 1 s |
| 2 | 16 s |
| 3 | 81 s |
| 4 | 256 s (about 4 min) |
| 5 | 625 s (about 10 min) |
| 10 | 10 000 s (about 2.8 h) |
| 24 | 331 776 s (about 3.8 days) |

The whole default budget of 25 waits 24 times, 1 763 020 s in total, about
20.4 days. The arithmetic is in
[Async Architecture](architecture/async.md#outcomes-backoff-and-retention).

**Retention.** Completed jobs are deleted 24 h, and failed jobs 7 days,
after they became terminal, of every kind, registered or not, by every
worker, every 60 s, in 500-row batches. Nothing else deletes a job. Live
jobs are never deleted.

**Inspection.** The table is `background_jobs`. Columns an operator reads:
`id`, `kind`, `payload`, `unique_key`, `state` (`pending`, `running`,
`completed`, `failed`), `failure_reason` (`permanent`, `exhausted`),
`attempts`, `claim_generation`, `not_before`, `claim_expires_at`,
`finished_at`, `error_summary`, `trace_context`. The state machine is in
[Async Architecture](architecture/async.md#the-job-table-and-states).
Reading payloads is reading business data.

```sql
SELECT kind, state, count(*) AS jobs
FROM background_jobs
WHERE state IN ('pending', 'running')
GROUP BY kind, state;
```

```sql
SELECT id, kind, failure_reason, attempts, finished_at, error_summary
FROM background_jobs
WHERE state = 'failed'
ORDER BY finished_at DESC;
```

```sql
SELECT id,
       convert_from(payload, 'UTF8') AS payload,
       convert_from(unique_key, 'UTF8') AS unique_key
FROM background_jobs
WHERE id = '<job id>';
```

There is no operator API or UI: no pause, cancel, redrive, or delete. The
engine is the table's only writer. The pack also does not provide periodic
or cron schedules; priorities, named queues, or per-kind concurrency or
fairness; producer-side cancellation; workflows, batches, or stored job
results; per-kind backoff schedules; a built-in business job kind; or any
datastore other than PostgreSQL. It does not complete a job inside the
handler's own transaction, so a database-only effect is not exactly once.

## Roll it out

Bring the pack up in this order:

1. Run `/migrate`. It adds `background_jobs`. The migration is forward-only,
   applied only by `/migrate`, and needs no database extension. Neither the
   service nor the worker migrates at startup
   ([Migrations](architecture/persistence.md#migrations),
   [migrations README](../migrations/README.md)).
2. Deploy the service. It can enqueue. Jobs wait.
3. Deploy `/jobs-worker` with its kinds registered.

Rolling back the worker leaves jobs pending until it returns. Rolling back
the service leaves the table, because the migration is forward-only.

A job of a kind no running worker registers stays live and untouched. It is
visible as `jobs_live_jobs{kind="<unregistered>"}` and in the oldest-age
gauge, and it runs once a worker registering it runs. Workers with different
policies for one kind may run at once, and the claiming worker's policy
governs. Payload types evolve additively (new fields with serde defaults),
because an older worker that cannot decode a newer payload fails the attempt
retryably until the budget is spent.

The initializer refuses a profile migration that adds or removes the pack
later. A manual addition or removal also sets `profiles.jobs` in
`template.lock`, which the image check reads
([Template sync](template-sync.md#initialization-record-and-replay)).

## Prove it

Unit tests beside their owners run under `make test`
(`make test-package PKG=infra-jobs`, `PKG=jobs-worker`). That includes the
shipped binary's process test in `crates/jobs-worker/tests/process.rs`: with
the default configuration and no database it exits `1` with a refusal,
exactly `no job kind is registered` in the template source, so a template
change that registers a kind fails `make test`. In a derived service either
that refusal or `postgres.enabled must be true to run the jobs worker`
passes.

```bash
make test
make test-package PKG=infra-jobs
make test-package PKG=jobs-worker
```

Real PostgreSQL: `ALLOW_HEAVY=1 make test-integration-db` runs
`test/tests/jobs/` (enqueue, execution with two racing engines, the process
suite on the test-only `jobs-worker-fixture` binary, and the joint module
with HTTP idempotency where both packs are retained).
`bash scripts/ci/test-integration-db.sh --test jobs` runs that target alone.
[PostgreSQL Validation](validation/postgres.md) owns the command and its
Docker requirement.

```bash
ALLOW_HEAVY=1 make test-integration-db
bash scripts/ci/test-integration-db.sh --test jobs
```

The image check includes the `/jobs-worker` step:

```bash
ALLOW_HEAVY=1 make runtime-image-check
```

A derived service proves its own kinds' effect idempotency in its own tests.

## Mechanism and reopen conditions

The engine is template-owned because no maintained crate with a stable
release fills the four-part gap: enqueue on the caller's sqlx 0.9 connection
that reports a duplicate without aborting the transaction; a schema owned by
`migrations/` with no startup migration or extension; execution inside this
pool's budgets and TLS stack; and the execution semantics under the worker's
lifecycle. One table, 1 s polling, claims kept alive every 10 s and claimable
again 30 s after their last extension, outcomes fenced by a claim token.

Reopen when a crate on the watch list closes all four gap items (awa 0.7 on
sqlx 0.9, underway's next release on sqlx 0.9, apalis-postgres 1.0,
graphile_worker with optional startup migration and default recovery), when
measured pickup latency or claim load contradicts the 1 s poll, when a derived
service needs a non-goal, or when a contract the pack preserves and rests on
changes upstream, such as the PostgreSQL transaction seam and its
commit-outcome policy.
[Async Architecture](architecture/async.md#decisions-recorded-here) records
the decisions, the deviations from the Go template, and the full reopen list.
