# Background jobs

Select `JOBS=postgres` with `DATABASE=postgres`. The default `JOBS=none`
removes the jobs migrations, `infra-jobs`, the operator commands, this guide, the
async architecture leaf, jobs tests, configuration, and the worker image
entrypoint unless messaging retains the worker. A messaging-only worker keeps
the loader CLI but has no jobs operator. The retained pack stays inert: service
code touches the table only when it calls `enqueue`, and an operator separately runs `/jobs-worker`.

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
old name; a rename is not a queue migration. Across rolling deployments and
backup restore, retain handlers that understand every outstanding kind and
payload version. Inspect stranded or failed identities and restore compatible
code, or perform a separately reviewed data conversion, before recovery.
Redrive resets attempts; it does not repair a poison payload, rename a kind,
or reinterpret stored intent. Malformed or incompatible payloads keep their
existing retry/exhaustion classification and sanitized failure behavior.

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
bind as text with explicit JSONB/text casts. Enqueue sends the insert and,
for a job due at once, at most one [wake notification](#claims-deadlines-and-shutdown)
per kind every 25 ms. UTF-8 is a schema precondition rather than a
per-call check: the canonical migration requires a UTF-8 database,
and the worker's startup check verifies it. The typed validation failures
include `InvalidKind`, `InvalidUniqueKey`, `InvalidDelay`,
`PayloadContainsNul`, `Serialize`, and `PayloadTooLarge`. Database errors
abort the caller transaction in the ordinary PostgreSQL way.

Enqueue and `compare_live_payload` share the same preparation path. It retains
at most 262,144 serialized bytes, grows its buffer only as needed up to that
ceiling, and discards excess output while counting it. Serialization still runs
once to completion: a late serializer error returns `Serialize`; a successful
oversized serialization returns `PayloadTooLarge { bytes }` with the exact
encoded length before the decoded-NUL check. Accepted JSON bytes are unchanged.
The retained-byte ceiling excludes allocator rounding, caller-owned payloads,
and allocations or CPU spent inside a custom serializer. It also does not bound
decoded handler payloads or caller-owned results; features own their sizes and
concurrent lifetimes. Preparation refusal happens before database effects.

Custom serializers must be finite, trusted and nonblocking. Callers admit
finite source bytes, collection cardinality and concurrent calls within their
transaction deadline before entering preparation. A deadline cannot interrupt
a synchronous callback that never returns. Known webhook and outbox producers
also inspect borrowed metadata cooperatively before cloning or base64 encoding;
their preflights do not change this generic serialization contract.

A payload carries identifiers, not secrets or copies of business data. It is
stored as queryable JSONB and readable by anyone who can read the table.
Completed jobs remain for 24 hours; failed jobs remain until explicit recovery
or discard. Size and protect storage for unresolved failures and their history.

```rust,ignore
infra_postgres::in_tx(pool, async |tx| {
    let widget_id = insert_widget(tx).await?;
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
their existing meanings and store `error`'s `Display` text. An error
propagated with `?` is retryable and its summary also carries each `source()`
whose text the summary does not already contain, so a cause kept only in
`#[source]` is not lost and a cause the error prints is not repeated. A retryable error, panic, timeout, or decode failure
uses the persisted retry policy; a permanent error becomes terminal.

`job.cancellation()` fires at the kind's timeout and when a forced drain
cancels the attempt. The handler then has up to 100 ms to return before its
future is dropped once control returns to the executor; an immediately ready
`.await` alone does not guarantee that return. Returning `Ok(())`
in that window completes the job; any other return counts as the cancellation
itself, so a forced drain still releases the job and refunds the attempt.
Work already started with `tokio::task::spawn_blocking` is not stopped.
Follow [business-work admission and lifetime](architecture/runtime-lifecycle.md#business-work-admission-and-lifetime):
admit before submission, retain capacity and completion/panic observation until
actual execution ends, and check the token inside blocking loops. Expect the
job to run again while that work may still be running; the effect and fencing
rules below still apply, and cancellation is no proof that an effect ceased.

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
`job.complete_in_tx(tx).await?` inside that same transaction closure. The
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
        write_effect(tx, job.payload().widget_id).await?;
        job.complete_in_tx(tx).await?;
        Ok::<_, WelcomeError>(())
    }).await;

    match result {
        Err(WelcomeError::Tx(infra_postgres::TxError::CommitUnknown(error))) => {
            Err(infra_jobs::JobError::retryable(error))
        }
        Err(error) => Err(error.into()),
        Ok(()) => Ok(()),
    }
}
```

The derived error type preserves each cause and implements `Error`, so the
existing conversion to retryable `JobError` remains available. Do not swallow
`CompleteError::StaleClaim` or rerun a `CommitUnknown` closure. Map the latter
to ordinary retryable failure: its fenced outcome observes a committed COMPLETE
as unchanged, can retry after rollback, and otherwise leaves ownership to lease
expiry. An external effect still needs provider or business idempotency: this
API makes only the supplied PostgreSQL effect atomic.

`JobError::retry_after_at_least(error, delay)` and `JobError::snooze(delay)`
return `Result<JobError, InvalidDelay>` and use enqueue's checked delay domain.
`retry_after_at_least` spends an attempt and is a floor under the normal
jittered backoff. Snooze takes precedence over exhaustion, returns the job to
pending at database time, clears the claim, and refunds one attempt; a repeated
fenced transition cannot refund twice.

## Register kinds and retain terminal history

Register each handler in the worker's service-local registration function:

```rust,ignore
fn register(registration: &mut jobs_worker::Registration<'_>)
    -> Result<(), jobs_worker::BuildError>
{
    registration.jobs.register(infra_jobs::Policy::default(), welcome);
    Ok(())
}
```

Pass `register`, or a closure that captures what its handlers need, to
`jobs_worker::run` in the worker entrypoint. `Registration` also gives the
loaded configuration, a shutdown token, and `spawn(name, |cancel| task)` for a
background task the worker owns: the task runs until `cancel` fires, the
background-join stage joins it, and one that returns or panics earlier stops
the worker with exit code 1 under that name. The
unmodified template refuses startup because it ships no business kind.
Registration rejects an empty set, duplicate/invalid names, and out-of-range
policies. Defaults are 25 attempts and a 60-second timeout; accepted ranges
are 1–25 attempts and 1 second–1 hour. The claiming worker's policy owns both.

`Policy::max_running` bounds how many attempts of one kind an engine runs at
once; the default `None` leaves `jobs.max_workers` as the only bound. The
engine asks each claim for no more of the kind than the free part of the
bound, counting due jobs and expired leases together, so the rest of the
kind's due work stays `pending` in the queue: it spends no attempt, holds no
slot, and the engine's other slots stay free for its other kinds. A finished
attempt of a full kind wakes the claim loop, so the next due job starts
without waiting for the poll. The bound is per engine and so per worker
process: `N` processes run up to `N` times as many. It does not order the
kind's jobs and does not partition them by payload.

PostgreSQL computes `attempt^4 * (0.9 + 0.2 * random())` seconds (floored by
`retry_after_at_least`) when it writes the retry; a re-sent fenced write may
redraw. With immediate failures and no extra floor, queueing, downtime, or
handler cost, the nominal delays before the final attempt sum to
`sum(a^4, a=1..24) = 1,763,020 seconds` (about 20.4 days) for 25 attempts.
For 20 attempts, `sum(a^4, a=1..19) = 562,666 seconds` (about 6.51 days).
Independent jitter gives a +/-10% jitter-only range. These are policy arithmetic,
not a delivery bound: handler time, retry floors, outage, backpressure, and
scheduling can lengthen the horizon.

Exhaustion and permanent failure are terminal. Summaries replace
controls with spaces and are limited to 1024 UTF-8 bytes; handlers must not
include secrets in them. `error_summary` holds the most recent summary. The
`errors` JSONB array keeps the history: every failed attempt (retry,
exhaustion, permanent failure, timeout, panic, undecodable payload) and every
lease-expiry rescue appends one `{"attempt", "at", "error"}` entry. Snooze
and forced-drain release append nothing, so a job has at most one entry per
spent attempt, 25 at the largest policy.
Successful jobs remain for 24 hours. Failed jobs remain until explicit redrive
or discard; they are neither claimed nor owners of live unique keys. Retention
runs every minute in batches of 500, deletes only completed jobs, and is
independent of registered kinds. Unknown kinds remain unclaimed. Each redrive
archives the previous cycle in `recovery_history` and starts a fresh attempt
budget; archive growth has no automatic cap or truncation. Completed retention
or explicit discard deletes the entire row, including its archive.

## Configure and size the worker

`jobs.max_workers` is the maximum concurrent attempts per worker process; it
defaults to 1 and ranges from 1 to 500. The worker requires
`postgres.max_connections >= jobs.max_workers + 2`. The two additional
connections cover engine statements and readiness; there is no upkeep
connection. The worker process also keeps one `LISTEN` connection outside
the pool, so the database sees one more session per worker; the outbox
publisher engine shares it. `LISTEN` needs a session of its own: through a
transaction-mode pooler such as PgBouncer the statement succeeds but no
notification arrives, nothing is reported as failed, and pickup falls back to
the one-second poll. Give the worker a direct or session-mode connection when
pickup latency matters. The listener opens every connection with the pool's
current connect options, so a password rotated through
`postgres.password_file` reaches it too. `http.grace_period` must cover `http.drain_timeout` plus the fixed
17-second cleanup, listener, join, pool-close, and telemetry tail.

## Run and stop the worker

Run the image with `--entrypoint /jobs-worker`, using the same PostgreSQL and
configuration inputs as the service. The worker serves only health and metrics
listeners, never an application API. It reads the service's keys for them:
`http.addr` is its health listener, and `http.drain_timeout` and
`http.grace_period` are its shutdown budgets. A worker that shares a host or
network namespace with the service therefore needs its own `APP__HTTP__ADDR`,
and its own `APP__OBSERVABILITY__METRICS__ADDR` when metrics are served. The
metrics listener also serves `GET /health/live`, as the service's does, so the
platform's liveness probe targets the same port in both processes;
`/health/ready` stays on `http.addr`. It is
ready after startup and claiming begin, and becomes unready as soon as its
first stop signal arrives.

The worker stops itself, with exit code 1 after the staged shutdown, when
something it needs ends without a stop signal: an engine's claim loop,
retention, listener, or sampling task (`jobs_engine_task_stopped` names it),
the messaging consumer, or one of its own background tasks
(`background_task_stopped` names it: metrics upkeep, runtime metrics, pool
metrics, password refresh, the readiness refresher, or a task a registration
spawned). Each record carries `panicked`. A panic anywhere in the process is
logged once as `panicked` with its file, line, and column and never its
message, which can carry the payload a handler was processing; a handler's
panic is still only a retried attempt.

## Claims, deadlines, and shutdown

The policy registered for a kind supplies its timeout and attempt cap. A claim
lease is fixed at `statement_timestamp() + timeout + 60 seconds`; the local
deadline uses the same whole-microsecond timeout and is two seconds earlier.
There are no renewal, heartbeat, upkeep, or claim-attribution reads. A failed
or unavailable claim acknowledgement never dispatches the returned row, and
any committed row recovers when its original lease expires. A stop never
cancels an already-invoked claim: it may settle within its existing backstop
and an acknowledged, locally-valid row still transfers to its supervisor. No
new claim round begins after stop. Claims lock rows while they scan with
`SKIP LOCKED`, so concurrent workers take disjoint jobs and a row another
session holds is skipped rather than stalling the claim.

Enqueue of a job due at once sends `NOTIFY background_jobs` with the kind
name, at most once per 25 ms for each kind in a process, and it takes effect
when the caller commits. A worker with that kind registered claims at once instead of
at its next one-second poll. After a claim that found work but did not fill
every free slot, the next claim starts 25 ms after the previous one; after a
claim that filled every slot, at once; after an empty claim, at the next
notification or poll. A lost notification or listener connection delays a
job only until the next poll. So does a skipped one: an enqueue inside the
25 ms window relies on the worker the earlier notification woke, and waits
for the poll when it commits after that worker has gone idle again, or when
the notifying transaction rolled back. The listener subscribes at most once
per poll interval, so a connection that served longer than that is replaced
at once and one the server keeps closing costs one connection a second. Each
lost connection counts as a `listen` failure.

One supervisor owns each admitted claim, slot, handler, deadline, and
intended queue transition through cleanup. The handler runs on the
supervisor's task. On timeout or forced drain it first cancels the handler,
gives it up to 100 ms of cooperative completion inside the existing deadline,
and drops only a still-running handler. Global and per-kind capacity remains
held through the outcome write, retry waits, and queued or in-flight completion
bookkeeping. Capacity returns only after that responsibility ends or its
immutable deadline expires and all retained bookkeeping is dropped. Saturated
bookkeeping therefore leaves further due jobs unclaimed.
Completions queued while an earlier completion write is in flight share the
next write. It records a known
handler result once and gives that result precedence over force or timeout.
After the cancellation only success is known: an error, snooze, or panic that
answers it is recorded as the timeout or, in a forced drain, as a release.
Once known, a result is persisted and never replaced with release. Failed
or unavailable outcome acknowledgements retry the fenced write at
one-second intervals within the local/cleanup deadline; a zero-row
acknowledgement is merely unchanged, never durable attribution. At the
deadline, leave recovery to lease expiry.

On the first signal the worker disables readiness and stops claiming, then
drains. A forced drain calls `Started::cancel_and_finish` for the same two
second cleanup stage. A handler it cancels is released: the attempt is
refunded and `not_before` is unchanged, so the job is due at once and keeps
its place in claim order. `attempts_finished` reports known local results,
cancelled handlers, acknowledged releases, and uncertainty without claiming
that a zero-row write released a job. After attempt cleanup, the tail keeps
listeners 2 s, background join 3 s, dependency close 5 s, and shared trace/logger
cleanup 5 s. Including the 2 s attempt cleanup, 0.5 s SDK join slack and 1 s
runtime reserve, the complete tail is 18.5 s inside the original process
deadline. Forced background completion shares the dependency-close allocation;
unconfirmed work remains degraded. A successful completion races safely with
forced cleanup because both operations are fenced
on the same row.

## Storage, observation, and inspection

The canonical migration stores `payload` as `jsonb`, `unique_key` as
`text COLLATE "C"`, `created_at`, nullable UUID `attempted_by`, the `errors` history, and
`trace_state text`; it uses the running index `(kind, claim_expires_at,
not_before, id)`. JSONB's semantic normalization is intentional. `created_at`
is the enqueue database time. Enqueue generates `id` as a UUIDv7, so the
primary key grows in insertion order (its first 48 bits are the enqueue
time in milliseconds); the column has no default because enqueue is the only
insert path. `attempted_by` is the random UUID of the worker process that
most recently claimed the row; the engines of one process share it. An expired running row
gets a bounded payload-free rescue marker in `error_summary` and in `errors`;
a normal later outcome may replace the summary, never the history entry. Pending rows are never marked as rescued.

New trace data stores bounded ASCII `trace_context` and `trace_state` only.
The worker extracts through the installed propagator and creates a span link,
never a remote parent. Empty trace-state is absent; malformed, overbound, or
control-bearing context produces an unlinked attempt. No baggage is stored.
Each attempt runs in one consumer span, exported as `process <kind>`, with
`job.id`, `job.kind`, `job.attempt`, and the `outcome` that
`jobs_attempts_total` counts. `retry`, `timeout`, `exhausted`, and `permanent`
mark the span as an error; `snoozed` and `cancelled` do not. An attempt whose
result never became known leaves `outcome` unset.

Each worker exposes these counters and the histogram on its `/metrics`
listener:

| Instrument | Labels | Meaning |
| --- | --- | --- |
| `jobs_attempts_total` | `kind`, `outcome` | Handler results as the worker observed them: `completed`, `retry`, `timeout`, `exhausted`, `permanent`, `snoozed`, `cancelled`. |
| `jobs_persistence_total` | `kind`, `disposition` | Outcome writes: `applied`; `unchanged`, an acknowledged zero-row write that makes no ownership attribution; `unknown`, left to lease expiry and logged at `warn`. |
| `jobs_attempt_duration_seconds` | `kind` | Handler run time. |
| `jobs_claim_duration_seconds` | none | Claim request duration through acknowledgement or failure. |
| `jobs_queue_wait_seconds` | `kind` | Claimed-row database time minus its current `not_before`, floored at zero. |
| `jobs_worker_operation_failures_total` | `operation` | Failed `claim`, `record`, `release`, `retention`, or `sample` statements, and failed or lost `listen` connections. |

Records never carry the payload: `job_failed` (`warn`), `job_attempt_failed`
(`info`), `job_attempt_finished` (`info`, snooze and cancellation),
`job_attempt_completed` and `job_persistence_finished` (`debug`, or `warn`
when unknown), and
`jobs_operation_failed` / `jobs_operation_recovered` on the first failure
and the first recovery of each operation. `jobs_operation_failed` carries
`sqlstate` or `cause`. A startup check that could not read the session logs
`jobs_startup_check_failed` (`warn`) with `sqlstate` and `cause`, `timeout`
for the five-second bound, before startup fails. A handler panic logs
`background task panicked` (`error`) with `panic.file`, `panic.line`, and
`panic.column` inside the attempt's span; the worker replaces Rust's default
panic hook in every profile, so the panic text, which can quote the payload,
is never printed.

One process-owned sampler runs every ten seconds over the union of registered
ordinary and publisher kinds. Each kind's `available`, `scheduled`, `running`,
and `failed` count is capped at 1000; 1000 means at least that many rows.
`jobs_failed_jobs{kind}` reports failed rows separately from
`jobs_live_jobs{kind,state}`. Oldest available age uses an independent indexed
due-row lookup. Samples are per-process and must not be summed across replicas.
Unknown kinds require explicit operator inspection below, not metric labels.

`jobs_oldest_available_age_seconds` and
`jobs_observation_timestamp_seconds` share the same complete union sample.
Startup publishes zero for every registered value and timestamp. Only a
complete decoded sample publishes all values, then its database timestamp.
SQL or decode failure retains all last-good values and the timestamp. Alerting
requires a nonzero timestamp no older than 30 seconds. A successful empty queue
reports zero values with a nonzero timestamp. The two-second sample timeout
bounds elapsed time, not physical pages or dead tuples scanned.

## Inspect and recover retained jobs

The same executable supplies PostgreSQL-only commands. Loader flags precede
the command; omitting it runs the normal worker. Operator commands load only
the typed PostgreSQL configuration, use one pooled connection, and admit migration
history and UTF-8/writable/READ COMMITTED session settings against the canonical
writable queue, including for inspection. Read operations then use read-only
transactions. They start no handler registry, broker,
listener, telemetry exporter, claim loop, or maintenance task. The common
loader still refuses secret-like values anywhere in TOML; unrelated provider
sections otherwise need not be valid. Password files are read once. The
operator requires `postgres.enabled`; ordinary worker capacity and HTTP
budgets do not apply.

```text
jobs-worker [--config PATH] [--config-overlay PATH] [--secrets-dir PATH] inspect ID
jobs-worker [loader flags] failed [--after CURSOR] [--limit 100]
jobs-worker [loader flags] unhandled --handled-kinds LIST [--after CURSOR] [--limit 100]
jobs-worker [loader flags] redrive ID --kind KIND --version VERSION
```

Each executed command emits one JSON document with `schema_version: 1`,
`action`, and `outcome`; help and pre-execution usage errors keep their CLI
presentation. Inspection includes database `observed_at` and safe snapshots: id,
kind, state, lossless decimal-string version, attempts, failure reason,
created/scheduled/claim-expiry/finished times, and recovery count. It never
returns payloads, unique keys, trace carriers, error summaries, error arrays,
or archived bodies. Timestamps use UTC RFC3339 with six fractional digits,
nullable where absent. `inspect` reports `found` or `missing`.

`failed` includes every failed kind. `unhandled` requires the explicit
comma-separated union of kinds handled anywhere in the intended fleet, with
no whitespace normalization. For example, if the fleet handles ordinary
`widgets.welcome` and outbox publication, pass
`--handled-kinds widgets.welcome,publish_domain_event`; include any retained
webhook kinds too. An explicit `--handled-kinds ''` means none. Omission is a
usage error. The request accepts at most 1024 input names and 66,559 bytes.
Every unhandled response after argument admission, including failure results,
echoes the validated, sorted, deduplicated `handled_kinds` array; the explicit
empty set returns `[]`. A different handled set changes the question: restart
traversal without a cursor after changing it.

Each page scans at most `limit` primary-key-ordered rows (1–500, default 100),
then filters them. JSON includes `scanned`, `items`, `complete`, and
`next_cursor`. A cursor is `v1:<canonical UUID>` for the last scanned row,
including nonmatches. Follow it even when `items` is empty; a full final page
may need an extra empty page. A timeout/error is unavailable, never an empty
successful page. Traversal is complete over unchanged data; concurrent changes
behind the cursor require a later traversal. No fixed physical-I/O bound is
claimed.

Before redrive, reconcile any possible prior effect, repair its cause, restore
a compatible handler, and inspect the exact identity. Supply its id, kind,
and version unchanged. Recovery locks the failed row, fences on all three
values, archives the previous attempt cycle, and resets attempts to zero on
the same row. The new version comes from the non-reusable claim-generation
sequence. Job id, payload, unique key, creation time, and traces stay unchanged.
The next claim spends attempt 1 under the current policy; normal polling picks
it up. A competing live unique key causes `conflict` and leaves the failed row
and history intact. Recovery performs no business-closure replay and gives no
exactly-once external effect guarantee. Failed custody and permitted manual
replay have no automatic expiry, so any finite consumer deduplication TTL can
expire before a permitted replay. Retain durable logical-ID effect identity
for the full permitted replay lifetime, or reconcile effects and explicitly
constrain replay before expiring that identity. A redrive receipt or unchanged
completion write does not establish whether an external effect happened.

**Discard permanently abandons unresolved work.** It deletes the inspected
failed row and all its history. Use it only after deciding that this exact work
must never be retried; absence later cannot identify who deleted it.

```text
jobs-worker [loader flags] discard ID --kind KIND --version VERSION
```

Mutation receipts contain `action`, `id`, `kind`, `expected_version`, and
`outcome`; only acknowledged `redriven` adds `new_version`. Outcomes are
`redriven`, `discarded`, `missing`, `stale`, `conflict`, `failed`, and `unknown`.
Only an acknowledged commit establishes success. A stale state/kind/version
or absent row is not success. An uncertain commit, signal, or timeout after
mutation invocation returns `unknown`; inspect the same id before any deliberate
retry. A new state/version may show recovery, the old failed version may still
be eligible, and absence establishes only absence. Never retry automatically.
Database diagnostics carry bounded SQLSTATE/cause, never raw database details.

Success exits 0, unsuccessful operations exit 1, and CLI usage exits 2.
`cleanup="incomplete"` preserves an acknowledged outcome but exits 1; missing
stdout/receipt cannot undo a commit and requires inspection. After argument,
configuration, and file admission, network work and teardown have a 33-second
ceiling: connect 5s, history 5s, session check 5s, operation 12s, pool close 5s,
and runtime close 1s. Reads and mutations set a transaction-local two-second
statement limit; mutations set it before the initial row lock. The 12-second
operation backstop includes acquire, begin, and commit; ordinary pooled session
budgets are unchanged after the transaction. This does not bound filesystem
latency.

## Upgrade and custody

Deploy compatible handlers before enqueuing a new kind or payload version. Every
old worker that can claim that work must understand it, or the service must
exclude delivery through its accepted routing/filtering or queue separation.
Keep kind identities and payload meanings stable for outstanding and restorable
work; retire handlers only after the service's replay/restore window closes.
The [Production Contract](production-contract.md#operation-and-recovery) owns
cross-store fencing and reconciliation. These compatibility checks do not replace
the retained-failure custody gate below.

Apply both additive migrations before starting corrected workers. Old binaries
admit newer successful history, but can still delete failures older than seven
days. The retained-failure guarantee and supported recovery activation require
**every old retention owner for the database stopped or replaced**, including
custom `Engine::new` users. A rolling overlap does not establish that gate;
no migration restores already deleted rows. New binaries refuse missing or
mismatched history and never repair schema during startup.

Keep the additive schema and roll forward to a corrected worker. An old-binary
rollback restores failed deletion, early capacity release, and the observation
race. Stop claims/retention while repairing if custody must remain assured.
Never reset the claim-generation sequence or clear history; restore the sequence
above every persisted version so stale recovery tokens cannot become valid.
That fence does not survive restoring an earlier database history. Restore the
queue, history, and sequence consistently; invalidate saved pre-restore tokens,
commands, and receipts. Restore compatible handlers for outstanding kinds and
payload versions, reconcile already-applied effects, then re-inspect individual
identities before recovery. Never reuse a saved command as restore authority.

Static leases still delay uncertain/crashed-attempt recovery until expiry;
revisit them only for changed availability needs or measured unacceptable rescue
latency. The outbox retains one publisher slot and the combined worker failure
domain: NATS startup refusal can prevent ordinary jobs from starting, and a
critical engine/consumer failure stops the process. Independent throughput or
availability requirements require a separate design.

<!-- template:begin webhooks-common:docs-background-jobs-webhooks -->
## Webhook kinds

The optional provider uses the retained jobs worker and adds no worker binary,
queue, or scheduling loop. A handler receives the dispatch deadline and must
include all of its work inside that budget. `complete_in_tx(&mut Tx)` keeps
fenced completion in the consumer's database transaction. An uncertain
transaction commit is ordinary retryable failure; no handler replays its
business closure or writes a competing transition.
<!-- template:end webhooks-common:docs-background-jobs-webhooks -->

<!-- template:begin webhooks:docs-background-jobs-webhooks-outbound -->
`webhooks.deliver` uses 20 attempts and a 30-second timeout. Its valid
`Retry-After` value is only a capped floor beneath jobs-owned retry scheduling.
A missing configured endpoint retries, consumes attempts and eventually
exhausts. `webhooks.max_concurrent_deliveries` is the kind's `max_running`:
deliveries above it wait in the queue instead of holding worker slots.
Completed 2xx succeeds; 410 terminates with `endpoint_gone` and operator
guidance; all other HTTP responses retry.
<!-- template:end webhooks:docs-background-jobs-webhooks-outbound -->

<!-- template:begin inbound-webhooks:docs-background-jobs-webhooks-inbound -->
`webhooks.process` uses the existing 25-attempt, 60-second policy. A missing
consumer binding in historical work retries, consumes attempts and eventually
exhausts. The worker's `register` owns the consumer registry and rejects a
configured inbound endpoint without a consumer before claiming. The registry is
empty until an adopter binds its real consumer.
<!-- template:end inbound-webhooks:docs-background-jobs-webhooks-inbound -->

The pack still has no operator pause, cancel, bulk/force recovery, priority, queue,
workflow, or generic business-closure replay API. The webhook provider and
the messaging outbox, where retained, reuse its scheduling, attempt, and
completion mechanics. A lifecycle-crate extraction remains deliberately deferred under the condition in
[Async Architecture](architecture/async.md#ownership-and-retained-decisions).

<!-- template:begin source-template:docs-jobs-consumer-lifecycle -->
The source template provides a finite synthetic [native recovery rehearsal](consumer-lifecycle-rehearsal.md)
with historical actors, native archives and per-identity reconciliation.
<!-- template:end source-template:docs-jobs-consumer-lifecycle -->
