# Async Architecture

Load for a change to the jobs pack: its PostgreSQL representation, enqueue,
claiming, outcome transitions, observation, or worker integration. The adopter
contract is the [Background jobs](../background-jobs.md) guide.

## Ownership and retained decisions

| Owner | Responsibility |
| --- | --- |
| `infra-jobs` | All `background_jobs` statements, enqueue and kind contracts, claim and attempt supervision, maintenance, and the `trace_context` carrier. |
| `jobs-worker` | Worker composition, registration, startup and process shutdown. |
| `service-config` | `jobs.max_workers`; it does not own queue policy. |
| `migrations/` | The original table migration and `20260925000001_simplify_background_jobs.sql`; neither service nor worker changes schema at runtime. |
| `crates/infra-<provider>` | Concrete kinds, handlers, and producer calls. Features never depend on `infra-jobs`. |

The pack keeps PostgreSQL, `sqlx` 0.9, Tokio/TaskTracker, `serde_json`, and
the installed OpenTelemetry propagator. It adds no queue framework, lifecycle
crate, random-number dependency, or second observer process. The former lease
upkeep, outcome-attribution readback, and central attempt registry are removed.

Signals, deadline clamping, and task teardown remain separate service and
worker lifecycle owners. Extract a shared lifecycle crate only when a shared
change to signal semantics, deadline arithmetic, or tracked-task teardown
offsets the public API and profile cost; a third binary alone is insufficient.
Stages 10.5 and 10.6 reuse this pack's scheduling, attempts, and fenced
completion rather than creating competing queue machinery.

## Storage and enqueue

`background_jobs` retains its identity, state, attempt count, generation,
timing, terminal history, and live unique-key constraint. The simplification
migration requires a UTF-8 server, changes `payload` to `jsonb`, changes
`unique_key` to `text COLLATE "C"`, adds nullable `trace_state`, and replaces
the running index with `(kind, claim_expires_at, not_before, id) WHERE state =
'running'`. JSON values are the payload contract: PostgreSQL may normalize
formatting, duplicate keys, and numeric spelling. `C` collation preserves
exact UTF-8 key equality independent of the database default.

Before database access, enqueue validates kind, key, delay, serialized size,
and decoded NUL. It serializes once with `serde_json`; the validation detects
unescaped `\\u0000` without a lossy value round trip. Valid JSON and keys bind
as text with explicit casts, and the insert is enqueue's only statement.
UTF-8 is a schema precondition, not a per-call query: a database's
`server_encoding` is fixed at creation, the simplification migration refuses
a non-UTF-8 database, and worker startup verifies UTF-8, both converted column
types, and `trace_state`. The migration repeats UTF-8 validation, so an
incompatible row is refused, never silently sanitized.

`enqueue` remains the only insert and uses the caller's open
`&mut PgConnection` without transaction-control SQL or another connection.
It preserves caller isolation and commit fate. A database failure aborts that
transaction; validation and duplicate do not. The normal typed validation set
includes `InvalidDelay`; its maximum is 36,500 days and sub-microseconds are
dropped. `assert_valid_kind_name` is a reusable const assertion for static
kind names, while registration and enqueue retain typed runtime rejection.

The trace carrier stores only `trace_context` (legacy traceparent) and
`trace_state`. It allows only those keys, bounds them to 256 and 512 ASCII
bytes respectively, rejects controls and malformed nonempty tracestate, and
links a valid extracted remote span rather than making it the consumer parent.
It stores no baggage. Parent-only legacy rows remain valid; malformed or
overbound context leaves the attempt unlinked without failing the job.

## Static claims and outcomes

Registered policy supplies `(kind, max_attempts, timeout)`. A committed claim
sets its immutable lease to `statement_timestamp() + timeout + 60 seconds`.
The local deadline derives from the same whole-microsecond timeout and is two
seconds earlier. Acknowledgement never resets it; there is no heartbeat,
extension, upkeep task, or claim readback. A failed or unknown claim commit
dispatches no returned rows and leaves any committed claim to expire. A claim
acknowledged past its local deadline is likewise not dispatched.

Claims lock while they scan, the canonical `SKIP LOCKED` queue form: each
registered kind's due pending rows and expired running rows are read in
`not_before, id` order by a lateral scan that takes `FOR UPDATE SKIP LOCKED`
itself, at most `batch` per kind and branch. A row another session holds is
skipped and the scan moves to the next one, so concurrent workers take
disjoint jobs without underfilling their free slots, and a locked early job
never blocks later work. The statement then updates the globally earliest at
most `batch` of those rows; the update rechecks live eligibility. Candidates
it locked but did not pick stay locked only until the short claim transaction
commits. Choosing IDs before locking them was rejected: concurrent workers
chose the same IDs, the losers claimed nothing until the next poll, and a
one-slot worker stalled behind a locked earliest job. Measured on PostgreSQL
18.6 with 20 ms jobs, eight one-slot workers claimed 42 jobs/s that way and
271 jobs/s locking while scanning. This does not promise constant scan cost:
an indexed expired range can sort. There is deliberately no cursor, quota,
priority, fairness, or execution-order protocol.

Each admitted supervisor owns one claim, slot, immutable deadline, handler
join handle, and intended outcome until cleanup ends. It captures the outcome
arguments once. A known handler result wins over force/timeout when its join
completes; a successfully joined result also wins after an abort request. Once
a result is known, the supervisor persists it and can never replace it with a
release. A panic is a known failure. When joining or persistence cannot finish
inside its deadline, it writes nothing further and expiry recovers the row.

Every transition is fenced by `(id, claim_generation, state = 'running')`.
An acknowledged one-row write is applied; an acknowledged zero-row write is
unchanged and makes no attribution claim. Errors or unknown acknowledgement
retry the identical operation at the existing one-second cadence only until
the local or cleanup deadline. Unknown at deadline is uncertainty, not a
durable outcome. A retry delay is captured once: `attempt^4 * (0.9 + 0.2 *
draw)` seconds, with the PostgreSQL claim's `random()` draw and microsecond
round-down.

`Job::complete_in_tx(&mut PgConnection)` performs the same fenced COMPLETE
inside the caller's transaction. It returns `CompleteError::NoTransaction`,
`CompleteError::StaleClaim`, or its SQL cause; it neither obtains a connection
nor controls the transaction. Callers must propagate it from their transaction
closure so stale ownership rolls back preceding business writes. On a
`CommitUnknown`, map the transaction result to
`JobError::transaction_unknown(error)`: record uncertainty and issue no
retry, failure, or release transition. Do not blindly replay the closure.

`JobError::retry_after(error, delay)` and `JobError::snooze(delay)` use the
same checked delay domain and return `Result<_, InvalidDelay>`. Retry-after
spends an attempt and has no jitter. Snooze wins over exhaustion, returns the
row to pending at database time, clears its claim, and refunds exactly one
attempt; its fenced SQL cannot refund again after the first transition. A
handler cancelled by a forced drain is released: the unit is refunded the same
way, but `not_before` is unchanged, so the job is due at once and keeps its
place in claim order rather than queueing behind the backlog.

## Observation and retention

Every ten seconds each worker samples only registered kinds. For each kind and
`available`, `scheduled`, and `running`, an indexed `SELECT 1 ... LIMIT 1001`
publishes `min(count, 1000)` and a censoring gauge; the sample cap gauge is
1000. The oldest available age is a separate indexed, due-pending lookup from
the same database timestamp. These are per-process samples, not replica sums;
the removed `<unregistered>` aggregate is not replaced. Unknown kinds remain
unconsumed.

Before a successful sample, backlog/age/censoring are NaN, timestamp is zero,
and success is zero. A success publishes gauges and its database timestamp. A
failure restores NaN and success zero while retaining the last-success
timestamp. Consumers require success=1, timestamp>0, and age no greater than
30 seconds, distinguishing unobserved, exact zero, censoring, failed sampling,
and a stopped observer. The two-second statement timeout is a time backstop,
not a scan-size proof. Retention remains bounded terminal deletion; it never
deletes live rows. Terminal retention is independent of registered kinds.

## Rollout and proof boundary

Stop all producers and workers before applying
`20260925000001_simplify_background_jobs.sql`; bytea binaries cannot overlap
JSONB/text binaries. The existing migrator applies it atomically. On a known
failure it rolls the file and history entry back; on an unknown runner result,
inspect migration history and schema before choosing a compatible binary.
After conversion, old binaries and down-migration are not recovery. The
forward file preserves every job's identity, timing, attempts, generation,
state, and terminal history.

Relevant proof must exercise stale transactional completion, both sides of an
unknown commit, repeat snooze/refund, result-ready forced drain, disjoint
claims with locked candidates, migration from old compatible and refused rows,
trace-state and legacy parents, and every observation freshness state. Query
plans and lock observations support only the bounded-indexed claims above; no
performance percentage is promised.

## Reopen conditions and watch list

Reconsider the static lease only for a changed availability requirement or
measured unacceptable rescue latency; reconsider capped observation only for
measured aggregate observer cost. Reconsider library reuse only when a
maintained release meets the caller-connection enqueue, schema ownership,
execution-budget, and worker-lifecycle contracts together. A proposed queue,
online dual-format conversion, or lifecycle extraction needs its own accepted
design.

## Decisions recorded here

The durable decisions are the static lease, supervisor-owned outcome,
lock-while-scanning claims, JSONB/text conversion, capped fresh samples, and
the deferred lifecycle extraction recorded above. They remain active after the
planning bundle is removed.
