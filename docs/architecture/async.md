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
| `migrations/` | The canonical table migration; neither service nor worker changes schema at runtime. |
| `crates/infra-<provider>` | Concrete kinds, handlers, and producer calls. Features never depend on `infra-jobs`. |

The pack keeps PostgreSQL, `sqlx` 0.9, Tokio/TaskTracker, `serde_json`, and
the installed OpenTelemetry propagator. It adds no queue framework, lifecycle
crate, random-number dependency, or second observer process. The former lease
upkeep, outcome-attribution readback, and central attempt registry are removed.

Signals, deadline clamping, and task teardown remain separate service and
worker lifecycle owners. Extract a shared lifecycle crate only when a shared
change to signal semantics, deadline arithmetic, or tracked-task teardown
offsets the public API and profile cost; a third binary alone is insufficient.
The webhook provider and the messaging outbox, where retained, reuse this
pack's scheduling, attempts, and fenced completion rather than creating
competing queue machinery.

## Storage and enqueue

`background_jobs` retains its identity, state, attempt count, generation,
enqueue-time `created_at`, last successful claimant `attempted_by`, timing,
terminal history, the `errors` failure history, and live unique-key
constraint. `errors` is a JSONB array that the fenced retry and failure
writes and the claim's lease-expiry rescue append to, one
`{"attempt", "at", "error"}` entry per spent attempt (River and Oban keep the
same history); `error_summary` stays the latest entry's text. The successful
path never writes it. The `id` column has no default: enqueue supplies a
UUIDv7, and the ids, the worker id, and the completion batch bind through
sqlx's `uuid` codec, which `infra-postgres` enables for the whole PostgreSQL
profile so the resolved sqlx graph does not vary with the retained packs. Its canonical
migration requires a UTF-8 server and stores `payload` as `jsonb`, `unique_key`
as `text COLLATE "C"`, nullable `trace_state`, and the running index
`(kind, claim_expires_at, not_before, id) WHERE state = 'running'`. JSON values are the payload contract: PostgreSQL may normalize
formatting, duplicate keys, and numeric spelling. `C` collation preserves
exact UTF-8 key equality independent of the database default.

The migration sets `autovacuum_vacuum_scale_factor = 0` and
`autovacuum_vacuum_threshold = 5000` on this high-churn table. PostgreSQL
server settings remain operator-owned: use an `autovacuum_naptime` around ten
seconds, watch transaction age and xmin age, and consider PostgreSQL 17+
`transaction_timeout` for other roles. The migration changes neither a server
setting nor a role.

Before database access, enqueue validates kind, key, delay, serialized size,
and decoded NUL. It serializes once with `serde_json`; the validation detects
unescaped `\\u0000` without a lossy value round trip. Valid JSON and keys bind
as text with explicit casts, and the insert is enqueue's only statement.
UTF-8 is a schema precondition, not a per-call query: a database's
`server_encoding` is fixed at creation, the canonical migration requires UTF-8,
and worker startup verifies UTF-8. Migration history admission replaces
table-shape probes; it does not silently repair incompatible databases.

`enqueue` remains the only insert. It takes the shared opaque
`&mut infra_postgres::Tx` that the caller's `in_tx` closure receives, obtains
its connection inside the jobs adapter, and issues no transaction-control SQL
or second connection.
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
The attempt span keeps its `job.*` fields and takes no `messaging.*`
attributes: the queue is a table of this service's own database, and the
outbox attempt already contains the broker's `messaging.*` send span, so a
second messaging system on the enclosing span would name PostgreSQL as a
broker in every trace of a published event.

## Static claims and outcomes

Registered policy supplies `(kind, max_attempts, timeout)`. A committed claim
sets its immutable lease to `statement_timestamp() + timeout + 60 seconds`.
The local deadline derives from the same whole-microsecond timeout and is two
seconds earlier. Acknowledgement never resets it; there is no heartbeat,
extension, upkeep task, or claim readback. A failed or unavailable claim
acknowledgement dispatches no returned rows and leaves any committed claim to
expire. A claim acknowledged past its local deadline is likewise not dispatched.
Stop does not cancel a claim already invoked: it settles within its existing
backstop, and an acknowledged locally-valid row transfers to its supervisor
even if stop arrived in flight. No subsequent polling round starts after stop.

Claims follow the last round's result: a round that filled every free slot
claims again at once, a round that found work claims again 25 ms after it
started, and an empty round waits for the one-second poll or a wake. Enqueue
wakes idle workers with `NOTIFY background_jobs` (payload: the kind name),
debounced to once per 25 ms for each kind in a process and sent as a separate
statement only by the enqueue that wins the debounce. The debounce is per
kind because a listener wakes only the engines that register the notified
kind: one process-wide interval let a transaction's first enqueue suppress
the wake of its second kind, whose engine then waited for the poll. Every notifying commit takes
PostgreSQL's notify queue lock: notifying each enqueue halved concurrent
enqueue throughput at 16 connections, and a wake per notification turned
into a claim storm at 1000 jobs/s. A worker process listens on one connection
outside its pool: the engine built with `Engine::new` owns that listener and
terminal retention, and an engine built with `Engine::beside` it shares both
and its worker id instead of repeating them. Polling remains the recovery
path, and the only path through a transaction-mode pooler, where `LISTEN`
succeeds and delivers nothing. The listener subscribes at most once per poll
interval and reports each lost connection as a `listen` failure: the driver
returns a loss as no notification, without an error, so the earlier loop
neither counted it nor bounded how often it resubscribed and woke every
engine beyond the time a connection handshake takes. On DigitalOcean c-4
(PostgreSQL 18, 16 slots) pickup latency at 50 jobs/s went from p50 486 ms /
p99 981 ms to p50 15 ms / p99 28 ms, and debounced enqueue cost stayed within
noise.

Claims lock while they scan, the canonical `SKIP LOCKED` queue form: each
registered kind's due pending rows and expired running rows are read in
`not_before, id` order by a lateral scan that takes `FOR UPDATE SKIP LOCKED`
itself, at most `batch` per kind and branch. A row another session holds is
skipped and the scan moves to the next one, so concurrent workers take
disjoint jobs without underfilling their free slots, and a locked early job
never blocks later work. The statement then updates the globally earliest at
most `batch` of those rows; the update rechecks live eligibility. Candidates
it locked but did not pick stay locked only until the claim autocommit
statement completes. Choosing IDs before locking them was rejected: concurrent workers
chose the same IDs, the losers claimed nothing until the next poll, and a
one-slot worker stalled behind a locked earliest job. Measured on PostgreSQL
18.6 with 20 ms jobs, eight one-slot workers claimed 42 jobs/s that way and
271 jobs/s locking while scanning. This does not promise constant scan cost:
an indexed expired range can sort. There is deliberately no cursor, quota,
priority, fairness, or execution-order protocol.

Each admitted supervisor owns one claim, slot, immutable deadline, handler
future, and intended outcome until cleanup ends. It captures the outcome
arguments once. The handler runs on the supervisor's task behind
`catch_unwind`, which removed one task spawn per attempt (-11% worker CPU per
job at 64 slots). On timeout or forced drain it first cancels the handler and
allows up to 100 ms of cooperative completion inside the existing deadline,
then drops a still-running handler. The slot returns when the handler's result
is known, before the outcome write, as River frees a worker before its batch
completer writes; the supervisor still owns that write and the drain still
waits for it (+17-23% jobs/s at 4-16 slots). A handler result that joins before the
cancellation is known and wins over force/timeout. After the cancellation only
a successful join is known; an error, snooze, or panic that answers it takes
the cancellation's disposition, so a forced drain releases the job however a
cooperative handler reacts. Once a result is known, the supervisor persists it
and can never replace it with a release. A panic before cancellation is a
known failure. When joining or persistence cannot finish
inside its deadline, it writes nothing further and expiry recovers the row.

Every transition is fenced by `(id, claim_generation, claim_expires_at IS
NOT NULL)`; the table CHECK makes the last term equal to `state = 'running'`.
The literal state predicate let the planner prove the partial running index
and scan all of it, dead entries of every job claimed since the last VACUUM
included, so each outcome write grew with the backlog (1 ms per COMPLETE after
20k jobs; fixing it raised 64-slot throughput from 3.3k to 8.3k jobs/s).
Completions queued while one completion write is in flight go out together in
the next `UPDATE ... FROM unnest(...)` (group commit, like River's batch
completer): at 64 slots database CPU per job fell from 305 to 97 us.
An acknowledged one-row write is applied; an acknowledged zero-row write is
unchanged and makes no attribution claim. Errors or unavailable acknowledgement
retry the fenced write at the existing one-second cadence only until
the local or cleanup deadline. Unknown at deadline is uncertainty, not a
durable outcome. PostgreSQL computes `attempt^4 * (0.9 + 0.2 * random())`
seconds (floored by `retry_after_at_least`) when it writes the retry; a
re-sent fenced write may redraw.

`Job::complete_in_tx(&mut infra_postgres::Tx)` performs the same fenced COMPLETE
inside the caller's transaction. It returns `CompleteError::StaleClaim` or its
SQL cause and does not control the transaction. Callers must propagate it from
their transaction closure so stale ownership rolls back preceding business
writes. A `CommitUnknown` is an ordinary retryable handler failure: never
replay its business closure. The following fenced outcome sees an already
committed COMPLETE as unchanged, can retry after rollback, and otherwise
leaves recovery to expiry when its acknowledgement cannot be established.

`JobError::retry_after_at_least(error, delay)` and `JobError::snooze(delay)`
use the same checked delay domain and return `Result<_, InvalidDelay>`.
`retry_after_at_least` spends an attempt and is a floor under the normal
jittered backoff. Snooze wins over exhaustion, returns the row to pending at
database time, clears its claim, and refunds exactly one attempt; its fenced
SQL cannot refund again after the first transition. A
handler cancelled by a forced drain is released: the unit is refunded the same
way, but `not_before` is unchanged, so the job is due at once and keeps its
place in claim order rather than queueing behind the backlog.

## Observation and retention

Every ten seconds each worker samples only registered kinds. For each kind and
`available`, `scheduled`, and `running`, an indexed query publishes the count
capped at 1000; a value of 1000 means at least that many rows. The oldest
available age is a separate indexed, due-pending lookup from the same database
timestamp. These are per-process samples, not replica sums; the removed
`<unregistered>` aggregate is not replaced. Unknown kinds remain unconsumed.

The only sampling gauges are `jobs_live_jobs{kind,state}`,
`jobs_oldest_available_age_seconds{kind}`, and
`jobs_observation_timestamp_seconds`. Before first success every registered
value and timestamp is zero. A completely decoded successful sample publishes
all values and then its database timestamp. A query or decode
failure retains the last good values and timestamp; operation-failure telemetry
still records the failure. `jobs_operation_failed` carries `sqlstate` or
`cause`. Consumers reject timestamp zero or a timestamp older
than 30 seconds. The two-second statement timeout is a time backstop, not a
scan-size proof. Retention remains bounded terminal deletion; it never deletes
live rows. Terminal retention is independent of registered kinds.

## Proof boundary

Relevant proof must exercise stale transactional completion, both sides of an
unknown commit through ordinary retry, repeat snooze/refund, result-ready
forced drain, stop during an in-flight claim, disjoint claims with locked
candidates, canonical-migration admission, rescue identity/evidence,
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

Screened again on 2026-10-01 against crates.io and each repository.
graphile_worker 0.13.6 (sqlx 0.9) enqueues on the caller's transaction via
`WorkerUtils::with_executor`, but keeps its own `graphile_worker` schema and
migrator, makes crashed-worker recovery an opt-in heartbeat sweeper, completes
by id with no fenced outcome, has no per-kind attempt deadline, and has one
maintainer. apalis-postgres is still 1.0.0-rc.9 (sqlx 0.9, own `apalis`
schema, acknowledgement fenced only by worker id). awa 0.6.9 fences
completions by run lease but releases on sqlx 0.8, sets deadlines per queue,
and has one maintainer. underway 0.2.0 (2025-07) is on sqlx 0.8. Loco 1.2.0
ships its own PostgreSQL queue rather than one of these.

## Decisions recorded here

<!-- template:begin webhooks-common:docs-async-webhooks -->
## Durable webhook work

The optional webhook directions reuse this queue; they do not add a second queue,
worker loop, lifecycle owner, delay engine, or attempt observer. The jobs worker
owns claims, deadlines, jitter, retries, terminal retention, and shutdown.

`Job::complete_in_tx` accepts the opaque `&mut infra_postgres::Tx` and keeps its
generation-fenced update. The same captured attempt deadline is visible to the
handler, so webhook signing/HTTP or consumer work cannot extend the supervisor's
budget.
<!-- template:end webhooks-common:docs-async-webhooks -->

<!-- template:begin webhooks:docs-async-webhooks-outbound -->
Outbound work inserts `webhooks.deliver` in the caller's `&mut Tx`; the worker
uses the retained 20-attempt, 30-second policy. Receiver `Retry-After` advice is
only a capped floor; jobs combines it with normal backoff. A completed 2xx
succeeds, 410 terminates with operator guidance to disable the configured
endpoint, and every other HTTP response retries. Missing endpoints consume
attempts. Only endpoint capacity uses a one-second snooze with an attempt
refund: each worker process permits one active exchange per endpoint, keeping
another slot available when the worker has at least two slots. This is neither
cross-process suppression nor queue fairness.
<!-- template:end webhooks:docs-async-webhooks-outbound -->

<!-- template:begin inbound-webhooks:docs-async-webhooks-inbound -->
An inbound receipt inserts `webhooks.process` in its receiver transaction; the
worker uses the existing 25-attempt, 60-second policy. Both process roots
construct the shared adopter registry once and reject unbound configured
endpoints at startup; a historical job whose binding disappeared retries and
exhausts normally. Consumer work and fenced completion remain one transaction.
<!-- template:end inbound-webhooks:docs-async-webhooks-inbound -->

The durable decisions are the static lease, supervisor-owned outcome,
lock-while-scanning claims, JSONB/text conversion, capped fresh samples, and
the deferred lifecycle extraction recorded above. They remain active after the
planning bundle is removed.

<!-- template:begin messaging:docs-async-messaging -->
## JetStream alongside jobs

JetStream is independently runnable and is not a PostgreSQL queue. When both
profiles are retained, `jobs-worker` composes the two engines but gives them
separate admitted capacity so busy job handlers cannot prevent due messaging
work. The messaging adapter owns broker pull, handler settlement and DLQ; jobs
owns only its existing claims, attempts, and terminal history.
<!-- template:end messaging:docs-async-messaging -->

<!-- template:begin outbox:docs-async-outbox -->
The transactional outbox reuses that jobs authority without adding a table,
queue loop, or transaction owner. A second one-slot engine, built beside the
ordinary one so both share one listener, retention loop, and worker id,
registers only the private publication kind. Combined ordinary jobs plus outbox need `N + 5` pool
connections; outbox-only needs three. Its claim loop, jobs maintenance, and
all engines share the worker's existing shutdown deadlines. See
[PostgreSQL transactional outbox](../postgres-transactional-outbox.md).
<!-- template:end outbox:docs-async-outbox -->

<!-- template:begin worker:docs-async-worker-lifetime -->
The retained worker process has one signal, readiness, grace, task-tracking,
and dependency-close lifecycle even when it composes jobs and messaging. It
stops both admission loops at drain, shares one absolute drain/cleanup budget,
joins application-owned work, and does not grant either engine a second full
grace period.
<!-- template:end worker:docs-async-worker-lifetime -->
