# Specification: infra-jobs reliability

Status: ready

Requester meaning: [Intent](intent.md). Fixed source baseline:
`67be869acea112af271ec8ba621cbc50ae9d36b7` on
`codex/infra-jobs-reliability-20261002`.

## Outcome and authority

Keep every admitted attempt bounded through its complete local responsibility,
retain unresolved failed work, and provide safe operator inspection and recovery.
The output is one separate PR containing the coherent correction and its proof.
Merge, deployment, and operating on a live queue are outside execution authority.

This Definition owns behavior. Technical Design owns public Rust types, command
syntax, placement, storage representation, migration needs, and query plans.
Implementation chooses concrete tests under the existing final-validation owner.

## Current evidence and audit disposition

These are static source observations, not a claim of a reproduced production
incident or measured performance for this candidate.

| Audit point | Disposition and reason | Reopen condition |
| --- | --- | --- |
| `run_attempt` drops handler slots before `persist`; `Completions.queued` is an unbounded vector | Change: configured engine capacity must cover local attempts and all associated completion bookkeeping until they end; an outcome-write outage must backpressure claims. | Any path retains attempt-related work after returning its admission capacity. |
| Failed rows are deleted after seven days; no supported redrive exists | Change: failed rows remain unresolved durable work until explicit recovery or discard. Provide version-fenced operator actions, including immutable outbox replay. | An adopter supplies a mandatory retention or erasure policy. |
| Ordinary and publisher engines sample disjoint kind sets but publish one unlabeled freshness timestamp | Change: one consistent process observation covers the registered kind union; success for one subset cannot renew freshness for another subset whose observation failed. | A different supported process composition requires a new observation scope. |
| Quartic retries can span weeks; incompatible payload decoding consumes the kind attempt budget | Retain the retry mechanism and defaults; document the horizon and compatible-kind/payload restoration before recovery. No accepted delivery deadline or measured failure justifies silently shortening retries. | An adopter supplies a delivery deadline or evidence shows the retained policy prevents its accepted outcome. |
| Static lease is handler timeout plus 60 seconds, without heartbeat | Retain. Crash rescue can take the original remaining lease, up to approximately 61 minutes at the one-hour handler limit, plus scheduling/database availability delay. No faster recovery SLO was supplied; heartbeat would reopen established ownership and failure semantics. | Changed availability requirement or measured unacceptable rescue delay. |
| Publisher has one dedicated slot per process | Retain. It isolates publication admission from ordinary slots; raising `jobs.max_workers` does not increase publisher concurrency. Multiple compatible worker replicas supply additional publisher slots, subject to database and broker capacity. No throughput or ordering SLO is promised. | Measured publication backlog/latency cannot be met by the accepted deployment capacity. |
| Combined worker admits NATS before ordinary claims and shares process failure | Retain fail-closed admission and one failure/shutdown domain. Do not claim independent ordinary-job availability during broker startup failure or fatal consumer failure. | An adopter requires independent availability or deployment scaling. |
| Unknown kinds are unclaimed and absent from registered-kind samples | Change operator inspection: live jobs outside an explicitly declared handled-kind set must be discoverable through a bounded, complete-page traversal. Preserve registered-kind metrics and never consume, rename, or delete unknown work automatically. | Fleet registration cannot be represented by that declared set, or proactive fleet-wide detection becomes an accepted requirement. |

Source owners:
[async architecture](../../docs/architecture/async.md),
[jobs guide](../../docs/background-jobs.md),
[outbox guide](../../docs/postgres-transactional-outbox.md),
[worker lifecycle](../../docs/architecture/runtime-lifecycle.md#jobs-worker),
[persistence](../../docs/architecture/persistence.md).
Primary code is `crates/infra-jobs/src/{attempt,claim,maintenance,engine,kind}.rs`
and `crates/jobs-worker/src/bootstrap.rs`; retention constants are 24 hours for
completed and seven days for failed at this baseline. The observation defect is visible in `Engine::start` /
`Engine::beside` and `maintenance::{sample_once,publish_sample}`: both engines
start sampling, each query uses its own registry, and both update the same
unlabeled `jobs_observation_timestamp_seconds`.

## B1. Bound the entire attempt responsibility

For an engine with admitted capacity N, no more than N claimed attempts may
remain locally owned at once, including handler execution, known results waiting
for an outcome acknowledgement, persistence retries, and forced cleanup.
Reservations for an invoked claim are part of that capacity. Each attempt's
pending or in-flight completion entry remains covered by that same bound;
cancelled waiters or abandoned batch entries must not accumulate across slot
reuse. There is no separate unbounded overflow behind the bound.

Capacity becomes available only when that attempt's local responsibility and
bookkeeping have ended: an applied or unchanged acknowledgement, or expiry of
the existing local/cleanup deadline with explicit uncertainty and recovery left
to the lease. A stopped handler alone does not make capacity available. This
does not require claiming that a zero-row write succeeded or belongs to another
worker; it remains `unchanged` with no durable attribution.

When all capacity is occupied, further due work remains durable and unclaimed.
It spends no attempt and allocates no new supervisor/completion entry. Bounded
per-kind admission continues to hold, as do the reserved publisher's independent
capacity and the worker's shared management resources and shutdown deadline.
The change may reduce admission throughput when outcome writes are slow; that
backpressure is the intended correction and no performance percentage is claimed.

Known handler results keep precedence over timeout and forced release. Error,
retry, snooze, cancellation, transactional completion, and stale-generation
semantics remain those of the existing async architecture. Never extend an
immutable deadline merely to preserve a result or free a slot early by losing it.

Nearest falsifier: hold outcome persistence while many fast handlers and due
jobs are available; locally owned attempts and completion entries must remain
within N, then make progress after persistence resumes. Exercise deadline and
forced-cleanup cancellation so old queued entries cannot evade that bound.

## B2. Failed work remains in custody

Automatic retention deletes only completed jobs after the existing 24-hour
interval, with existing batch/time/cancellation limits. It never deletes failed,
pending, or running jobs, regardless of age, registration, failure reason, or
whether the kind is the reserved outbox publisher. Existing failed rows gain
this protection when the corrected retention owner is running; already deleted
rows cannot be recovered by this change.

Failed is terminal for automatic execution but unresolved for operator custody.
It remains excluded from normal claims. Both `permanent` and `exhausted` may be
explicitly redriven after the operator has corrected or accepted the cause, or
explicitly discarded. No automatic recovery loop repeatedly resets attempt
budgets. Retained rows and their history consume storage until resolved; the
runbook must state this and provide discovery and explicit cleanup.

Completed retention stays unchanged after a redriven job eventually succeeds.
Live unique-key uniqueness stays limited to pending/running rows; retaining a
failed row does not reserve its former live unique-key slot.

Nearest falsifier: retention over old completed, failed, pending, running, and
unknown-kind rows removes only eligible completed rows, including the outbox
failure case.

## B3. Supported operator inspection and single-job recovery

Ship local operator commands through the jobs-worker deliverable for read-only
inspection, redrive, and discard. Running the worker without an operator action
preserves ordinary startup. These commands use admitted PostgreSQL credentials
and configuration sources and do not expose an HTTP/RPC administration surface.
They must work without connecting NATS, registering/running handlers, starting
claim/retention loops, binding listeners, or requiring unrelated provider secrets.
They still enforce the applicable PostgreSQL connection, schema-history, and
secret-redaction policies. Technical Design selects the smallest compatible
command and configuration projection, documenting its intentional CLI delta.

Inspection supplies stable job identity, kind, current state, a non-reusable
state/version token, attempt count, failure reason, and relevant queue/terminal
times. Default output excludes payload, unique keys, trace carriers, arbitrary
stored error text, and credentials. Operator-supplied ids and version tokens
are validated before mutation; output and diagnostics stay payload-free.

The command can inspect one id, page through failed rows, or page through live
rows outside a declared handled-kind set. For unhandled inspection the operator
must supply that set explicitly; it represents the intended fleet, not an
inference from one process. A combined deployment's set includes the reserved
publication kind. Unknown is relative to that set and is never evidence that
no other deployment handles a row. Omission is a usage error, not an empty-set
assumption. An explicitly empty set is allowed and means no kinds are handled.

Every page has a hard row/work bound and a resumable cursor; a page may contain
no matches while still returning a continuation. The response distinguishes a
completed traversal from a partial page, unavailable observation, or a database
timeout. A timeout never means an empty queue. A full traversal over unchanged
data can discover every matching row. Concurrent queue changes make inspection
a current observation, not a frozen fleet-wide snapshot. Exact bounds and the
cursor representation belong to Technical Design; no unbounded dynamic metric
labels or every-ten-second fleet-wide anti-join are required.

Redrive and discard each target exactly one inspected failed row by id, kind,
and version token. There is no wildcard, bulk, age-based, payload-editing,
kind-renaming, or force-bypass mutation in this correction. Successful redrive
atomically makes that same job pending and due at database time, clears terminal
and claim state, and grants a fresh attempt budget under its eventual registered
policy. The next handler sees attempt 1. It preserves job id, kind, payload,
unique key, enqueue time, and trace carrier. Earlier failure history remains
available and distinguishable from the new recovery cycle; recovery must not
silently replace the original evidence. Each successful recovery changes the
version so that the request cannot be reused against a later failed cycle.

Recovery outcomes are closed:

| Observed condition | Required result/effect |
| --- | --- |
| Exact failed row/version, redrive admissible | One atomic failed-to-pending transition; no business handler executes in the command. |
| Exact failed row/version, discard requested | Delete that failed row only, after an explicit discard invocation; report acknowledged deletion. |
| Target does not exist | Report missing; no mutation and no claim that this invocation deleted it. |
| Kind/version differs or row is pending/running/completed | Report stale or ineligible; leave it untouched. |
| Another live row occupies the same kind/unique key during redrive | Report conflict; leave the failed identity retained. Never merge payloads, override the live row, clear its key, or create another job identity. |
| Concurrent actions on the same inspected version | At most one transition; every loser is unchanged/rejected, with no reset of the winner's next attempt cycle. |
| Known database failure | Report failure without claiming a durable transition; retain ordinary rollback semantics. |
| Commit or acknowledgement cannot be established | Report unknown and instruct inspection of the same identity; never silently retry with a refreshed version or claim success. Reusing the original token cannot redrive another cycle. |

A successful command result means the database commit was acknowledged, not
that a handler ran, the broker accepted a publication, or a consumer applied an
effect. An adapter-level result inside a caller transaction, if exposed, remains
provisional until that caller commits. A non-successful command has a nonzero
process result and a distinct safe outcome for operators; inspection of a
missing id may report missing normally without implying a mutation.

Discard is an explicit abandonment of unresolved work and may permanently lose
an unpublished event. Its command documentation must say that before the usage
example. The implementation supplies the action; this task does not execute it
against any live database. A payload-free receipt identifies action, job, kind,
version, and known/unknown result. Missing after an uncertain discard is absence,
not attribution of who deleted it.

Nearest falsifiers: stale repeated redrive after a later failed cycle; concurrent
redrive/discard; live-key collision including a concurrent enqueue; terminal
completion versus stale recovery; and both committed and rolled-back outcomes
when acknowledgement is unknown. Missing/invalid targets cannot cause effects.

## B4. Preserve outbox and external-effect identity

Redrive does not rebuild an event or replay its business transaction. The stored
route, logical id, publication id, format version, occurrence time, and exact
prepared bytes remain identical; malformed intent may fail again and is never
silently repaired. Restoring a compatible handler or producer configuration is
an operator prerequisite outside the recovery command's authority.

Delivery remains at least once. An effect may already exist before a failed
attempt, ambiguous acknowledgement, or an explicit redrive. Consumer logical-id
deduplication must cover the actual retention, backup/restore, and manual replay
horizon; the broker's finite duplicate window is insufficient. Because failed
custody and permitted manual redrive have no automatic expiry, no finite
consumer deduplication TTL alone guarantees duplicate suppression for every
allowed replay. Adopters must retain durable logical-id effect identity for
the full permitted replay lifetime, or reconcile and explicitly constrain
replay before expiring that identity; the template promises no exactly-once
effects. Operators must
reconcile non-idempotent ordinary-handler effects before redrive. Neither an
unchanged completion write nor a redrive receipt proves that an external effect
did or did not happen. Existing fenced `complete_in_tx` and CommitUnknown rules
remain unchanged; stale transactions cannot commit their preceding business
effects and an uncertain business closure is not replayed by recovery.

Nearest falsifier: a failed/ambiguous publication is redriven with the exact same
stored identity and bytes; known completion is still required before terminal
success, and redrive never re-enqueues the business operation under a new id.

## B5. Visibility and operator guidance compose with retention

Add a capped failed-depth observation for registered kinds with the existing
1000-row meaning and sample freshness policy. A successful empty sample gives
zero; startup or failed observation does not pretend to have observed zero.
Publish values and freshness consistently across the complete registered-kind
union of the process, including the ordinary and reserved publisher engines.
A successful sample for one engine must never advance freshness for stale
values of another. Advance the shared freshness only after a complete
consistent observation; on any incomplete or failed observation preserve the
last-good complete values and timestamp. Startup remains unobserved until that
complete sample succeeds. Preserve the existing 10-second sampling cadence,
1000-row cap per kind/state, bounded database operation, and 30-second stale
threshold; do not add a separate observer service or arbitrary kind labels.
Document that replica samples are not additive. This enables alerting on
unresolved failure accumulation without exposing arbitrary kind labels.

Unhandled inspection supplies the complementary on-demand view, including old
kind names after a rename and the declared publisher name. It never claims,
fails, redrives, renames, or deletes those rows. The runbook must explain how to
restore a compatible handler or perform a separately reviewed data conversion;
simply renaming a handler is not a queue migration. Registered live metrics,
failure counters, freshness rejection at zero or older than 30 seconds, and
their failure interpretation remain unchanged.

Nearest falsifier: a queue containing only failed and unhandled work cannot be
represented as a confirmed healthy empty queue by the documented operations.
An observation timeout is visible, and continuation through nonmatching pages
eventually exposes the matching rows in an unchanged queue. With ordinary and
publisher engines together, a stalled/failed observation affecting one subset
cannot be hidden by continuing success for the other subset.

## B6. Retained retry, poison, and restore behavior

Keep the current `attempt^4` seconds with independent +/-10% jitter, the
kind-specific attempt cap (default/max 25; outbound webhook policy 20), and
`retry_after_at_least` as a lower bound. With immediate failures and no extra
floor, queueing, downtime, or handler cost, nominal delays before the final
attempt sum to `sum(a^4, a=1..24) = 1,763,020 seconds` (about 20.4 days)
and `sum(a^4, a=1..19) = 562,666 seconds` (about 6.51 days) respectively.
The corresponding jitter-only range is +/-10%; these are arithmetic policy
illustrations, not a delivery bound. Handler time, retry floors, outage,
backpressure, and scheduling can lengthen the horizon.

Malformed or incompatible stored payloads keep the existing retry/exhaustion
classification and sanitized failure behavior. Do not automatically rewrite
payloads, rename kinds, or silently reinterpret a stored outbox intent. The
runbook must make rolling-version compatibility concrete: retain a handler
that understands outstanding kind/payload versions across deployment and
backup restore, inspect stranded/failed identities, restore compatible code
or perform a separately reviewed conversion, reconcile possible external
effects, then use the supported one-row recovery. Retention is not a queue
migration and redrive does not repair a poison payload.

Nearest falsifier: guidance that treats a finite broker duplicate window as
covering all allowed retries/redrives, treats a handler rename as migration,
or promises completion within the nominal sum would violate this contract.

## Compatibility, rollout, and proof boundary

Preserve current default worker registration, process exit/shutdown semantics,
transactional enqueue and duplicate comparisons, SKIP LOCKED selection,
generation-fenced outcomes, kind timeouts/retries, caller-transaction completion,
template profile removal, and completed-row retention. Changes must remain
present and buildable in each affected existing optional-profile representative;
do not multiply the entire validation matrix across unchanged dimensions.

Old workers still delete failed rows after seven days. Therefore the new failed
custody guarantee starts only after every old retention owner on that database
is stopped/upgraded. Rollout instructions must explicitly drain/replace them
before depending on the guarantee. A rollback to an old retention owner requires
protecting unresolved failures first or stopping that owner; it is not a safe
transparent rollback. No migration rewrite or silent runtime schema repair is
authorized. Technical Design must close any append-only migration and mixed-
version admission requirements before Implementation.

Use existing tests and add only missing material cases corresponding to the
falsifiers above. Ordinary build/test completion follows the repository budget;
real PostgreSQL, migration, outbox, and template-profile claims follow their
existing validation/CI owners. In particular, pure mocks cannot establish a
claim about transactional rollback, uniqueness races, or stale fencing. No
production observation, benchmark SLO, new environment, or duplicate full-build
matrix is added by this specification. The final assembled delivery review is
required for the changed concurrency and durable recovery behavior.

## Composition check and ready condition

In a broker outage, the combined worker keeps its existing admission/failure
behavior. Existing running attempts still spend bounded local capacity while
outcomes are uncertain. A publisher that exhausts remains failed beyond seven
days and raises the failed-depth signal. The operator can inspect it through
PostgreSQL alone, reconcile possible publication, restore its compatible owner,
and redrive the exact identity once. A repeated stale recovery request cannot
reset it again; a competing live key leaves the failed intent intact. A kind
removed during deployment remains durable and discoverable through unhandled
inspection; no auto-delete conceals it. All of this holds only after old
retention owners have been removed from the database's worker fleet.

## Adopted evidence

B1-B4, the operator-inspection and failed-depth parts of B5, and the retained
lease/publisher/combined-failure-domain decisions adopt the reviewed ready
`jobs-worker-reliability` Definition from the parallel checkout at the same
source baseline. That work is read-only evidence, not execution authority or
a shared writable candidate. The local Intent and this Specification are the
authority for this branch. The local review receipt records original hashes
and its independent PASS, preserves that evidence for unchanged semantic
scope, and reviews the explicit process-observation and retry/restore deltas.
Technical Design must evaluate existing supported mechanisms before selecting
any new CLI, storage, or coordination mechanism; no dependency admission or
implementation placement is decided here.

Definition is ready when independent Specification Review finds no surviving
material divergence. Reopen Specification for changed behavior/retention or
effect identity, Intake for changed requester meaning/authority, and Technical
Design for mechanism, placement, resource bounds, or rollout details that do
not alter this behavior contract.
