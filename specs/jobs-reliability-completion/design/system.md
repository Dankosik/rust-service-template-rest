# Jobs reliability completion: Technical Design

Status: ready. Independent [Technical Design Review](../technical-design-review.md): PASS.
Owner: Technical Design. Inputs: [ready specification](../spec.md),
[Intent](../intent.md), [Definition evidence](../research/baseline.md).
Source baseline: `ac88395be87cba3a1e0587f533dc50a71e358c8d`; current Definition
commit: `ccf16c3ca29955701639ce3f522626228c699b75` (documentation only).
The [ownership map](ownership.md) is part of this decision.

## Selected mechanism

Extend the existing jobs attempt owner in place. Retain its task, tracker,
admission, completion batching, fencing, deadline arithmetic and process-failure
observer. Add a safe destruction boundary for the owned handler future and
an armed retirement guard for the supervisor. No new runtime crate, task per
handler, unbounded result registry, queue framework or runtime setting.

R2–R5 share one executable reading-counter reference in the existing
`integration-tests` fixture owner. Its local CLI accepts an operation, a real
`jobs_worker::run` process performs the business job, and the CLI reads durable
results. Outbox and webhook deliveries update independent receiver projections.
The reference sources and schema are also installed as service-owned files in
an actually initialized disposable service. They are not shipped in the
template service image or added to the template's canonical runtime migrations.

This is a diagnostic recipe, not a new application framework. The current
Tx, jobs, messaging, webhook test transport, commit proxy, process fixture,
Compose, initializer and portable-sync capabilities supply its mechanisms.
The [existing research](../research/baseline.md) closes library selection:
no external library or version change is needed. If implementation finds a
missing capability, reopen only that decision before adding a dependency.

| Choice | Decisive constraint and accepted cost | Alternative disposition / reopen |
| --- | --- | --- |
| Handler poll and destruction stay on its supervisor task | Preserve current scheduling cost and outcome precedence; a small private owner must destroy exactly once | A separate handler task reintroduces a spawn/join per attempt without solving another requirement; reopen for actual independent execution lifetime |
| Guard reports into the existing per-start failure latch | TaskTracker owns completion but not success; guards already enforce this for engine tasks | A JoinSet collector/second lifecycle would duplicate ownership and retain results; reopen only if future result processing needs an independent owner |
| Independent PostgreSQL receiver database in the same disposable Compose service | Receiver truth survives producer restore; it need not survive loss of this entire local server | A file store or second database container adds storage semantics or lifecycle with no stronger accepted claim |
| Fixture CLI plus actual worker composition | R5 needs a working business path, not a public API; keeps fixture business/schema out of the shipped scaffold | Public REST/auth/OpenAPI and new feature/provider crates add an unrelated contract; revisit only for an adopter-facing API requirement |
| One source reference scenario over existing carriers | Actual crash/restore/upgrade and measurements, without multiplying the initializer matrix | A new cloud runner or full workload per profile/harness is unnecessary; optional environments never substitute for an unrun required result |

## R1: attempt containment and failure custody

The acknowledged claim transfers its slots and payload to one supervisor as
today. `Engine::start` remains the owner of the start's stop token and sticky
failure token. Pass that custody through the claim path to dispatch; do not
replace it with a global token shared by unrelated engine starts.

Dispatch arms a private supervisor-retirement guard before submission to the
attempt tracker. The guard therefore also exists if the submitted future is
dropped before its first poll. It holds the existing start's failure/stop
capabilities, not a second task list. Unexpected destruction, cancellation or
unwind before normal retirement stops new claim rounds and latches failure.
An already-issued claim still settles under the existing rule. A panic remains
a failure even if normal retirement had just been marked. Ordinary retirement
means a known fenced outcome, an acknowledged unchanged write, or explicit
uncertainty after the established budget; an impossible missing registration
is not a successful retirement.

The handler owner holds the existing heap-pinned `HandlerFuture`. Poll still
uses the resolved `futures-util` catch boundary. Destruction takes that owned
future exactly once and uses `std::panic::catch_unwind` at the synchronous
drop boundary; the fallback destructor uses the same boundary so early return
or supervisor cancellation cannot bypass it. This requires no unsafe pin
projection. It contains the supported single unwind and emits a payload-free
observation. It does not format, persist or otherwise inspect the panic payload.
The [standard-library contract](https://doc.rust-lang.org/std/panic/fn.catch_unwind.html)
also permits the caught payload's destructor to panic. A secondary unwind
during its ordinary disposal follows the armed supervisor-failure path; it
is not silently ignored, retried in a disposal loop, or retained/leaked in a
growing payload collection. Such a supervisor failure stops admission and
becomes the existing process failure, rather than promising continued work.
Abort, double panic during unwind and blocking/starved execution retain the
specification's exclusion; no timer promises to preempt a destructor.

| Handler/supervisor crossing | Enforcement and finality |
| --- | --- |
| Ready result wins the existing biased poll | Keep that result. An inner handler destructor which already panics inside the typed wrapper's poll remains the existing sanitized handler-panic outcome |
| Timeout/force selected, cooperative completion succeeds | Keep success; other responses use the already-selected cancellation cause |
| Cooperative grace ends with handler Pending | Safely destroy it before ordinary outcome persistence; contained drop panic does not rewrite timeout into panic or force into retry |
| Handler destruction consumes the remaining budget | Record uncertainty; no late queue write or extended lease/deadline |
| Supervisor fails outside contained handler work | Guard reports `jobs_engine_task_stopped` with static task identity and panicked/unconfirmed status; `Started::failed` becomes ready. It makes no new claim about whether an earlier write committed |
| Tracker becomes empty | Completion observation only. The worker's existing live wait/pending-failure and shutdown readback still consult `Started::failed`: live failure is primary exit 1; cleanup-only failure after stop votes degraded exit 3; prior primary failure wins |

Contained handler destruction needs one finite diagnostic event in the attempt
span; normal handler panic remains routine attempt failure, not an engine
failure. Existing unknown/persistence counters describe database uncertainty;
do not increment them merely because the supervisor guard cannot prove a
different already-known outcome. Existing `AttemptSlots` and completion
registration retain admission until both supervisor and bookkeeping retire.
No new counter labelled by job ID and no payload-bearing diagnostic is added.

The worker's existing failure readers are retained, not reimplemented. A
source-confirmed gap in a reader may be repaired there; the current read path
already checks engine failures during serving and after background joining.
Preparation/first-poll deadline policy is unchanged: it is not an established
defect. Reopen this section for an actual reproduction, not the hypothesis.

## R2/R3/R5: one business flow and its authorities

The generated fixture operation is `(scope, operation_id, article_id,
content_version, content)`. Scope is the fixed disposable run/service identity;
IDs are bounded UUIDs, content is bounded, and the immutable encoded operation
is at most 1 KiB. Equality compares the complete immutable fields, not a hash
alone, transport ID or attempt generation. Two operation IDs for one article
represent two reads. Reusing one ID with different fields is a conflict.

| Durable owner | Representation and responsibility |
| --- | --- |
| Producer `reading_requests` | Primary key `(scope, operation_id)`; immutable request plus its prepared event identity/time and accepted transport IDs. Durable acceptance/readback independent of queue retention |
| Producer `reading_effects` | Separate marker keyed by `(scope, operation_id)`; immutable operation and stored result of the first local mutation; no TTL |
| Producer `reading_articles` | Article aggregate keyed by `(scope, article_id)`, with checked nonnegative read count |
| Receiver `reading_effects` / `reading_articles` | Same recipe in a different database; marker and aggregate additionally scope by fixed channel `outbox` or `webhook`. Each is a separate observable external effect, not twice the local reading count |
| `background_jobs`, broker state | Existing transport scheduling, leases, attempts and settlement; never the authority for business dedup or external readback |

`accept` executes an application transaction through `infra_postgres::in_tx`:
admit the request identity and enqueue one local `reading.record` job, one
prepared `reading.accepted` outbox event and one durable webhook. All three
intents commit with the accepted request. The event describes accepted intent,
not a local effect that has not happened yet. Preparing identity/time/body once
keeps retries stable. An equal existing request returns its accepted identity;
a conflicting request rejects without mutation or additional intent. On unknown
COMMIT, the command reports uncertainty and reads the same request identity
before deciding whether another same-identity attempt is allowed.

Fan-out is fixed at acceptance: 128 accepted operations create at most 384
initial job rows. No handler creates another queue job. After acceptance stops,
the fixed workload cannot amplify its logical backlog; retry/redrive reuses
existing intent except the explicitly isolated transport-identity replay recipe.

The local job calls the marker-and-aggregate recipe in one caller-owned `Tx`
and propagates `job.complete_in_tx(tx)` failure. Marker uniqueness arbitrates
concurrent duplicates. A new marker and the aggregate increment commit together;
an equal duplicate observes the established result after the winner's fate;
conflicting immutable content is refused. Rollback removes both changes.
A stale completion aborts preceding business changes. Unknown COMMIT returns
through existing handler/Tx classification without calling the business closure
again; readback, fenced completion and ordinary lease recovery keep the same
operation identity. The recipe's stored result is the first mutation result,
not the aggregate's later count.

Outbox reuses the accepted operation's stable logical ID and prepared bytes.
Webhook body carries that same business identity; its `webhook-id` is the
transport JobId and may change when a new transport is deliberately created.
Both receivers execute the same marker/aggregate transaction against their own
database. NATS consumer ACK and HTTP success follow acknowledged receiver
commit. A receiver CommitUnknown first reconciles its marker; no ACK/2xx asserts
effect without that result. Matching duplicates settle successfully after
readback; conflict never increments the aggregate. No egress ordering is
promised: final evidence observes all three channels independently.

The receiver is a local fixture process using existing real JetStream consumer
and loopback webhook transport patterns. Its endpoint, channels, keys and DB
target are harness-owned constants/inputs, never operation-selected routes.
Use existing default-off test-support injection for a loopback webhook client;
keep production HTTPS/admission unchanged. Disposable signing material stays
in memory/environment and out of receipts. Receipt readback is a bounded CLI/DB
operation, separate from the producer's saved backup. Existing protocol tests
remain the signing/wire proof; this reference owns durable business finality.

No marker expires. Recipe documentation relates this to indefinite retained
failed jobs, operator replay and restore. Demonstrate a new transport identity
and permitted redrive after actual transport cleanup, reusing current retention
and short broker-window fixture techniques. If queue aging is accelerated,
record that fact; it is not evidence that 24 wall-clock hours elapsed. Actual
process crash and backup/restore below are never replaced by staged row updates.

## R2: crash, restore and operator sequence

The reference uses three disposable roles: producer worker, receiver process,
and supervising scenario driver. The driver owns every child, proxy, observer,
temporary checkout, database, stream and backup path; it joins/stops them on
success and failure. The existing Compose carrier owns the PG/PgBouncer/NATS
container lifecycle. Kill only the driver's recorded worker PID, without
SIGTERM/drain, and observe its actual exit before replacement. Fault milestones
use existing commit/autocommit proxy and ACK-dropping/receiver fixture seams;
bounded handshakes establish where the interruption occurred.

Use the Compose PostgreSQL version's own `pg_dump`/`pg_restore` tools, no new
host installation. Quiesce producer writers for the snapshot and record exact
membership in `reading_requests`; take a real database backup including queue,
claim-generation sequence, business schema/data and migration history. The
receiver database and NATS storage are outside that dump. A chosen later
accepted operation demonstrates the RPO gap: restoring the snapshot may lose
it, and the driver must report it as outside the backup, never silently rebuild
it from its expected-results manifest.

| Stage / owner | Action, safe failure and completion boundary |
| --- | --- |
| Driver, acknowledged intent | Exercise unacknowledged/rollback acceptance and committed acceptance before execution; independently read the request identity before retrying |
| Driver, interruption | Terminate the actual worker at recorded effect/ACK/completion boundaries; producer-only kill leaves committed receiver markers readable |
| Worker restart | Start compatible code against the unchanged producer DB; real lease expiry/fenced retry recovers live intent. Preserve independent effect readback; no SQL lease acceleration establishes this crash claim |
| Driver, restore | Stop and join all old producer writers/consumers, restore the actual dump into a fresh empty disposable producer DB, read migration history and kinds, and switch only the new process to that DB |
| Operator recipe | Discard every pre-restore command/token/receipt; fresh inspection may have the same numeric version. No epoch or automatic token invalidation is claimed |
| Reconciliation | Compare restored request/snapshot membership, queue ID/kind/state/generation and each local/receiver marker before enabling processing or issuing redrive |
| Known effect | Equal marker permits same-identity replay which the receiver absorbs; do not infer this from queue completed or broker ACK alone |
| Known absence | A successful complete lookup against the authoritative receiver with old writers stopped permits compatible same-identity replay |
| Unknown external effect | Missing/unavailable readback leaves `pending_manual_reconciliation`; keep that isolated scenario's processing stopped, issue no redrive/new delivery, and retain the receipt |
| Completion | Positive scenario reaches one effect per selected operation/channel; unknown scenario retains its explicit hold. Stop producer, finish bounded shutdown, capture final observations, then clean only owned disposable resources |

The unknown branch deliberately blocks receiver readback while preserving its
durable store. It runs in an isolated scenario so a global worker restart cannot
accidentally replay an unresolved row. Its expected result is an operator hold,
not the all-128-success criterion of a positive load scenario.

Each reconciliation row records logical identity, snapshot membership,
acceptance result, current queue identity/state/generation, per-channel marker
and result, permitted action and its acknowledged/unknown receipt. The run also
retains revisions, tool/server versions, redacted config, backup hash/membership,
fault milestones, elapsed recovery and resource cleanup result. This proves
local DR mechanics and the stated RPO; it is not production RPO/RTO certification.

## R4: fixed workload, budget and observation

Use one retained all-required-profile representative, direct PostgreSQL for
the combined process rehearsal; existing tests retain transaction-PgBouncer
proof. No full Cartesian product of harnesses, database modes and workloads.
Compare baseline and each fault with the same release fixture executable and
configuration. Compile once per exact source revision; setup/build durations
are recorded separately, never represented as runtime recovery or throughput.

| Input | Fixed value / owner |
| --- | --- |
| Work | 128 accepted operations; immutable encoded payload at most 1 KiB; fixed article distribution; producer then stops |
| Job slots | Ordinary jobs `jobs.max_workers = 3`, shared by reading jobs and webhooks; outbox's existing separate slot = 1; total at most 4 |
| Kind policy | Reading job timeout 30 s, 25 attempts; existing webhook/outbox policies remain unchanged; no production default changes |
| Worker pool | `postgres.max_connections = 8`, the admitted combined-mode `N + 5` minimum; one extra LISTEN session |
| Other pools | CLI producer 2, receiver 2; receiver consumer concurrency 1 and fixed finite HTTP admission; no dynamic growth |
| Session allocation | Worker 8 + LISTEN 1 + CLI 2 + receiver 2 + snapshot/migrator 1 + bounded observer/control 2 = 16, plus server reserved/admin allowance; read actual `max_connections` and fail preflight if this allocation does not fit |
| Fault | One at a time, at most 5 s: worker withheld/stopped with confirmed backlog; worker's actual shared pool occupied; NATS unavailable after initial admission |
| Recovery | At most 180 s after fault release, including any restart/readiness/real lease wait; do not start the timer after these finish |
| Scenario | At most 300 s from first fixture process start through workload, fault, recovery, reconciliation and child shutdown; initialized checkout/build/dependency provisioning is separately timed setup |

Keep existing PostgreSQL acquire/statement/return, worker drain/cleanup and
45-second process grace budgets. No enlarging them after failure. For NATS,
stop/restart only this run's broker with its retained stream state; start the
fault after worker readiness because ordinary worker startup requires NATS.
For pool pressure, a fixture-only control holds acquired connections from the
worker's existing pool for the accepted window; an unrelated observer pool is
not a valid substitute. The first accepted reading job obtains this pool from
`Job::pool`; its fixture registration supplies a bounded hold/release control.
Admit that operation first, confirm the hold, then submit the rest of the
128-operation batch. Release the connections before its ordinary effect/Tx.
The control remains inside the admitted handler lifetime, has a 5-second
ceiling even if the driver disappears, and cannot enter a shipped profile.
It supplies observation and does not add a runtime setting.

The 300-second runtime ceiling includes the sequential 5-second fault,
180-second recovery observation and existing shutdown ceiling. The remaining
time covers finite acceptance, baseline observation and reconciliation; failure
to fit is a failed scenario with evidence, not a larger bound or a skipped case.
Backup/upgrade exercises are separately bounded process runs; their compile
and initialization work is setup, while every started runtime scenario uses
the same ceiling. This interpretation preserves the runtime envelope rather
than trying to include Cargo compilation in an application workload duration.

Capture existing queue/failure metrics with freshness timestamps, direct bounded
SQL readback for the 128 identities, process counters/RSS/CPU and monotonic
milestones. Report available/scheduled/running/failed/completed/unknown separately,
attempts, oldest age, acquisition waits/failures, used/max pool capacity and
recovery time. Existing queue gauges do not expose admission or completion
membership. Add only two process-wide, label-free gauges in the existing
`infra-jobs` metric owner: currently owned attempt slots and currently
registered queued-plus-in-flight completion entries. Update them at their
actual ownership transitions, including unwind and final retirement; no second
sampler, public state accessor or result registry. The receipt records observed
peaks/idle return; owner-local proof establishes the structural at-most-four
bound which sparse scrapes alone cannot prove. RSS does not establish it.

Success requires all 128 accepted identities to reach one confirmed effect
per channel, backlog to drain after producer stop, owned admission/bookkeeping
to return to idle, and pool occupancy to stay within 8. Completed task/results
must not accumulate outside those owners. RSS/CPU are measured samples, not a
hard memory cap or production capacity claim. Attempt exhaustion or the
recovery deadline fails the run; retain its diagnosis. New fairness, quota,
executor or pool-size policies require a measured in-scope violation and this
design's narrow reopen.

## R5: exact initialized-service upgrade

Use baseline template revision
`ac88395be87cba3a1e0587f533dc50a71e358c8d` and the eventual immutable implementation
candidate SHA. They must be distinct. The receipt records both template SHAs,
the derived Git commits before/after adoption, reference-source hashes and
binary identities. The candidate SHA is filled only after commit; a dirty
worktree label or mutable branch name is insufficient.

Initialize a fresh disposable checkout from baseline through `init-module.sh`
with PostgreSQL/jobs/outbox/JetStream/durable outbound webhook and bounded
outbound HTTP retained. Install the candidate's standalone reference fixture
sources and their scoped Cargo declarations into that derived checkout as
service-owned application work. This is legitimate baseline application code:
it uses existing public APIs and must execute on baseline before upgrading.
Add a service-owned architecture fact/skill, commit the derived application,
run acceptance → worker → count readback, and leave durable accepted/effected
identities and schema in place. The feature and customization therefore predate
the template upgrade.

Portable `template-sync` updates only its declared portable surface and its
receipt. It does not update Cargo/application Rust, migration files or local
architecture. Execute its normal diff/apply flow from the candidate source.
Then explicitly adopt the candidate's scoped runtime source delta from baseline
(the changed `infra-jobs` files and any causally required worker failure-reader
repair). Record the actual patch, hashes and integration steps; abort on an
unexpected conflict instead of copying the whole candidate over the service.
Reference business source/schema and customization stay service-owned.
No template runtime migration or persisted queue format change is selected.

Build the updated derived executable with its locked graph and run the same
feature on preserved data, then R3 duplicate/conflict readback and R2 recovery
against that updated executable. A second initialization without feature/data,
a source diff alone, or portable sync alone cannot establish upgrade success.
The existing customization canary remains adequate for generic sync mechanics;
this run adds business execution across actual distinct runtime revisions.

## Carrier, proof boundaries and release closure

Extend the existing `integration-tests` jobs area and test-only worker fixture
pattern. Keep a reference-specific driver under `scripts/tests/` for initializer,
exact-source adoption, backup tooling and process measurements. It borrows
the existing PostgreSQL/NATS Compose carrier and validation lock; no replacement
container environment, third-party runner or production resource is needed.

The regular focused jobs/worker proof owns R1. The combined reference exercise
runs once for the source candidate on the existing integration carrier, with
one selected initialized-service graph for R5. Reuse compiled binaries across
its bounded scenarios. Do not add the full rehearsal to every initializer
graph or harness; preserve existing profile/fixture compilation checks. If the
existing integration CI invocation needs a source-only reference step, put it
under that same integration job and make its failure fail that job, with the
matching classifier/command ownership; no independent release gate is added.

Implementation owns cases, assertions, fixture coordination details and exact
commands. It reuses the existing effect-as-PK, transactional completion,
unknown-commit, operator and sync-canary evidence where those claims are
unchanged. Added proof covers the materially missing Pending-drop/supervisor,
separate marker/business mutation, actual crash/restore, bounded measurements
and executed derived upgrade claims. Final validation, integration build and
the independent assembled-candidate review occur once under their existing
owners; Design runs no heavy runtime proof.

Deployment graph is local only: CLI → producer PG/jobs worker → NATS/webhook →
receiver PG, plus the source-to-derived adoption edge. Stop old writers before
restore or replacement; start compatible code only after history/identity
readback. Last rollback-safe state is the pre-adoption derived source commit
with its original DB, plus the retained independent receiver store. Runtime
rollback preserves schema and intents; producer restore loses post-snapshot
intent and must reconcile surviving external effects. Cleanup may delete only
the recorded disposable resources after retaining the result, including failed
run diagnosis. Commit/push/PR updates are authorized in the parent outcome;
merge, deployment, production access and new paid infrastructure remain outside.

Reopen Specification for altered meaning/finality/replay lifetime; Research for
a contradicted source fact; Technical Design for missing ownership, a mechanism
gap, actual budget violation or unsupported fixture API; Intake only for a
changed user outcome or authority. Implementation never needs to ask the user
to select a runtime mechanism.
