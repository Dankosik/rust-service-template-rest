# Background jobs technical design

Status: ready (independent Technical Design review: PASS)

Consumes the ready [specification](../spec.md); its preserved invariants remain
normative. This document closes mechanisms, [ownership](ownership.md) closes
placement, and [rollout](rollout.md) closes migration order. No implementation
or runtime evidence is claimed here.

## Decisions and reuse

Keep the current PostgreSQL queue, `infra_postgres::in_tx_with`, sqlx 0.9.0,
Tokio/TaskTracker, serde_json and installed OpenTelemetry 0.32.0. Historical
crate comparison remains in the [Definition evidence](../research/definition-baseline.md):
apalis/other queue frameworks still cannot replace the caller-connection enqueue
and policy boundary without a larger migration. No crate/feature upgrade or new
dependency is selected. The simplification deletes upkeep, attribution reads,
and the central per-attempt result/deadline registry rather than replacing them
with another reconciliation protocol.

| Choice | Decisive constraint; accepted cost; reopen evidence |
| --- | --- |
| Static leases instead of renewals | Accepted timeout + 60 seconds; crash rescue may wait the full lease. Reopen only for a changed availability requirement or measured unacceptable rescue latency. |
| Supervisor owns one attempt until cleanup ends | A known result must never race a separate releaser. Root cancellation communicates a deadline, never takes over the outcome. Reopen if an actual detached task is required. |
| PostgreSQL `random()` returned with claim | Existing supported random source, no Rust RNG dependency or custom generator; one draw travels with an acknowledged claim. Reopen for a requirement to compute jitter outside PostgreSQL. |
| Capped observation on each worker | No leader lease, observer role, or new operational deployment; work scales with registered kinds and workers, with a fixed row cap. Reopen if measured aggregate observer cost needs coordination. |
| Keep separate process lifecycle owners | Signals, deadline clamping and cancel/close/join are duplicated; HTTP propagation/drain and worker drain/outcome cleanup have different policy. A new `infra-runtime` leaf would expose three tiny public primitives and affect every profile; placement in service/jobs/health/telemetry would give a current owner unrelated responsibilities. Defer extraction until either existing process changes signal semantics, deadline arithmetic, or tracked-task teardown and the shared change offsets that cost. A third binary alone is not the trigger. |

## Claim and local authority

Registered policy supplies `(kind, max_attempts, timeout)` to the claim statement.
The lease is `statement_timestamp() + timeout + interval '60 seconds'`. Bind
whole microseconds; derive the local deadline from **the same truncated duration**:
`sent_before_query + timeout + 60 seconds - 2 seconds`. An acknowledgement does
not reset it. Handler execution is bounded by the lesser of its policy timeout
from dispatch and this absolute deadline. Persistence also stops at this deadline.
The 12-second operation backstop, 1-second polling, pool bound and existing
statement/transaction classifier remain unchanged. No heartbeat task survives.

Only a known committed claim dispatches. A failed/unknown commit drops returned
rows and slot permits, records the operation failure, and lets any committed
claim expire. There is no SELECT to rediscover it. A claim acknowledged after
its local deadline is not dispatched. Its original lease recovers it.

Use the existing pending index `(kind, not_before, id)`. Replace the running
index with `(kind, claim_expires_at, not_before, id) WHERE state = 'running'`
to support registered-kind expiry lookup and bounded running counts. Claim SQL
has these three stages in one transaction:

1. For each registered kind, discover up to the requested batch of due pending
   IDs and up to that batch of expired running IDs, each ordered by
   `(not_before, id)`, **without row locks**. The running branch uses its indexed
   expiry range and may sort expired candidates; no constant scan-cost claim is
   made for claiming.
2. First materialize the globally selected IDs **before locking**:
   `selected AS MATERIALIZED (SELECT id FROM candidates ORDER BY not_before,id
   LIMIT batch)`. Join only those IDs back to `background_jobs AS job`, reapply
   the live due/expired and registered-kind predicates, and use
   `FOR UPDATE OF job SKIP LOCKED`. Return the locked generation and use it as
   an additional equality guard in the update. This is the only locking SELECT;
   neither per-kind discovery nor the materialized selection locks rows.
3. Update only selected locked IDs: current exhaustion rules, sequence-generated
   fencing, attempt increment, and per-kind expiry. Return payload as
   `payload::text`, trace fields and one `random()` draw in `[0,1)` for each
   running attempt. Exhaustions do not dispatch.

The locking SELECT is separate from the UNION query (PostgreSQL does not allow
locking a UNION result). Concurrent change is rechecked on the row being locked;
final update guards defend the selected identity. PostgreSQL can retain a lock
on a row rejected by that recheck, so a LIMIT on the locking SELECT alone would
not bound locks. The materialized list permits at most `batch` job IDs to reach
any lock attempt, including rejected rows. Contention can underfill a batch even
when other jobs are due; normal polling rediscoveries retry after competing
transactions finish. A persistently locked earliest selection can delay later
jobs. Accept this bounded-lock tradeoff; add no overfetch/cursor protocol.
Earliest-due preference and unknown-kind isolation
remain; a sustained kind backlog may delay another kind. There are no quotas,
priorities or strict execution-order guarantees.

## One supervisor, one fixed queue outcome

Each admitted supervisor owns the claim, slot, immutable deadline, handler
JoinHandle, and eventual intended outcome on its stack. The attempt TaskTracker
owns/join-tracks supervisors; its length supplies in-flight count. Remove the
`Attempts` map, per-attempt deadline watch, extension acknowledgements,
supersession token, attribution structs and `lease::reconcile`. Keep only
process-level stop/failure/forced-cleanup control and bounded counters.

An intended transition captures all arguments once: ID/generation, outcome,
sanitized summary, and delay. Ordinary delay is `attempt^4 * (0.9 + 0.2 * draw)`
seconds, rounded down to microseconds; no later write draws again. Complete,
fail, retry, snooze and release all require the same ID, generation and
`state = 'running'`. Expiry alone is not an outcome-write rejection.

Persist with the existing explicit READ COMMITTED transaction and commit
classifier. Retry the identical operation after an error/unknown acknowledgement,
at the existing one-second interval clamped to the local/cleanup deadline.
An acknowledged one-row write ends persistence; an acknowledged zero-row write
also ends it, with **no attribution claim**. A repeat following a committed write
is therefore a no-op, including a snooze/release refund. Unknown acknowledgement
at deadline emits uncertainty and leaves recovery to expiry. The handler never
runs twice in this supervisor.

Record handler duration and observed result once, when known, independently of
queue-write acknowledgement. Queue operation failure and persistence disposition
(`applied`, `unchanged`, `unknown`) remain distinguishable. Remove `written` and
`superseded` claims derived from readback. Finite attempt labels describe
observed success, retryable failure/timeout, exhaustion, permanent failure,
snooze, cancellation and transactional uncertainty; none promises durable
transition attribution. Retain bounded kind labels and no payload/error labels.

## Stop and the result/cancellation race

The worker still disables readiness, stops claiming, drains, then uses the
existing **2-second cleanup stage**, 2-second listeners, 3-second background
join, 5-second pool close and 5-second telemetry stage: the 17-second tail and
parent grace remain unchanged. Rename the drain-end record/API to describe
cleanup rather than proven releases (`cancel_and_finish` / `attempts_finished`).

`stop_claiming` stops new sends/admissions. The claim loop races an in-flight
claim with stop; if cancellation loses its acknowledgement, it dispatches no
rows and leaves expiry recovery. It closes the supervisor tracker on exit.
When forcing the drain, store one absolute cleanup deadline before firing one
shared force token. Already-issued outcome writes shrink to
`min(local_deadline, cleanup_deadline)`; dropping a write does not change the
intended operation or imply it rolled back.

The supervisor polls a completed handler JoinHandle before force/timeout arms
using biased selection. If force wins, request cancellation, abort the handler,
and join it inside the remaining deadline. A successfully joined result still
wins even if abort was requested meanwhile. Only a joined cancellation means
unfinished work eligible for release/refund. A handler panic remains a known
failure. Once a result exists, the supervisor persists that result and can
never switch to release. If joining or persistence cannot finish, write nothing
further and leave expiry recovery; never extend grace to obtain certainty.

`Started::cancel_and_finish` signals, then waits for tracked supervisors within
that deadline; it does not abort supervisors or independently write their rows.
Counters report local known results, cancelled handlers, acknowledged release
writes and uncertain cleanup, without claiming zero-row writes were releases.
An in-flight caller transaction may still finish its rollback/commit in the
driver after cancellation: the fenced completion and release serialize on the
same row. A committed completion makes release a no-op; a rolled-back completion
cannot leave committed business effects. No claim readback is needed.

## Handler API, completion, and delay

Keep `Handler::run -> Result<(), JobError>` and ordinary handlers. `Job<K>` gains
a private generation copied from its claim and exposes
`complete_in_tx(&self, conn: &mut PgConnection) -> Result<(), CompleteError>`.
It checks sqlx `Connection::is_in_transaction()` before issuing SQL (the
supported caller is `infra_postgres::in_tx` / `in_tx_with`), then executes the
same fenced COMPLETE statement. One affected row returns success;
zero returns `CompleteError::StaleClaim`; an absent tracked transaction returns
`NoTransaction`; SQL errors retain their cause. The method does not acquire,
commit, roll back, create a savepoint, or mutate a local completed flag.

Supported adapter usage puts business effects and `job.complete_in_tx(conn).await?`
inside one existing transaction closure. `CompleteError` must propagate out of
that closure: `in_tx` rolls back every prior business write on stale ownership.
Swallowing that error and returning success is outside the supported contract;
the guide must show the error type/conversion and the `?`, not just a loose SQL
snippet. No generic business-closure replay helper is added. The existing
transaction boundary's commit classifier stays unchanged.

After an acknowledged commit, return `Ok(())`; the supervisor's ordinary
completion is an intentionally harmless zero-row operation. A `CommitFailed`
or closure error follows explicit adapter failure policy. A `CommitUnknown`
must map to a dedicated `JobError::transaction_unknown(error)` disposition:
record uncertainty and issue **no retry/fail/release transition** for that known
result, so an uncommitted job recovers by expiry and a committed one stays
complete. The adapter never blindly reruns the closure. The force-cleanup path
honors that disposition exactly like any known result. An external effect still
needs the stable job ID/provider idempotency; this API promises atomicity only
for effects inside the supplied PostgreSQL transaction.

Extend the private JobError disposition (do not overload the permanent boolean)
with checked constructors `retry_after(error, Duration)` and `snooze(Duration)`,
each returning `Result<JobError, InvalidDelay>`. Preserve retryable/permanent
constructors and the existing error-to-retryable conversion. Share one checked
delay conversion with enqueue: maximum 36,500 days, truncate sub-microseconds,
zero allowed; out-of-range returns the typed error before any queue SQL.

Retry-after spends an attempt, uses the supplied delay without jitter, and
exhausts at the normal cap. Snooze takes precedence over cap exhaustion, sets
pending and `not_before = statement_timestamp() + delay`, clears the claim and
refunds exactly one attempt; it does not overwrite error history as a failure
or emit failure telemetry. Identity/live uniqueness are unchanged. Repeated
snooze SQL cannot refund twice because the first transition leaves running.

Make the existing kind-name predicate `const`; export a small const assertion
usable as `const _: () = assert_valid_kind_name(MyKind::NAME)`. Apply it to
shipped documentation/test kinds. Runtime registration/enqueue retain typed
validation for kinds that do not opt into a const assertion; do not insert an
unconditional generic assertion that prevents testing/rejecting reachable input.

## JSONB, text and trace carrier

Forward migration `20260925000001_simplify_background_jobs.sql` asserts
`server_encoding = 'UTF8'`, changes payload using
`convert_from(payload, 'UTF8')::jsonb`, changes unique key using
`convert_from(unique_key, 'UTF8')` to `text COLLATE "C"`, adds nullable
`trace_state text`, and replaces the running index above. Preserve all other
columns/state and the live unique index; explicit C collation preserves exact
UTF-8 key equality independent of database default collation. No payload GIN
index, dual format, trigger or second table is introduced.

Serialize enqueue payload once with installed serde_json, enforce its existing
serialized byte limit, and reject decoded NUL before **any** SQL. For the trusted
serializer-produced JSON bytes, consume backslash escape pairs; reject an
unescaped `\u0000` sequence, while a consumed `\\` pair prevents the following
literal `u0000` from being treated as an escape. This checks every key/value,
including values later hidden by duplicate keys, without a second serialization,
lossy Value round-trip, custom JSON parser or new nesting limit. Keep all existing
kind/key/delay validation before database access. Bind valid JSON as text with
an explicit SQL `::jsonb` cast (no sqlx JSON feature), and decode claimed
`payload::text` through serde_json. JSON values, not formatting/key order, are
the contract; PostgreSQL handles last-key-wins duplicate-key normalization.

After Rust validation, enqueue first runs a successful
`SELECT current_setting('server_encoding')` on its caller connection and returns
typed `UnsupportedEncoding` unless UTF8, before sending payload/key text. This
small extra round trip is the simple producer guard without a new pool/witness
type or changing global PostgreSQL admission. Worker startup checks encoding and
actual payload/key types plus the trace-state column under its existing budget;
wrong encoding has a distinct startup refusal. The migration also guards UTF8.
No global pool or HTTP-idempotency policy is changed.

Keep `trace_context` as the legacy traceparent field; new `trace_state` stores
tracestate. A small carrier implements OpenTelemetry Injector/Extractor,
allowlisting only these two keys. Capture with
`global::get_text_map_propagator(...inject_context...)`; extract with
`extract_with_context(&Context::new(), ...)` and add the valid remote span
context as a **link**, never as the consumer's parent. Allow at most 256 bytes
for traceparent and 512 for tracestate, ASCII/no control characters. Before
extraction, validate a present nonempty tracestate with the installed
`opentelemetry::trace::TraceState::from_str`; absence/empty means no tracestate.
Reject the whole carrier on that parse error: SDK 0.32.1's propagator otherwise
silently defaults malformed tracestate while preserving the valid parent.
Overbound or malformed context becomes an unlinked attempt without failing the job; no
baggage is stored or extracted. Legacy parent-only rows work unchanged. Rename
the codec module to `trace_context.rs` and delete handwritten W3C parsing and
formatting. `infra-telemetry` already installs SDK `TraceContextPropagator`;
infra-jobs does not install globals or depend on infra-telemetry.

## Queue observation

Every ten seconds, each worker queries only its registered kinds. For each kind
and state (`available`, `scheduled`, `running`), count rows from an indexed
`SELECT 1 ... LIMIT 1001` subquery: publish `min(count, 1000)` and
`jobs_live_jobs_censored{kind,state} = (count > 1000)`. Publish a constant cap
of 1000 in `jobs_live_jobs_sample_cap`. Pending ranges split at the sample's
single `statement_timestamp()`; the running index starts with kind. No live
GROUP BY scan remains. At most `3 * kinds * 1001` visible entries feed counts;
MVCC dead tuples/planner I/O are not bounded by that arithmetic.

Oldest-ready uses a separate indexed per-kind pending lookup ordered by
`not_before,id LIMIT 1` with `not_before <= statement_timestamp()` and derives
age from that same database timestamp. It is independent of count censoring;
no future/terminal row contributes. Both queries can be lateral components of
one sampling statement. No locking clauses are used. Keep the two-second sample
statement timeout as an elapsed-time backstop, not a scan-size proof.

Publish all sample gauges only after successful transaction acknowledgement.
Before first success, publish backlog/age/censoring as NaN, timestamp as 0 and
`jobs_observation_success = 0`. Success publishes values, database Unix timestamp
in `jobs_observation_timestamp_seconds`, and success=1. Failure sets success=0
and backlog/age/censoring to NaN while preserving the last-success timestamp.
Readers require success=1, timestamp>0 and timestamp age<=30 seconds; the age
guard detects a stopped observer even when it cannot publish a failure.
Document gauges as per-process samples, not additive counts across replicas.
Remove the `<unregistered>` aggregate rather than scan all unknown kinds;
unknown-kind rows still remain unconsumed. This visibility tradeoff is explicit.

## Proof and affected authorities

Implementation chooses minimal cases under [the existing PostgreSQL owner](../../../docs/validation/postgres.md)
and final-validation boundary. Core falsifiers are stale transactional completion
after business writes, either side of lost COMMIT acknowledgement, repeat
snooze/release refund, result-ready at forced drain, and claim disjointness with
multiple kinds/locked candidates. Query plans/lock observations must support
the narrower indexed/capped claims; no performance percentage is promised.
Migration proof must run from the immutable old schema with compatible and
refused rows, not only against an empty new schema. Trace proof includes
tracestate and legacy parent-only rows. Observation proof distinguishes
unobserved, exact zero, censored count, failed sample and stopped observer.

The e6 enqueue suite keeps its primary PostgreSQL ownership. Consolidate setup
or tables only after mapping no-holder/per-kind, terminal holder, visible live
holder, post-snapshot insert/update, and in-flight insert/update commit/rollback
to Created/Duplicate/40001, blocking and transaction-usability oracles. No case
is approved for deletion by name/count alone; Implementation applies test-audit
to the actual complete cases and keeps each distinct oracle.

Update conflicting active guide, async/persistence/runtime architecture and
crate API docs. Update the jobs profile removal inventory for the new migration;
otherwise a jobs=none initializer would retain jobs schema. Roadmap 10.5/10.6
explicitly reuse infra-jobs scheduling, attempts and transactional completion;
webhook signing/HTTP policy and outbox/JetStream publication semantics stay in
their adapters. Do not implement those future products or redesign their
business atomicity here. Existing CI-owned DB, migration, profile and image
gates stay CI-owned; no new mandatory local matrix.

## Primary evidence used

The current pinned PostgreSQL runtime is 18 (`env/docker-compose.yml`).
[SELECT locking](https://www.postgresql.org/docs/18/sql-select.html#SQL-FOR-UPDATE-SHARE)
supports row locks after ordering/limit, SKIP LOCKED and the UNION restriction;
the proposed composed query still needs actual plan/concurrency proof.
[Random functions](https://www.postgresql.org/docs/18/functions-math.html#FUNCTIONS-MATH-RANDOM-TABLE)
provide the installed uniform draw.
[JSON types](https://www.postgresql.org/docs/18/datatype-json.html) and
[collation](https://www.postgresql.org/docs/18/collation.html) inform conversion,
normalization, NUL refusal and exact text equality.

Resolved primary source inspected locally: sqlx-core 0.9.0
`src/connection.rs::is_in_transaction`; repository
`infra-postgres/src/transaction.rs::in_tx_with`; opentelemetry 0.32.0
`propagation/text_map_propagator.rs`; opentelemetry_sdk 0.32.1
`propagation/trace_context.rs` (malformed-tracestate default); and
`infra-telemetry/src/traces.rs::install`. No upstream API upgrade is inferred.
