# Technical design: bounded custody and recoverable jobs

Status: ready

Authority: [ready behavior](../spec.md), [intent](../intent.md), and
[Definition result](../definition-result.md). Source baseline:
`67be869acea112af271ec8ba621cbc50ae9d36b7`. This design changes no behavior
disposition in B1–B5. The retained static lease, single publication slot and
combined worker failure domain remain explicit limitations. Supporting
[mechanism evidence](../research/mechanisms.md), [ownership](ownership.md), and
[rollout](../rollout.md) close the implementation inputs. No runtime proof was
run during this phase.

## 1. Full attempt custody and completion cancellation

Keep the existing claim reservation, Tokio semaphores, TaskTracker, immutable
local deadline, forced-cleanup deadline, completion batch and fenced SQL. Do
not add a dispatcher, completion task, channel or second queue. The existing
`run_attempt` release immediately after the handler ends is removed.

Each dispatched attempt has one private `Arc<AttemptSlots>` containing its
owned global permit and optional per-kind `KindSlot`. The supervisor owns one
reference until `persist` returns or the attempt ends as uncertain. A queued or
in-flight completion entry holds a reference to the same object. Thus the last
reference releases capacity, and every piece of retained completion work still
has its original admission reservation. Direct non-completion writes retain
the supervisor's reference throughout their write/retry wait. Claim-in-flight
reservations continue to occupy the same semaphore before dispatch. No change
to claim acknowledgement or stop-during-claim rules is required.

The completion entry also carries its immutable local deadline. A small
private registration guard belongs to the `complete_batched` future and removes
its entry from the queued vector synchronously on drop. Its key is the unique
`(job id, claim generation)`; a supervisor registers at most one entry at a
time. Queue mutation remains under the existing short `std::sync::Mutex`, with
no await inside it. When the existing writer takes a batch, ownership moves
from that vector to its local batch; the guard may then find no queued entry.
The batch retains the slot references until its SQL future and all reply
senders are dropped. This closes the race where another supervisor's deadline
ends while its entry is in an in-flight batch.

Before starting a batch write, discard entries whose receiver closed or local
deadline expired. Do not start an outcome for an expired entry. Run the single
batch SQL through the existing operation backstop (12 seconds), bounded also
by the earliest included entry's local deadline and the shared cleanup
deadline. A writer cancelled while waiting for the mutex unregisters its queued
entry. A writer cancelled during SQL drops its owned batch and reply senders;
live waiters observe the closed reply and retry only within their original
deadlines. A receiver cancelled while another writer is sending does not allow
its admission reference to escape that batch. Guard and batch cleanup precede
the last permit release; no async cleanup or detached work is introduced.

Retire each in-flight entry's batch membership and admission reference before
sending or dropping its reply sender. An awakened waiter may immediately
register its retry on another runtime thread, so reply visibility must not
precede retirement of the previous entry. The same ordering applies in the
batch's cancellation/drop path. A private batch-drop guard may enforce that
ordering; it owns no task or second queue.

When all N permits are occupied, the claim loop cannot reserve more and
allocates no further supervisor or completion entry. The distinct-attempt
bound is N; the sum of queued and in-flight entries is at most N because each
attempt has one registration and no retry starts before its previous batch
reply closes. Arrays derived for the active SQL batch contain at most N
elements and disappear with it. Slots are shared references, never copied
permits. At most one writer exists. The publisher engine applies the same
invariant with N=1, separately from ordinary capacity. Per-kind slots remain
held through bookkeeping too.

Applied and unchanged acknowledgements finish responsibility. On deadline or
forced-cleanup expiry, the existing unknown result is recorded; no known
handler result is replaced by release and no deadline is extended. A batch's
earliest deadline can make other members retry earlier; this is the accepted
cost of retaining batching without keeping expired work. A database command
may have committed before its client future is dropped; generation fencing and
lease recovery retain that uncertainty semantics. The database's session
timeout remains the backstop for server work after client cancellation.

Reuse choice: existing batching plus standard `Arc`/RAII/oneshot, rather than a
bounded MPSC completer or removal of batching. A bounded channel alone cannot
bound expired entries retained by its writer, and adds another lifecycle;
removing batching discards an already supported throughput mechanism. Reopen
only if implementation demonstrates the described single-registration/drop
ordering cannot be enforced or measured batching cost warrants a separate
change. No throughput gain or percentage is claimed.

## 2. Retained failures, versions and recovery history

Automatic retention becomes one completed-only path: the existing 24-hour age,
500-row `SKIP LOCKED` batches, one-second statement limit, 12-second operation
backstop, process management permit and cancellation token remain. Remove the
failed-retention duration and failed branch; no compatibility branch may still
delete failed jobs. Failed rows do not participate in normal claims or reserve
a live unique key.

Use the existing `claim_generation` as the non-reusable operator version.
Every claim already draws from `background_jobs_claim_generation`. A successful
redrive draws from that same non-cycling sequence; do not increment a row-local
counter, reset a generation, use timestamps as tokens, or reuse a rolled-back
token. Inspection renders the nonnegative bigint as a decimal string, so JSON
clients cannot round it. Operator mutation accepts only a nonnegative decimal
value fitting i64, without signs/whitespace; the locked state check, rather
than token 0 alone, decides whether a row is eligible. Sequence exhaustion is
a database failure leaving the failed row intact. Restores must preserve the
sequence above the maximum persisted generation; resetting it is outside this
feature and invalidates stale-token guarantees.

Append one migration adding `recovery_history jsonb NOT NULL DEFAULT '[]'` to
`background_jobs`. It is an additive constant-default column; do not rewrite
the applied create or errors migrations. The only writer of this array is
redrive. Each element archives the just-ended cycle with these exact fields:
`version` (previous claim generation as JSON string), `attempts`,
`failure_reason`, `finished_at`, `attempted_by`, `error_summary`, `errors`, and
`redriven_at`. Times are database timestamps serialized by PostgreSQL. Existing
`errors` retains its current per-spent-attempt representation inside each
cycle, so no failure/rescue writer needs a new error format. The array order
is recovery order; the archived generation identifies the prior cycle and the
new row generation separates the next one. No payload, unique key or trace
carrier is copied into history.

On redrive, one fenced UPDATE appends this archive and sets `state='pending'`,
`not_before=statement_timestamp()`, `attempts=0`,
`claim_generation=nextval('background_jobs_claim_generation')`,
`claim_expires_at=NULL`, `attempted_by=NULL`, `finished_at=NULL`,
`failure_reason=NULL`, `error_summary=NULL`, and `errors='[]'`. Preserve id,
kind, payload, unique key, created_at and both trace fields byte-for-byte at
their existing PostgreSQL representation. The next registered handler claim
spends attempt 1 under its current policy. This is not enqueue and sends no
new producer/publication operation. The ordinary one-second poll is sufficient
for pickup; no recovery-specific NOTIFY protocol is added.

The history stays with the row through later failure cycles and is removed
only with explicit discard or the existing retention after eventual success.
It is not a permanent audit log. Default inspection reports `recovery_count`
only, using `jsonb_array_length`; arbitrary error history stays out of stdout.
Authorized database inspection can examine the named archive column and the
current errors when an incident requires it. No new history-export command is
needed by the accepted contract. Durable history can grow over repeated manual
redrives; reads/writes have time backstops, but no fixed storage-byte claim is
made. Reopen for a mandatory audit/erasure policy or measured history growth;
do not truncate evidence to make recovery appear successful.

## 3. Recovery arbitration and finality

`infra_jobs::operator` owns both statements and typed results; only
`infra_postgres::in_tx` controls transactions. Redrive and discard take a
validated `RecoveryTarget { id, kind, version }` and `&mut Tx`; no function
starts a business handler, obtains a second connection, or retries a business
closure. Their successful Rust results are provisional until the caller's
commit is acknowledged. The command wraps exactly one call in `in_tx`.

First lock the row by primary key using `SELECT ... FOR UPDATE`, decoding only
id/kind/state/generation. No row means `Missing`. A different kind or version,
or any state other than failed, means `Stale`. Then perform the UPDATE above
or DELETE with all four predicates (id, kind, version, failed) repeated. Lock
ownership plus the predicates allow at most one winner across redrive/redrive
and redrive/discard. A loser that waited observes the committed new version or
absence. A zero-row mutation after a lock is never success; return stale and
roll back. Discard acknowledges one deleted failed row only.

The existing partial unique index `background_jobs_live_unique_key` is the
sole live-key arbiter. Do not precheck and assume the key stays free. On
redrive, the UPDATE joining the live set may wait for an enqueue or another
recovery. SQLSTATE 23505 naming that exact index maps to `Conflict`; every
other constraint/SQL failure remains a database failure. Propagate the error
out of the transaction so its archive/reset rolls back. No savepoint, payload
merge, force option or key clearing is required. Deadlock/serialization errors
are known database failures under the existing Tx classification; no retry
loop hides them.

Commands render the closed outcomes `redriven`, `discarded`, `missing`,
`stale`, `conflict`, `failed`, `unknown`. A known rollback is failed, and
`TxError::CommitUnknown` is unknown. The 12-second timeout or signal after a
mutation operation has been invoked is conservatively unknown, even if the
client cannot establish whether COMMIT began. Before that boundary it is an
unavailable/failure result without a mutation claim. No result performs
readback followed by an automatic retry. Unknown instructs a fresh inspection
of the same id: the new version/state may show recovery, the old failed
version may permit a deliberate retry, and absence after discard proves only
absence. Do not attribute another actor's effect to this command.

The payload-free receipt carries `action`, job `id`, `kind`,
`expected_version`, `outcome`, and, only after acknowledged redrive,
`new_version`. Database diagnostics include only bounded SQLSTATE/cause,
never the raw SQLx error, unique index DETAIL, payload, key, DSN, trace or
stored error string. Every unsuccessful mutation exits 1; CLI usage exits 2;
success exits 0. Unknown always exits 1. These are operator-mode results and
do not change ordinary worker exit 0/1/3.

## 4. PostgreSQL-only command composition

The existing `jobs-worker` executable accepts optional subcommands. No
subcommand follows today's `run(args, register)` path unchanged. Worker CLI
parsing uses already-installed clap 4.6.7 derive/flatten with existing
`LoadOptions`; no argv splitting or bespoke parser. Loader flags precede the
subcommand (same definitions; they are not runtime key setters):

```text
jobs-worker [--config PATH] [--config-overlay PATH] [--secrets-dir PATH] inspect ID
jobs-worker [loader flags] failed [--after CURSOR] [--limit 100]
jobs-worker [loader flags] unhandled --handled-kinds LIST [--after CURSOR] [--limit 100]
jobs-worker [loader flags] redrive ID --kind KIND --version VERSION
jobs-worker [loader flags] discard ID --kind KIND --version VERSION
```

`LIST` is comma-separated with no whitespace normalization: `a,b`, and an
explicit empty argument `--handled-kinds ''` means none. Omission is a usage
error. The adapter admits each name through the existing 1..64-byte kind
grammar, deduplicates into a standard BTreeSet, and rejects more than 1024
input names or more than 66,559 input bytes before allocating the parsed set.
That is an operator-request resource bound, not a fleet registry limit; a
larger declared fleet reopens this CLI bound explicitly. The combined runbook
includes the reserved `publish_domain_event` kind, the current `OUTBOX_KIND`
in `infra-messaging::outbox`.
Changing a list between pages changes the question; restart from no cursor for
a complete result relative to that list.

Use existing UUID parsing for IDs, reject nil IDs, and expose safe parse errors
without echoing arbitrary input. Cursor is exactly `v1:<canonical UUID>`; it
is an exclusive primary-key position, not authorization. Accept only that
version and UUID syntax, at most 39 bytes. Limit is 1..500; default 100.
Kind/version/id/cursor/list validation completes before database access.

`service_config::load_jobs_operator` returns `JobsOperatorConfig` containing
only `postgres: PostgresConfig`. It uses the existing generic merge function,
namespace/file/secret pre-scans, sources, precedence and value-free error
renderer. Only the PostgreSQL section is decoded/validated, with unknown keys
inside it refused; other sections are ignored as in the existing migration
projection. It does not require auth, messaging, webhook, object-storage,
HTTP, jobs capacity, logging or telemetry configuration. A secret-like nonempty
TOML value is still refused anywhere by the shared pre-scan; this intentional
safety rule is not bypassed by projection. No configuration key is added.

Operator mode branches before ordinary config, registration and bootstrap.
It builds one short-lived Tokio runtime, installs/owns the existing Signals
streams, admits the configured DSN/password file with `Dsn`, requires
`postgres.enabled`, and opens the ordinary provider pool with one connection,
ReadCommitted and the configured session-budget source. One is a stricter cap
than every admitted `postgres.max_connections`; worker N-based pool admission
does not apply. `application_name` is fixed `jobs-worker-operator`, so no
telemetry section/secret is loaded for identity. Password files are read once
at admission; this one-shot process starts no refresher.

Call the existing `migrate::verify_history` under its 5-second bound, then a
shared infra-jobs session check under its existing 5-second startup budget:
UTF8 and read committed are required; mutation also requires writable primary
session. Read-only inspection may run on a read-only session whose schema
history is admitted. Refactor the current session query, not Engine creation,
to share this fact check. No Engine, listener, readiness, telemetry exporter,
registry, broker, worker TaskTracker or maintenance task is started.

One operation consumes the existing 12-second job-operation ceiling, including
acquire/BEGIN/query/COMMIT. Page/inspect reads use a read-only `in_tx_with`
transaction and `SET LOCAL statement_timeout='2000ms'`; mutation uses the
existing 8-second session statement ceiling. Closing the pool consumes the
existing five-second dependency-close ceiling; runtime teardown remains one
second. These stages run once without retries. The pool's three-second acquire
bound alone does not cover its session verification or refusal cleanup: wrap
the complete operator `infra_postgres::connect` future in the existing
five-second jobs startup-check ceiling. Bound history to five, the jobs session
check to five and the command to twelve: after argument/config/file admission,
normal network work plus close/runtime is bounded by 5+5+5+12+5+1=33 seconds.
The provider's internal three-second acquisition remains enforced. No filesystem-latency
guarantee is claimed. A stop signal interrupts admission/operation, drops its
future, then closes admitted resources. Mutation after invocation is unknown;
inspection is unavailable. Missing close acknowledgement never changes an
already acknowledged database outcome, but reports `cleanup='incomplete'` and
exits 1. Stdout/write failure likewise cannot undo a commit; shell automation
must inspect the same identity after an absent receipt. No process::exit call.

## 5. Bounded inspection and failed-depth observation

`inspect ID` returns one safe snapshot or `missing`. Snapshot fields are id,
kind, state, version string, attempts, failure_reason, created_at, not_before,
claim_expires_at, finished_at and recovery_count. Times are nullable UTC text
with microsecond precision produced from database timestamptz values; absence
is JSON null. No default output path fetches/decodes payload, key, trace,
error_summary or errors. `recovery_count` reads the array length without
returning archived content. Keep the representation in one DTO owner.

Both paged commands scan at most `limit` rows by the existing UUID primary key:
`WHERE id > cursor ORDER BY id LIMIT limit`, omitting the predicate on the
first page. Select only the safe snapshot columns. Filter that bounded result
in Rust: failed includes every kind with `state='failed'`; unhandled includes
pending/running rows whose kind is absent from the supplied set. The cursor
is the last **scanned** id, including nonmatches. If scanned rows equal limit,
return `complete=false` and that cursor; otherwise `complete=true` and no next
cursor. A full final page can require one extra empty page to prove completion.
An empty filtered page with a cursor is partial, not an empty queue. Every
success includes database `observed_at`, `scanned`, `items`, `complete` and
`next_cursor`. Timeout/unavailability produce an error envelope and exit 1,
with no successful empty page or advancing cursor.

This avoids an anti-join that can scan an arbitrarily large known-kind prefix
before returning its first match. It traverses every matching row over an
unchanged table, including completed stretches, with O(limit) decoded rows
and O(log handled kinds) membership checks. Concurrent inserts below the
cursor or transitions behind it require a later traversal. The bound is on
logical rows/decoded/output memory; MVCC dead tuples, page I/O and TOAST history
length are not made constant by LIMIT, so the database and outer deadlines
remain real time backstops. Query-plan proof must establish the intended keyset
index access without claiming a fixed physical-page bound.

Add `jobs_failed_jobs{kind}` capped at 1000, separately from `jobs_live_jobs`.
The failed gauge uses only registered names. Add an index `(kind,id) WHERE
state='failed'` through a separate one-statement `CREATE INDEX CONCURRENTLY IF
NOT EXISTS` migration. The sample adds a literal failed-state lateral lookup
limited to 1000, giving a structural 1000 visible-row bound per registered
kind without scanning every failed kind. The index also covers unknown-kind
failed storage; it does not confer execution ownership.

Close the existing multi-engine freshness race while extending the sample:
the `Engine::new` process-duty owner now owns the single sampling task as well
as retention and LISTEN. Its short peer-registry snapshot supplies the
deduplicated union of ordinary and publisher kind names; all worker engines
are built before any start. `Engine::beside` adds no sampler. Initialize every
registered live/failed gauge and the shared timestamp at zero. One database
sample decodes all kinds/states before publishing values followed by the one
database timestamp. Any query/decode failure retains every previous value and
timestamp. Dynamic peer registration, if used by a library caller, is picked
up as one union on a later sample; it never authorizes arbitrary queue kind
labels. Sampler task failure propagates through its existing process-duty
engine and combined worker shutdown. No new process or management permit is
introduced; this removes duplicate sampler work.

Freshness zero or older than 30 seconds remains unusable. Prometheus scrape
publication has the same value-then-timestamp ordering and scrape granularity
as the baseline; this is not a transactional metrics backend. Replica samples
are not additive. Unknown-kind visibility remains an explicit operator page
traversal, never a periodic fleet anti-join or unbounded metric label.

## 6. Material flow and proof boundary

| Trigger and owners | Durable/visible result | Failure/recovery and proof owner |
| --- | --- | --- |
| Claim reservation → supervisor → completion batch | One admission object covers handler, retries and bookkeeping until last drop; no extra claim above N | Deadline/cancellation drops registration/batch first, records uncertainty and relies on static lease; deterministic coordination beside attempt owner plus existing PostgreSQL jobs execution suite |
| Failed transition → retention owner | Failure remains failed indefinitely; completed rows retain 24 hours | No registered/unknown/outbox failure may enter retention DELETE; real rows in existing jobs execution suite |
| CLI → projected config → Dsn/pool/history → inspection | Safe bounded page or missing snapshot without dependency/handler startup | Usage rejection before I/O; timeout never empty; jobs-worker process and jobs operator integration proof |
| CLI target → shared Tx → row lock → live-key index → commit | Same row redriven once, or explicitly discarded; old cycle retained on redrive | Stale/missing/conflict leave no transition, Tx unknown instructs inspection; real PostgreSQL arbitration and both commit/rollback outcomes |
| Redriven outbox row → existing publisher → broker/consumer | Original stored intent and exact prepared publication identity/bytes flow unchanged | At-least-once, possible prior effect and consumer dedup horizon remain; existing outbox/jobs proof, no new live broker or proxy requirement |
| Process owner → union-of-kinds sample → metric values/timestamp | Failed accumulation visible only under a fresh successful sample | One engine's success cannot refresh another's failed observation; existing sampler/combined-worker tests |
| Additive schema → old/new writers → corrected fleet | Both writer versions accept added column/index; custody guarantee begins after last old retention owner stops | History admission and rollback boundary in rollout; migration/profile proof stays with existing owners |

Implementation chooses cases, fixtures and assertions under its normal owner.
Necessary proving questions are: saturated outcome writes, queue-waiter and
in-flight-batch cancellation across repeated slot reuse; known-result priority;
completed-only retention of old rows; exact identity/history/attempt reset;
same-token later-cycle rejection; concurrent mutation and enqueue collision;
acknowledged versus unknown commit/rollback; unchanged outbox bytes; input and
secret refusal; paging through entirely nonmatching windows; sample freshness
across ordinary/publisher ownership; additive history/profile compatibility.
The existing real-database and initializer CI paths own those observations;
pure mocks do not certify database outcomes. No separate test-plan phase,
benchmark acceptance, repeated full-build matrix, new fault proxy, external
fleet change or production query is required.

Update active architecture/runbook descriptions that currently say slots end
at handler return, failures expire after seven days, each engine samples, or
the binary refuses all positional arguments. Explain discard as permanent
abandonment **before** its first example; redrive requires reconciliation of
non-idempotent effects. Explain declared fleet kinds, capped counts, history
storage/custody, polling pickup, static recovery latency, one publication slot,
the shared failure domain and the all-old-workers-stopped gate. Update only
the relevant profile markers and current source owners.
