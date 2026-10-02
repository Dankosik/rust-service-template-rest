# Technical Design: bounded and recoverable infra-jobs

Status: ready

Authority: [Intent](../intent.md), [Specification](../spec.md), and
[Definition transition](../definition-transition.md). Source baseline:
`67be869acea112af271ec8ba621cbc50ae9d36b7`. This design changes no accepted
behavior and authorizes no live queue operation. [Ownership](ownership.md)
and [rollout](rollout.md) are part of the same candidate.

The selected implementation foundation is PR #225 at immutable source
`a04b699f744038bf9b529bdee89eda76c00f59a6` (implementation commit
`ebb88eadce1b3cb30cde1686b8927e7a78d7bc95`). Its descendant
`5a683be7098fdba4981afffd774f59fed40145ed` is also admitted: the only delta is
an equivalent `let else` in operator SQLSTATE extraction and the historical
webhook-row comparison accounting for the new empty archive column. Use Git
objects, never the concurrently changing parallel checkout. This is adoption
of available source, not its validation or a claim that it is already landed.

Keep that implementation's covered B1–B6 mechanisms and names. The remaining
required delta is limited to:

| Delta | Missing foundation behavior | Selected correction and owner |
| --- | --- | --- |
| D1 | Batch SQL vectors and remaining entries can survive an earlier reply. | `attempt.rs` retires the complete batch's entry/custody state and SQL buffers before any success/failure reply or cancellation-induced reply closure can wake another thread. |
| D2 | Late `beside` membership can leave a fresh timestamp for an incomplete union. | `engine.rs` and `maintenance.rs` initialize new series and invalidate freshness under the peer mutex, then compare captured membership and publish under that same mutex. |
| D3 | Inspection permits a read-only/replica session despite the selected canonical-queue admission. | All operator modes use the same UTF8/writable/READ COMMITTED startup check before beginning a read-only or mutating transaction; remove the optional-writability bypass introduced solely for this path. |
| D4 | Recovery lock/update/delete lacks the read path's local two-second limit. | `infra-jobs::operator` sets the existing fixed `SET LOCAL statement_timeout` before every mutation's first row lock. The caller's admitted pool and 12-second operation backstop remain. |
| D5 | Executed command JSON lacks versioned schema and declared-kind context. | `jobs-worker::operator` emits `schema_version: 1` and the admitted normalized `handled_kinds` set on unhandled results, including failure results after argument admission; the adapter exposes its validated set read-only instead of reparsing it. |
| D6 | Runbooks do not fully state indefinite replay, restore-token invalidation and retained retry/poison limits. | Update existing jobs/outbox/architecture guidance from B4/B6 and the rollout below, with only missing material regression proof selected by Implementation. |

Schema-history representation, public provisional transaction API, CLI spelling,
500-row/1024-kind bounds and RFC3339 timestamps below adopt the foundation as
behaviorally equivalent. They do not reopen Definition or require rebuilding
covered queue/recovery/config/profile code.

## Selected mechanism and alternatives

Keep the existing PostgreSQL queue, Tokio supervisors, completion batching,
`infra_postgres::in_tx` boundary, and process lifecycle. Reuse the foundation's operator module
in `infra-jobs`, `jobs-worker` CLI/operator modules, and jobs-specific
configuration projection using the current loader. No new crate, framework,
daemon, HTTP surface, or configuration knob is needed.

| Decision | Decisive constraint and selected mechanism | Alternative disposition and cost | Reopen condition |
| --- | --- | --- | --- |
| Bounded completion custody | One admission permit pair covers a supervisor and any completion entry until both end; cancellation removes queued work, and in-flight batches retain custody. | Keeping only the supervisor's permit leaves abandoned entries outside the bound. Removing batching is simpler but discards the existing measured group-commit benefit; retain its one-writer mechanism and fix ownership. The intentional cost is backpressure during slow persistence and a small shared ownership guard. | A bounded implementation cannot preserve batch cancellation/deadline semantics, or measured cost justifies removing batching. |
| Recovery fence | Reuse `background_jobs_claim_generation` and advance `claim_generation` on redrive as well as claim. Archive the preceding failed cycle under its old version in `recovery_history`. | A second version sequence/table duplicates an existing non-reuse authority. Resetting attempts without advancing generation permits stale recovery; timestamps or `xmin` do not provide the required identity. | Another writer bypasses generation allocation or recovery needs an external restore incarnation. |
| Failure history | Atomically append the entire prior failed cycle, including `errors`, into same-row `recovery_history`, then reset active-cycle `errors`. | Clearing without archiving loses evidence. The previously selected per-error recovery-generation tag is equivalent but would rebuild working code; a second table/store is unnecessary. Same-row JSONB growth and rewrite cost remain accepted limits. | Actual retained history size prevents accepted operation within the database budgets. |
| Inspection | Fixed-size primary-key windows, then metadata-only filtering. | `WHERE kind NOT IN (...) LIMIT n` can search an arbitrary number of nonmatching live rows. Offset pagination increases skipped work. A fleet registry/scheduled anti-join is outside scope. Window traversal may return sparse or empty pages. | A measured need for faster fleet inspection justifies another index/query owner. |
| Recovery transaction | The adapter locks one id, compares kind/state/version and applies one fenced update/delete on the caller's opaque `Tx`; jobs-worker owns `in_tx` and acknowledged finality. The existing unique index arbitrates live-key races. | Preflight uniqueness is not arbitration; re-enqueue changes identity. The available CLI is a concrete caller for the already implemented provisional API, so a replacement pool-owning API has no current benefit. | A new caller cannot preserve transaction failure propagation, finality or input admission. |
| CLI/config | Clap's existing derive/flatten/subcommand support plus the existing loader's typed projection seam. | Hand-written argv parsing duplicates installed Clap. Full `Config` admission unnecessarily demands provider secrets. `MigrationConfig` additionally loads telemetry configuration that this command does not use. | A required operator behavior needs more configuration than PostgreSQL. |
| Observation | Only the process-duty engine samples the complete peer-kind union in one SQL observation. | Per-engine timestamps require a new metric contract; multiple writers to the current unlabeled freshness cannot represent a complete observation. | A supported process hosts unrelated `Engine::new` trees rather than one tree using `beside`. |

The queue-library survey already adopted by
[Async Architecture](../../../docs/architecture/async.md#reopen-conditions-and-watch-list)
remains applicable: transaction-local enqueue, static captured deadlines,
generation-fenced outcomes, schema ownership, and integrated teardown remain
the constraints. This correction uses existing supported primitives rather than
introducing a general queue mechanism. Bounded mechanism evidence and exact
resolved versions are in [supporting evidence](../research/mechanism-evidence.md).

## Full attempt and completion lifetime

The claim owner continues reserving engine permits before invoking CLAIM and
per-kind permits before dispatch. Each dispatched attempt wraps its existing
engine `OwnedSemaphorePermit` and optional `KindSlot` in one private
reference-counted custody value in `attempt.rs`. The supervisor holds one
reference until `persist` finishes or its captured local/cleanup deadline ends.
The early `drop(slots)` is removed. Direct retry/fail/snooze/release writes need
no extra owner and never free the capacity merely because the handler stopped.

For COMPLETE, the existing completion queue holds at most one entry per
`(id, generation)` attempt. Each entry carries a reference to the same custody,
its captured local deadline, and its existing reply sender. A scoped queue
registration removes that exact queued entry on cancellation/drop under the
short synchronous queue mutex. The queue mutex never crosses an await. Moving
an entry into the one in-flight batch transfers ownership; cancellation cannot
remove a moved entry, so the batch's custody reference holds its permits until
the entry itself is dropped. The writer task is still one of the tracked
supervisors; no detached completer or second task registry is introduced.

Before issuing a batch, prune closed/expired queued entries and send no new SQL
for them. The batch's acquire and statement acknowledgement are bounded by the
earliest participating immutable local deadline, the existing operation
backstop, and the dynamically observed cleanup deadline. All paths that drop
the write future also drop its entire batch and replies. A waiter whose entry
was in flight may end with uncertainty while the writer is cancelling; its
capacity cannot be reused before that entry is gone. One expired participant
may cause still-valid participants to retry, but never extends its deadline.

The next registration for an attempt begins only after its preceding entry has
been consumed/dropped; a failed batch drops all entries before answering retry.
The batch owner first extracts terminal reply handles/outcomes, then disposes
all entries/custody references and all SQL vectors/results for that finished or
abandoned batch. Only after that complete retirement may it send a success or
failure reply, or drop a sender on cancellation. This ordering also applies to
CompletionBatch's drop path; per-entry disposal before its own reply alone is
insufficient. The waiting supervisors still hold their custody references. Merely avoiding an
await between sending a reply and dropping the entry is insufficient: the
awakened waiter may run on another Tokio worker thread. Retry remains the
existing one-second cadence. A cancellation
before writer-lock acquisition removes the queue entry immediately; writer
cancellation drops the batch and unlocks the writer. These paths close the
otherwise accumulating abandoned waiter/batch entries.

The structural count is at most N distinct custody values per engine, each
covering one reservation/supervisor and at most one queued or in-flight
completion entry. Queue plus batch entries are at most N; their SQL arrays and
replies are O(N), not a second capacity pool. The publication engine retains
its separate single permit. Completion guards hold no payload or error string.
All tracked writers and supervisors finish under the same process cleanup
deadline. Acknowledged zero-row outcomes remain `unchanged`; no readback is
added to attribute them. Known results still win exactly as `drive` currently
specifies; uncertainty never becomes a forced release after a result is known.

`complete_in_tx` remains generation-fenced on the caller's existing `Tx` and
does not release supervisor capacity. Its caller must propagate stale failure;
CommitUnknown never replays the business closure. A later ordinary outcome
can see unchanged after a committed COMPLETE, apply after rollback, or remain
unknown until lease expiry. The claim's acknowledgement-derived immutable
deadline, static timeout-plus-60-second lease, and shutdown grace are unchanged.

## Durable recovery identity and history

Adopt the foundation's transactional migration
`20261002150001_add_background_job_recovery_history.sql`: add
`recovery_history jsonb NOT NULL DEFAULT '[]'` without changing existing rows'
identity, payload or active `errors`. Its second nontransactional migration,
`20261002150002_index_failed_background_jobs.sql`, creates
`background_jobs_failed_kind` on `(kind, id)` with literal
`WHERE state = 'failed'`, concurrently. No alternative recovery-generation
column or failure-writer edits are required. Existing indexes retain their
access paths and the live unique-key index remains the arbitration owner.

The inspected version is `claim_generation`, rendered as a decimal string and
validated as a nonnegative signed 64-bit value. With id/kind it identifies a
recoverable failed incarnation, not every intermediate running/pending state.
CLAIM draws a fresh sequence value even on claim-time exhaustion. A successful
redrive advances that same noncycling sequence once; rolled-back/conflicting
allocations remain spent and exhaustion fails instead of wrapping/resetting.

The one redrive update appends the prior cycle into `recovery_history`, recording
its old version, attempts, failure reason, finished time, attempted-by identity,
error summary, complete `errors` array and redrive time. Only after preserving
those values in that same atomic statement does it set state pending, attempts
zero and not-before to database statement time, clear terminal/claim fields,
clear active-cycle `error_summary`/`errors`, and allocate the new version.
Id, kind, payload, unique key, enqueue time and trace carrier remain unchanged.
The next claim spends attempt 1. Existing failure/rescue writers continue
appending their ordinary entries to the current `errors`; prior cycles remain
intact and distinguishable in the archive. No archive truncation or automatic
failure deletion is introduced. Completed retention or explicit discard removes
the whole row. Archive growth/rewrite cost is accepted; reopen for evidence that
it prevents operation within the existing budgets.

Inspection returns only the archive count, not history/error bodies. The
foundation computes that count in PostgreSQL under the statement bound; no
unbounded JSON history is transferred into the operator process merely to
inspect metadata. This does not promise constant physical work for an arbitrary
archive size.
No recovery token promises fencing across an operator resetting a sequence or
restoring an earlier database history. Restore procedure must invalidate saved
operator commands/receipts, reconcile effects and re-inspect the restored queue.
This is the existing backup/restore authority boundary, not a runtime bypass.

## PostgreSQL-only operator entry and public contract

When jobs are retained, `jobs-worker::run` parses an optional operator
subcommand before calling `start` or invoking registration. Existing loader
flags are accepted before the subcommand; omitted subcommand uses the existing
full configuration, registration, worker startup, exit and shutdown path.
`--help` and parse failures retain Clap's 0/2 results; `--version` remains absent.
Only this binary gains the documented positional command exception; the service
and migrate binaries keep loader-only CLI behavior.

```text
jobs-worker [loader flags] inspect ID
jobs-worker [loader flags] failed [--after CURSOR] [--limit N]
jobs-worker [loader flags] unhandled --handled-kinds LIST [--after CURSOR] [--limit N]
jobs-worker [loader flags] redrive ID --kind KIND --version GENERATION
jobs-worker [loader flags] discard ID --kind KIND --version GENERATION
```

`--handled-kinds` is one explicit comma-separated value, with no whitespace
normalization, bounded to 1024 input names and 66,559 bytes before database I/O.
Names use the existing 1..64-byte kind grammar; duplicates are deduplicated in
a sorted set. An explicitly empty string means no handled kinds; omission is
a usage error. UUID ids are non-nil, and cursors are `v1:<canonical non-nil
UUID>`; both are validated before I/O. Limit defaults to 100 and accepts 1..500.
These are command-work bounds, not configuration knobs. Keep the same handled
set through a traversal; changing it requires restarting without a cursor.
Unhandled responses echo the validated normalized `handled_kinds` array,
including an empty array for the explicit empty set, never inferred registrations.
The outbox guide names `publish_domain_event` as required when the fleet handles
publication jobs.
The configuration owner adds `JobsOperatorConfig { postgres: PostgresConfig }`
and `load_jobs_operator(&LoadOptions)`. It uses the existing generic merge,
environment/secrets-directory namespace admission, all-file secret/addressable
key scan, precedence and value-free decoding. It ignores unrelated sections
without decoding them, just as the migration projection does, but rejects
invalid/unknown PostgreSQL keys. No missing broker/webhook/auth secret is
required. Malformed names, ambiguous names, file secrets and source read errors
still fail even in ignored sections. Composition requires PostgreSQL enabled
and its admitted DSN. Password-file admission and conflict/ambient libpq
refusal stay in `infra-postgres`.

The one-shot entry uses the foundation's explicit current-thread Tokio runtime,
installs existing signal streams before database I/O, opens an admitted pool limited to
one connection (within the configured maximum), then performs embedded-history
verification and the jobs UTF8/writable/READ COMMITTED check. Keep that pool-based check and the engine delegate; remove its optional
writability parameter because every operator command is admitted against the
canonical writable queue. The read transaction itself remains read-only. No
registry or engine is constructed to reuse admission. Use fixed application_name
`jobs-worker-operator`; no OTLP exporter, metrics recorder/listener, background
refresh, NATS connection or handler construction occurs. Password is read at
admission; this finite command does not start the long-running refresh task.

Each read or mutation uses one transaction with READ COMMITTED established by
the admitted pool. Inspection explicitly selects READ COMMITTED/read-only;
mutation retains the foundation's `in_tx` on that checked pool. Both have the
two-second local statement budget used by observation, including the mutation's
initial lock, within the actual existing 12-second operation backstop including
acquire/begin/commit. The former design incorrectly called that backstop five
seconds: five seconds belongs to startup checks. Retaining the source's 12
seconds preserves Definition's existing bounded-operation behavior and adds no
new SLO or total deadline. The operator
transaction sets a local statement timeout; ordinary pooled defaults remain
unchanged when it ends. Read operations use a read-only transaction after
canonical-writer admission. No command retries SQL automatically. Database
timeout means timeout/unavailable, never an empty result. A stop before the
action begins prevents it; a stop or client deadline during a mutation is
reported conservatively as unknown unless an acknowledged result is already
available. A ready acknowledged result wins over cancellation. Pool close uses
the existing five-second close ceiling and runtime shutdown uses the existing
one-second ceiling. No HTTP grace is loaded or invented for this one-shot path;
ordinary worker grace and exit mapping remain unchanged.

Each executed command emits one JSON document with `schema_version: 1` on stdout.
Clap help and pre-execution usage errors keep their existing presentation. Safe diagnostic codes
go to stderr without formatting SQLx error Display/source/DETAIL, configuration
values or stored errors. Every result includes `action` and `outcome`. An
inspected row includes id, kind, state, decimal-string version, attempts,
failure reason, recovery count, and created/not-before/claim-expiry/finished
times rendered by PostgreSQL as UTC RFC3339 strings with six fractional digits
(nullable where absent). The database `observed_at` uses the same format;
there is no floating-point timestamp conversion. No payload, unique key, trace carrier, attempted-by identity, history
body or credentials are returned. Build identity may accompany the document
from the existing compile-time worker `BUILD_INFO`, without new config inputs.

Inspect missing returns `outcome: missing` with exit 0. Successful read pages,
acknowledged redrive/discard return 0. Missing/stale/conflict on mutation,
known database failure, timeout, cancellation, unavailable admission, unknown
commit and output failure return 1 with distinct safe codes. Syntax/validation
failure before execution returns 2. An output failure after acknowledged commit
does not undo the action; documentation directs inspection of the same identity.

## Bounded inspection traversal

Adopt `infra_jobs::operator::{Inspection, RecoveryTarget, inspect, redrive,
discard}` over the caller's opaque `&mut Tx`. The adapter owns validated input,
fixed SQL, per-statement budget, safe snapshots and closed provisional outcomes.
It exposes read-only access to an Inspection's admitted handled-kind set so the
binary never reparses it for output. Jobs-worker owns transaction orchestration,
JSON/process policy, the 12-second whole-operation backstop and final receipt.
Public recovery results are explicitly provisional until caller commit; callers
must propagate adapter failure out of the transaction. The adapter never commits,
rolls back, opens another connection or runs a business handler.

Inspect is one primary-key lookup selecting only the safe columns. Both lists
first fetch at most the admitted limit of rows with `id > after`, in ascending
primary-key order, across all states, selecting only those same columns. The
first page has a separate fixed query without an after predicate, so no
sentinel UUID can skip a valid row and no nullable-OR plan is needed. Filter the
bounded returned window in Rust: failed state for `failed`; pending/running and
kind absent from the declared set for `unhandled`. This uses the existing SQLx
fixed-query macros and avoids a filter being pushed before a SQL limit.

The page contains matching `items`, `scanned`, `next_cursor` and `complete`.
If the window contained limit rows, `next_cursor` is `v1:` plus its last scanned id even
when none matched, and complete is false. Fewer rows means complete true and
no continuation; an exactly-full final window requires one empty final query.
There is no internal scan-until-match loop, offset, payload fetch, COUNT of the
whole table, or second lookahead. A failed page returns no new cursor and no
complete claim; retrying its original cursor remains a read-only action.

This bounds visible rows fetched to at most 500 and handled-set admission to
at most 1024 names/66,559 bytes per invocation. It does not establish
constant physical PostgreSQL work: index traversal, MVCC dead tuples, heap
visibility and a poor plan can cost more. The statement and client timeouts
bound waiting and fail explicitly; representative query-plan evidence belongs
to the existing database proof, without a new performance SLO. On unchanged
data, monotone primary-key continuation discovers every matching row. Concurrent
inserts before the cursor or changing states require a new traversal; pages do
not pretend to share a frozen snapshot or an authoritative fleet registry.

## Single-row recovery transaction and reconciliation

After input and canonical-session validation, jobs-worker begins `in_tx` on its
admitted READ COMMITTED pool. The adapter applies the two-second local statement
timeout before accessing the target, then locks at most the one primary-key row using `SELECT ... FOR UPDATE`; select only id/kind/state/
generation. Missing returns Missing. A different kind, generation, or a state
other than failed returns `OperatorError::Stale`. These decisions perform no write.
For an exact target, run one update (redrive) or delete (discard), still fenced
by id/kind/failed/generation as defense against future rearrangement. Require
exactly one affected row before returning a provisional success to `in_tx`.
Zero is stale/ineligible, never applied. Do not use SKIP LOCKED: a concurrent
action settles inside the statement budget or returns timeout, and a loser
observes the settled current row before comparison.

Redrive archives the old cycle, draws one new sequence value, makes the
transition described above and returns that generation provisionally. It does not precheck uniqueness. The existing
`background_jobs_live_unique_key` index arbitrates both another live row and a
concurrent enqueue/redrive; only SQLSTATE 23505 with that exact constraint name
maps to `OperatorError::Conflict`. The caller propagates that error so in_tx rolls back before reporting failure, leaving the failed job and history in custody. Other constraint or
database failures retain their safe failure class, without key values. Two
commands using one original token cannot both transition the row, and a stale
command can never reset a later failed cycle. Discard deletes only the locked
failed row; deletion of history is the explicit abandonment requested by that
action, never automatic retention.

The public adapter's Redriven/Discarded value is provisional; only an
acknowledged caller commit lets the CLI report redriven/discarded. Known rollback,
begin/acquire/statement errors are failures; `TxError::CommitUnknown` or an
unobserved mutation acknowledgement is Unknown. A receipt includes action, id,
kind, the expected version and the known outcome; acknowledged redrive also
includes the newly assigned generation. No unknown result silently refreshes a
token or retries the action. Inspect the same identity: unchanged exact failed
version permits an explicit original-token retry after reconciliation; another
generation/state means the old request is no longer eligible. Absence after
uncertain discard establishes absence, not attribution of who deleted it.

Redrive never invokes a handler, rebuilds an outbox envelope, republishes a
business transaction, or changes stored routing/format/id/time/bytes. Exact
prepared publication bytes inside the JSONB payload remain untouched. Effects
may predate failure or ambiguity. The operator must reconcile ordinary
non-idempotent effects; consumers need durable logical-id effect identity for
the actual replay/restore lifetime. Indefinite failed custody permits replay
beyond any finite broker window or consumer TTL; the template promises no
exactly-once effect. Discard documentation states potential permanent loss of
an unpublished event before showing its command example.

## Retention and consistent process observation

Remove failed deletion and its seven-day policy entirely. Both automatic
retention and public `Engine::remove_expired` call the same completed-only path;
24 hours, 500-row batches, SKIP LOCKED, cadence and cancellation/budgets stay.
No fallback deletion predicate retains the old policy. Failed rows reserve no
live unique key. All old retention owners must stop/upgrade before this
guarantee holds; already deleted rows are outside recovery.

The engine with `owns_process_duties` starts the sole sampling task beside its
existing listener/retention tasks. `beside` engines start only their claim task.
Snapshot the sorted deduplicated kind union from the existing shared peers
container and sample all kinds/states in one SQL statement with one database
timestamp. The sample adds a capped failed lookup on the new partial index and
publishes `jobs_failed_jobs{kind}` separately, preserving `jobs_live_jobs`'s live
state meanings. The 1000 cap, ten-second cadence, two-second statement timeout,
12-second operation backstop and 30-second stale threshold remain.

Init the process metrics once, with value zero and timestamp zero meaning
unobserved. Peer addition must invalidate timestamp to zero and initialize its
new kind series under the peer metadata mutex before that peer can claim.
At publication re-lock the same peer metadata and compare the captured union;
if it changed, discard the incomplete-for-current-membership sample and keep
freshness invalid until a new full-union success. Publish all decoded values,
then the timestamp last, while preventing a concurrent membership change. No
peer-local init/start or sample may renew timestamp independently. Engine
construction before any start remains the normal composition, with this rule
also closing a late `beside` call.

Any query/decode failure preserves the previous complete values and timestamp;
the existing operation-failure signal records it. A membership change is the
only deliberate invalidation to unobserved. Gauges are individually published
through the existing metrics facade, not an atomic multi-gauge snapshot API;
the timestamp is the completion marker for that full sample. Replica samples
are not additive. Only registered static names become metric labels; unknown
names appear solely in bounded operator output.

## Enforcement and proof boundaries

| Invariant | Enforcing owner and all affected bypass paths | Proving surface / nearest falsifier |
| --- | --- | --- |
| Full-attempt N bound | Claim reservation, supervisor custody, queue-registration drop and in-flight batch guard; direct outcome/transactional-complete paths retain supervisor ownership. | Existing jobs execution tests hold outcome writes while handlers finish, cancel queued waiters and batch writers, expire deadlines, and observe progress after release. Any N+1 local custody/entry or early permit reuse falsifies it. |
| Failed custody | Completed-only maintenance entry, including direct remove_expired; operator discard is the only new failed deletion. | Existing real-DB retention cases cover all states, old failed/unknown/outbox rows, cancellation and completed retention. |
| One eligible recovery | Adapter input checks, primary-key lock, state/kind/generation predicate, noncycling sequence, unique index, in_tx commit result. | Real DB stale repeat after later cycle, concurrent redrive/discard and enqueue, live-key conflict, known rollback and uncertain committed/rolled-back branches. |
| History and immutable payload | Redrive excludes payload/identity from mutation and atomically archives the complete prior errors/cycle before clearing active-cycle errors. | Reuse foundation DB proof of original archived errors, current-cycle entries, immutable payload/prepared bytes and next attempt 1; close any actual outbox proof gap through its existing integration owner. |
| No unrelated startup effect | Optional command branch before full load/registration; narrow loader and one admitted pool; no engine construction. | Config unit tests and worker process/real-DB operator path with invalid/missing unrelated secrets and unavailable broker, plus history/session refusal and payload-free errors. |
| Bounded complete traversal | Adapter validates 500-row/1024-kind admission, SQL selects one PK window, Rust filters, and v1 cursor uses the last scanned id. | DB sparse/empty pages, final exactly-full page, zero-match queue, malformed cursor and timeout; query plans support only bounded visible-window claim. |
| One complete observation | One process sampler, union query and timestamp-last publish; peer-add invalidation and union comparison. | Ordinary plus publisher process tests with failed/blocked observation, startup, last-good, successful empty sample and membership change; subset freshness cannot succeed independently. |
| Profile containment | Current initializer inventory and marker authority; jobs removal prunes operator modules, exports, deps, migrations and tests. | Existing profile projections and representative runtime graphs, with no repeated full build across unchanged harness dimensions. |

Implementation chooses concrete tests, controls and grouping. These are proof
owners/falsifiers, not a separate Test Design or per-task gate. Final delivery
uses normal build/tests plus existing selected DB/migration/SQLx/outbox/profile
CI routes and one assembled independent review. This document establishes no
candidate runtime, performance, CI or deployed result. Retained retry, poison,
lease, one-slot publisher and combined dependency/failure-domain dispositions
are exactly B6 and the Specification's audit table; no new SLO is inferred.
