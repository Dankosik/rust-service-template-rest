# Background jobs

Select `JOBS=postgres` with `DATABASE=postgres`. The default `JOBS=none`
removes both jobs migrations, `infra-jobs`, `jobs-worker`, this guide, the
async architecture leaf, jobs tests, configuration, and the worker image
entrypoint. The retained pack stays inert: service code touches the table only
when it calls `enqueue`, and an operator separately runs `/jobs-worker`.

The pack durably enqueues with a business write in PostgreSQL and runs each
committed registered job at least once. A claim is permission for one attempt,
not proof of an exactly-once effect. Handlers must key external effects on the
stable job ID or a business idempotency key.

## Define and enqueue a kind

`JobKind` is serde `Serialize`/owned `Deserialize`, `Send + Sync + 'static`,
with `const NAME: &'static str`. Put it in the infrastructure adapter that
enqueues it, not in a feature crate. Names are 1–64 lowercase ASCII characters
from letters, digits, `.`, `_`, and `-`, beginning with a letter. A static kind
can make that contract compile-time visible:

```rust,ignore
#[derive(serde::Serialize, serde::Deserialize)]
struct Welcome { widget_id: i64 }

impl infra_jobs::JobKind for Welcome {
    const NAME: &'static str = "widgets.welcome";
}

const _: () = infra_jobs::assert_valid_kind_name(<Welcome as infra_jobs::JobKind>::NAME);
```

Runtime registration and enqueue still reject invalid reachable names with a
typed error. Renaming a kind strands its live rows until a worker registers its
old name.

`infra_jobs::enqueue(tx, &payload, options)` takes the shared opaque
`&mut infra_postgres::Tx` that an `in_tx` or `in_tx_with` closure receives,
including a handler's own transaction; the jobs adapter obtains the scoped
connection internally. It opens no connection and issues no `COMMIT`,
`ROLLBACK`, or savepoint. A created row therefore commits or rolls back with
the caller's business write. `EnqueueOptions::delay` is zero by default, has a
maximum of 36,500 days, and drops sub-microseconds. `unique_key` is an optional
1–255 byte UTF-8 string with no control characters. A live unique key returns
`Enqueued::Duplicate` and leaves the transaction usable.

Before sending SQL, enqueue validates kind, key, delay, JSON size (256 KiB),
and decoded NUL. It serializes once with `serde_json` and rejects a `\u0000`
escape (a NUL character), which JSONB cannot store. Valid payload and key
bind as text with explicit JSONB/text casts, and the insert is the only
statement enqueue sends. UTF-8 is a schema precondition rather than a
per-call check: the simplification migration refuses a non-UTF-8 database,
and the worker's startup check verifies it. The typed validation failures
include `InvalidKind`, `InvalidUniqueKey`, `InvalidDelay`,
`PayloadContainsNul`, `Serialize`, and `PayloadTooLarge`. Database errors
abort the caller transaction in the ordinary PostgreSQL way.

A payload carries identifiers, not secrets or copies of business data. It is
stored as queryable JSONB, kept with terminal history (24 hours, or seven
days after a failure), and readable by anyone who can read the table.

```rust,ignore
infra_postgres::in_tx(pool, async |tx| {
    let widget_id = insert_widget(infra_postgres::connection(tx)).await?;
    let key = widget_id.to_string();
    match infra_jobs::enqueue(tx, &Welcome { widget_id }, infra_jobs::EnqueueOptions {
        unique_key: Some(&key),
        ..Default::default()
    }).await? {
        infra_jobs::Enqueued::Created(_) | infra_jobs::Enqueued::Duplicate => Ok(widget_id),
    }
}).await
```

At `READ COMMITTED`, a conflicting live unique key is `Duplicate` after any
wait for its writer. At `REPEATABLE READ` and `SERIALIZABLE`, a holder changed
after the caller snapshot can instead produce `40001`; retry the whole caller
transaction only through the existing persistence policy. A `CommitUnknown`
means the job exists exactly when the business write does; reconcile that
business operation rather than blindly rerunning it.

<!-- template:begin jobs-http-idempotency:docs-background-jobs-idempotency -->

## Enqueue from an idempotent operation

An idempotent adapter passes the shared `infra_postgres::Tx` that
`infra_http::idempotency` re-exports to `infra_jobs::enqueue(tx, ...)` inside
the store's explicit `READ COMMITTED` transaction. The business write, success
record, and job commit together or not at all. A replay, refusal, or in-progress
request enqueues nothing; a duplicate unique key remains a successful adapter
outcome.

<!-- template:end jobs-http-idempotency:docs-background-jobs-idempotency -->

## Run handlers and complete a database effect

A `Handler<K>` returns `Result<(), JobError>`. `Ok(())` requests ordinary
completion; `JobError::retryable(error)` and `JobError::permanent(error)` keep
their existing meanings. A retryable error, panic, timeout, or decode failure
uses the persisted retry policy; a permanent error becomes terminal.

`job.cancellation()` fires at the kind's timeout and when a forced drain
cancels the attempt. The handler task is also aborted, which takes effect at
its next `.await`. Work started with `tokio::task::spawn_blocking` is not
stopped, so check the token inside blocking loops and expect the job to run
again while that work may still be running.

Reach PostgreSQL through `job.pool()`, holding at most one pooled connection
at a time: the pool has one connection per attempt slot plus two for the
engine and readiness (`jobs.max_workers + 2`), so a handler that runs a pool
query inside its own open transaction takes a connection another slot or the
engine's outcome write needs. The service's session budgets apply:
`statement_timeout` and `idle_in_transaction_session_timeout` are 8 s and a
pool acquire waits 3 s. A handler that needs a longer statement raises the
limit for its own transaction only, with `SET LOCAL statement_timeout = '...'`
as that transaction's first statement.

For an effect wholly inside a supplied PostgreSQL transaction, call
`job.complete_in_tx(infra_postgres::connection(tx)).await?` inside that same transaction closure. The
method requires a tracked transaction, writes the same fenced COMPLETE
transition, and never controls the transaction. Its `CompleteError` must
propagate: a stale claim rolls back the preceding business writes.

```rust,ignore
#[derive(Debug, thiserror::Error)]
enum WelcomeError {
    #[error(transparent)]
    Complete(#[from] infra_jobs::CompleteError),
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error(transparent)]
    Tx(#[from] infra_postgres::TxError),
}

async fn welcome(job: infra_jobs::Job<Welcome>) -> Result<(), infra_jobs::JobError> {
    let result = infra_postgres::in_tx(job.pool(), async |tx| {
        write_effect(infra_postgres::connection(tx), job.payload().widget_id).await?;
        job.complete_in_tx(infra_postgres::connection(tx)).await?;
        Ok::<_, WelcomeError>(())
    }).await;

    match result {
        Err(WelcomeError::Tx(infra_postgres::TxError::CommitUnknown(error))) => {
            Err(infra_jobs::JobError::transaction_unknown(error))
        }
        Err(error) => Err(error.into()),
        Ok(()) => Ok(()),
    }
}
```

The derived error type preserves each cause and implements `Error`, so the
existing conversion to retryable `JobError` remains available. Do not swallow
`CompleteError::StaleClaim`; do not rerun a `CommitUnknown` closure. The
transaction-unknown disposition records uncertainty and causes no later
retry/fail/release transition. An external effect still needs provider or
business idempotency: this API makes only the supplied PostgreSQL effect atomic.

`JobError::retry_after(error, delay)` and `JobError::snooze(delay)` return
`Result<JobError, InvalidDelay>` and use enqueue's checked delay domain.
Retry-after spends an attempt and schedules exactly the supplied delay without
jitter. Snooze takes precedence over exhaustion, returns the job to pending at
database time, clears the claim, and refunds one attempt; a repeated fenced
transition cannot refund twice.

## Register kinds and retain terminal history

Register each handler in the worker's service-local registration function:

```rust,ignore
fn register(kinds: &mut infra_jobs::Kinds, _: &jobs_worker::Support<'_>)
    -> Result<(), jobs_worker::BuildError>
{
    kinds.register(infra_jobs::Policy::default(), welcome);
    Ok(())
}
```

Pass `Some(register)` to `jobs_worker::run` in the worker entrypoint. The
unmodified template refuses startup because it ships no business kind.
Registration rejects an empty set, duplicate/invalid names, and out-of-range
policies. Defaults are 25 attempts and a 60-second timeout; accepted ranges
are 1–25 attempts and 1 second–1 hour. The claiming worker's policy owns both.

Ordinary retry delay is `attempt^4 * (0.9 + 0.2 * draw)` seconds with the
claim's PostgreSQL random draw, rounded down to microseconds once. Exhaustion
and permanent failure are terminal. Summaries replace controls with spaces
and are limited to 1024 UTF-8 bytes; handlers must not include secrets in them.
Successful jobs remain for 24 hours and failed jobs for seven days. Retention
runs every minute in batches of 500 and never deletes live jobs. Unknown kinds
remain unclaimed; terminal retention is independent of registered kinds.

## Configure and size the worker

`jobs.max_workers` is the maximum concurrent attempts per worker process; it
defaults to 1 and ranges from 1 to 500. The worker requires
`postgres.max_connections >= jobs.max_workers + 2`. The two additional
connections cover engine statements and readiness; there is no upkeep
connection. `http.grace_period` must cover `http.drain_timeout` plus the fixed
17-second cleanup, listener, join, pool-close, and telemetry tail.

## Run and stop the worker

Run the image with `--entrypoint /jobs-worker`, using the same PostgreSQL and
configuration inputs as the service. The worker serves only health and metrics
listeners, never an application API. It is ready after startup and claiming
begin, and becomes unready as soon as its first stop signal arrives.

## Claims, deadlines, and shutdown

The policy registered for a kind supplies its timeout and attempt cap. A claim
lease is fixed at `statement_timestamp() + timeout + 60 seconds`; the local
deadline uses the same whole-microsecond timeout and is two seconds earlier.
There are no renewal, heartbeat, upkeep, or claim-attribution reads. A failed
or unknown claim acknowledgement never dispatches the returned row, and any
committed row recovers when its original lease expires. Claims lock rows while
they scan with `SKIP LOCKED`, so concurrent workers take disjoint jobs and a
row another session holds is skipped rather than stalling the claim.

One supervisor owns each admitted claim, slot, handler join, deadline, and
intended queue transition through cleanup. It records a known handler result
once and gives that result precedence over force or timeout. If an abort was
requested but the handler joins successfully, that known result still wins.
Once known, it is persisted and never replaced with release. Failed or unknown
outcome acknowledgements retry the identical transition at one-second intervals
within the local/cleanup deadline; a zero-row acknowledgement is merely
unchanged, never durable attribution. At the deadline, leave recovery to lease
expiry.

On the first signal the worker disables readiness and stops claiming, then
drains. A forced drain calls `Started::cancel_and_finish` for the same two
second cleanup stage. A handler it cancels is released: the attempt is
refunded and `not_before` is unchanged, so the job is due at once and keeps
its place in claim order. `attempts_finished` reports known local results,
cancelled handlers, acknowledged releases, and uncertainty without claiming
that a zero-row write released a job. The tail remains listeners 2 s,
background join 3 s, pool close 5 s, and telemetry flush 5 s. A successful
completion races safely with forced cleanup because both operations are fenced
on the same row.

## Storage, observation, and inspection

The forward simplification migration converts `payload` from `bytea` to
`jsonb`, `unique_key` to `text COLLATE "C"`, adds `trace_state text`, and
replaces the running index with `(kind, claim_expires_at, not_before, id)` for
running rows. It preserves job identity, state, generation, attempts, times,
and terminal history. JSONB's semantic normalization is intentional; legacy
payloads or database encodings that cannot convert are refused.

New trace data stores bounded ASCII `trace_context` and `trace_state` only.
The worker extracts through the installed propagator and creates a span link,
never a remote parent. Empty trace-state is absent; malformed, overbound, or
control-bearing context produces an unlinked attempt. No baggage is stored.

Each worker exposes these counters and the histogram on its `/metrics`
listener:

| Instrument | Labels | Meaning |
| --- | --- | --- |
| `jobs_attempts_total` | `kind`, `outcome` | Handler results as the worker observed them: `completed`, `retry`, `timeout`, `exhausted`, `permanent`, `snoozed`, `cancelled`, `transaction_unknown`. |
| `jobs_persistence_total` | `kind`, `disposition` | Outcome writes: `applied`; `unchanged`, a zero-row write, which is normal after `complete_in_tx` and otherwise means a newer claim owns the row; `unknown`, left to lease expiry and logged at `warn`. |
| `jobs_attempt_duration_seconds` | `kind` | Handler run time. |
| `jobs_worker_operation_failures_total` | `operation` | Failed `claim`, `record`, `release`, `retention`, or `sample` statements. |

Records never carry the payload: `job_failed` (`warn`), `job_attempt_failed`
(`info`), `job_attempt_finished` (`info`, snooze and cancellation),
`job_transaction_unknown` (`warn`), `job_attempt_completed` and
`job_persistence_finished` (`debug`, or `warn` when unknown), and
`jobs_operation_failed` / `jobs_operation_recovered` on the first failure
and the first recovery of each operation.

Every worker samples only registered kinds every ten seconds. For each kind and
`available`, `scheduled`, or `running` state, it counts at most 1001 indexed
rows, publishes a value capped at 1000 and a censoring gauge, and publishes the
constant cap in `jobs_live_jobs_sample_cap`. Oldest available age uses an
independent indexed due-row lookup. These samples are per-process and must not
be summed across replicas; unknown kinds are not aggregated. Live jobs of a
kind no worker registers, for example after a rename, appear only in SQL:

```sql
SELECT kind, state, count(*) FROM background_jobs
WHERE state IN ('pending', 'running')
  AND kind <> ALL (ARRAY['widgets.welcome'])  -- the registered kinds
GROUP BY kind, state;
```

Before a first successful sample, backlog, age, and censoring are NaN,
timestamp is 0, and success is 0. A successful sample publishes its database
timestamp. A failure returns the values to NaN and success to 0 but retains the
last-success timestamp. Alerting requires success=1, a nonzero timestamp, and
freshness no older than 30 seconds. This distinguishes an exact empty queue,
censoring, sample failure, and a stopped sampler. The two-second sample timeout
is an elapsed-time limit, not a scan-size claim.

## Roll out the conversion

Before the maintenance window, use an approved database session with a finite
statement timeout to force conversion of every old row without exposing data:

```sql
SHOW server_encoding;
SELECT count(convert_from(payload, 'UTF8')::jsonb) AS payloads,
       count(convert_from(unique_key, 'UTF8')) AS keys
FROM background_jobs;
```

A conversion error is a refusal; repairing or deleting data needs a separate
decision. The migration repeats conversion under its table lock, so this
preflight cannot authorize overlapping old producers.

This is a stopped-producer/worker conversion. Inventory and stop every old
producer and worker before `/migrate` applies
`20260925000001_simplify_background_jobs.sql`; old bytea binaries cannot run
against new JSONB/text columns. The migrator applies the forward file atomically
after the operator's read-only preflight. A known failure rolls back the file and its
history row. For an unknown migrator outcome, inspect migration history and
schema before selecting a binary. After a successful conversion, old binaries
and a down migration are not recovery; use a compatible forward repair.

The pack still has no operator pause, cancel, redrive, priority, queue,
workflow, or generic business-closure replay API. Future stages 10.5 and 10.6
reuse its scheduling, attempt, and completion mechanics. A lifecycle-crate
extraction remains deliberately deferred under the condition in
[Async Architecture](architecture/async.md#ownership-and-retained-decisions).
