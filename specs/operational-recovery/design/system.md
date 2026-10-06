# Operational recovery: system design

Status: fixed candidate for Technical Design Review. Baseline:
`699887b18594088a59bcc23a049d290d089f6da1`.
Inputs: [Definition result](../definition-result.md), [Specification](../spec.md),
[Intent](../intent.md), and [source research](../research/baseline.md).
This document and [execution](execution.md) select mechanisms;
[ownership](ownership.md) fixes their placement. Implementation owns test cases,
fixtures, assertions and commands within these proving boundaries.

## Decision

Keep the current readiness publisher, shared database pool, transport readers,
background supervision and staged cleanup. Add a process-armed completion
contract to the existing health state, selected unconditionally by the service
and retained worker roots. A completion gap is terminal for that process;
a completed failing round remains a recoverable dependency observation.

Keep the current validation entry points and receipt owner. Replace the unsafe
directory-deletion lock protocol with a permanent kernel-lock inode and
generation custody; add command-scoped local compiler caching and resource
evidence through those entry points. There is no second runner, queue daemon,
runtime framework, new production dependency or datastore.

The decisive drivers are lossless expiry/stop arbitration, one authority per
fact, recovery through real work, no change to numeric defaults, and safe
cross-worktree execution. The cost accepted is a small state extension and
one tracked expiry observer per process, plus Python standard-library adapters
for the existing developer scripts. The latter can refuse uncertain interrupted
ownership rather than permit overlapping validation.

## R1: one serialized completion contract

### Truth and representation

`crates/health/src/lib.rs` already writes `State.last_check` through
`watch::Sender::send_modify`; its monotonic `Instant` and `RefreshPolicy::stale_after`
remain the authority. Extend that same state with a small process-progress state:

| State | Meaning | Legal next state |
| --- | --- | --- |
| Unarmed | Generic reusable readiness; no process obligation | Armed after successful startup admission, or existing drain |
| Armed | The last completed round owes its next completion within the existing bound | Armed on a timely completion; Expired on a gap; Stopped on a timely stop |
| Expired | Sticky terminal evidence: prior completion, first expiry observation and bound | Expired, including after completion or stop |
| Stopped | Process stop closed future progress obligations before cancellation | Stopped |

Use `B = probe_budget + 3 * max(interval, probe_budget)` without a duplicate
formula, new timer setting or altered policy. Equality is fresh; expiry starts
strictly beyond `B`. A finished success, failure or timeout is a completion.
A poll, start, scrape, cached read, cancelled probe or ticker wake is not.

The process-facing health capability provides arm, await-expiry, inspect-expiry
and stop/disarm operations. Names and signatures may follow local idiom, but the
contract and ownership below are fixed. No trait, new crate, global registry or
arbitrary task-progress API is needed. Only composition roots arm it; ordinary
`Readiness::new`/`refresh`/reader users retain recoverable freshness semantics.

### Linearization and bypass closure

Arm, completed-round publication, expiry observation and stop/disarm arbitrate
under the same short watch-state write lock. No dependency I/O, logging,
cancellation wait or await occurs inside that critical section. For an armed
round the monotonic completion instant is taken at the serialized publication
boundary; a value sampled before waiting for that boundary cannot backdate a
late publication. There is still only one completion timestamp authority.

1. Startup completes its existing first refresh and admission verdict. The root
   arms immediately, using the admitted completion's timestamp, not resetting it
   to the arm time. Arming an already stale admitted completion fails through
   the primary progress-loss path. Failed pre-admission startup stays unarmed.
2. Before replacing an armed last completion, publication checks its previous
   age. A gap above `B` latches Expired first, even when the new round succeeds.
   New completed-round counters/timestamps may report what actually finished,
   but terminal readiness stays withdrawn and no recovered event is published.
3. A tracked observer waits for either publication, the strict stale edge
   (`last completion + B + 1 ns`), or process cancellation. It rechecks state
   under the same lock after wakeup, and reports the latched failure. Watch
   coalescing is harmless because a late publisher already retained the gap.
   This observer is independent of the refresher future and of HTTP, gRPC,
   diagnostics and metric scrapes.
4. A requested stop performs the serialized stop transition before cancellation
   can suppress a completion and before any awaited teardown. It checks for
   already-expired progress before changing Armed to Stopped. Every root stop
   path, including startup interruption, serving stop, panic/unwind cleanup and
   explicit failure cleanup, reaches that final arbitration. An earlier primary
   failure remains primary; progress cannot replace it. A gap newly elapsed
   after a timely stop does not turn requested cleanup into primary failure.
5. The process uses its first observed stop instant as the stop boundary; it
   does not claim the kernel's unobserved signal-arrival time. Signal selection
   and readiness disarm occur in the same non-awaiting root branch. Final root
   arbitration also covers a signal that wins a biased select before the
   background observer reports its already-latched expiry.
6. Drain remains monotone and wins for transport projection. `start_drain` also
   closes an armed obligation through the same operation, so an alternate drain
   caller cannot bypass expiry accounting. Terminal evidence survives draining.
   An Expired armed reader continues returning the existing not-ready wire
   response even after a new successful round; generic unarmed readers can
   still recover after a stale publication.

An expiry waiter releases its read borrow before sleeping or waiting for a
watch change. It uses the always-retained health implementation, not code
inside the optional gRPC template markers. It has no polling dependency on
`readiness_ready`, whose normal published-value semantics remain distinct from
time-adjusted readiness. Expiry forces that gauge to zero for the armed process;
it does not invent a completion timestamp.
After Stopped, the tracked observer waits for its cancellation token instead
of returning early while Background still requires process-lifetime work.
After reporting Expired it likewise retains normal tracker custody until
cancellation; its report, rather than its eventual return, triggers failure.

### Root failure and completion ownership

The service arms at its existing startup admission in
`crates/service/src/bootstrap/mod.rs`, before binding/announcing ready. Its
existing `Background` tracks both the refresher and expiry observer with child
cancellation tokens. The observer reports the static owner
`readiness_progress` through the existing sticky background failure reporter;
it does not merely return and hope a cancelled completion guard reports it.
Root `pending_failure` and final stop arbitration consume the same retained
health failure if report scheduling lost the race.

The worker arms immediately after successful readiness admission in
`crates/jobs-worker/src/bootstrap.rs::prepare`, before jobs start claiming,
consumers start pulling or the ready event. It starts the observer at that
point, not only at the later `spawn_refresher` call. Its existing background
tracker gains only a crate-private explicit first-failure recording operation,
using its current sticky `stopped` watch. `pending_failure`, `pending_end`,
`signal_ended` and the final `serve` arbitration must preserve the same rule.
The worker refresher can be started at admission alongside the observer to
avoid owing completions before its own driver is running.

The first serialized Expired transition produces one bounded
`readiness_progress_lost` indication, emitted outside the state lock with static
task name and numeric age/bound. Both roots consume that same retained evidence,
retain the first primary cause, and enter existing failure cleanup once.
Failure is exit `1`; clean requested stop is
`0`; degraded requested cleanup is `3`. Existing resources remain retained for
join and close. No observer creates a second teardown deadline or silently
restarts a task. The present grace and complete 18.5-second tail stay intact.

Every in-process observation requires runnable scheduling. Total starvation
can delay observation and teardown. On resumption, a late publisher or the
observer latches the expired gap before renewed ready publication. Only an
external supervisor can enforce wall-clock restart while the runtime cannot
run; no scheduler-isolated thread or platform mutation is part of this design.

### Selection and alternatives

| Alternative | Disposition and accepted cost | Reopen condition |
| --- | --- | --- |
| Poll only `ReadinessReader::verdict` in bootstrap | Rejected: a success can replace stale history before observation. The existing watch lock must retain the gap. | A canonical lossless completion event stream replaces the present state authority. |
| Make all health readers permanently fail after staleness | Rejected: generic library users own no process lifecycle and retain recovery. Arming is a root decision. | Specification intentionally changes generic health semantics. |
| Add one generic heartbeat/deadline for every manager | Rejected: idle listeners/queues are not failed work, and existing providers have real operation deadlines. | A named new manager has an accepted completion obligation without an adequate existing owner. |
| Add a watchdog crate/thread or second supervisor | Rejected: existing Tokio watch/timer, cancellation, trackers and failure channels provide the required mechanism under the stated scheduling bound. | Required wall-clock detection must survive full runtime starvation; that reopens Specification/platform ownership. |

No new Rust dependency is selected. The current Tokio/watch and cancellation
contracts are already used by health and both roots; a general watchdog cannot
choose this application's completion authority, stop precedence or exit policy.

## R2 and R3: recovery through actual consumers

### Runtime flows retained

Each instance retains its ordinary admitted SQLx pool, pool maximum, acquire
and probe deadlines, failure threshold, request shedding and failure codes.
Readiness's PostgreSQL probe competes for that same pool. Local acquire refusal
is a local capacity observation; a connection/query error is a dependency-path
observation, not a fleet diagnosis. Both finish rounds and keep the completion
contract alive. Remove pressure/interruption, then newly completed dependency
work and a fresh successful round restore ordinary readiness in the same
instance. Existing uncertainty/fencing rules remain authoritative for writes;
these checks require no uncertain write replay.

HTTP `/health/ready`, gRPC Check and one continuously open health Watch consume
the same instance reader. On ordinary failure the Watch must traverse
`SERVING -> NOT_SERVING -> SERVING`; it reconnects only for genuine transport
loss, never merely to learn recovery. Drain still sends NOT_SERVING and closes
Watch without extending drain. R1 expiry instead becomes terminal withdrawal
and process failure. Transport consumers never run probes.

### Proving boundaries and why they are sufficient

| Claim | Smallest authoritative boundary | What it cannot establish |
| --- | --- | --- |
| Exact completion-gap ordering, equality, late completion, stop and generic recovery | Health state and observer, with existing controlled-time facilities | OS process exit or actual database recovery |
| Both roots arm/observe/disarm independently of diagnostics and consumers, preserve exit/grace and cleanup custody | Existing service lifecycle/process boundary and retained worker process fixture, plus root-local control of lifecycle state | Fleet availability or scheduler-independent deadlines |
| A's responsive pool pressure does not mutate B's readiness or useful work; shared dependency interruption can affect both and both recover | Two independent instance compositions with separate admitted pools, health state, refresher lifetime and real TCP listeners, sharing one real PostgreSQL from the existing integration runner | Separate OS scheduling domains, production fleet sizing or performance |
| Dependency-backed HTTP and gRPC request success before and newly after ordinary recovery; live Watch propagation | The same database-backed two-instance composition through `infra_http::Server`/hardened router and `infra_grpc` router/standard health service | Shipped business endpoint or root-process supervision |
| With no diagnostics, the application connection cap can block probes and useful HTTP work, then both resume at unchanged capacity | The application listener in that composition, with diagnostics absent; existing black-box service no-diagnostics lifecycle observes actual bootstrap topology | Diagnostics scheduler isolation or platform healthcheck behavior |

The two-instance boundary deliberately means two separately owned service
compositions in one finite integration process, not two replicas with independent
OS schedulers. Each owns its own pool, refresher/cancel/join and listener handles;
only the real dependency is shared. The isolation claim is template-local state
and useful work. Process lifetime is proved separately at each actual root.

Place database-backed consumer composition under the existing
`test/tests/postgres.rs` binary, in one `test/tests/postgres/operational_recovery.rs`
module if needed for its cohesive lifecycle. Use the already declared SQLx,
HTTP, Tokio and health owners. The test-only HTTP operation performs a fresh
query through that instance's admitted pool; a test-only gRPC Echo implementation
does the same and uses the retained generated Echo contract. Their successful
return carries newly obtained database data so a cached probe or counter cannot
stand in for useful work. It adds no route to the shipped service, no schema,
production profile or public fixture API. Existing transport fixtures continue
owning pure protocol behavior; their old success does not prove database work.

The consumer module's gRPC imports and dev-dependency edges are projected only
when both PostgreSQL and gRPC are retained; its HTTP/pool proof remains when
gRPC is removed. Use existing auth fixture support only for the selected auth
profile; do not weaken the production chain to mount this proof. New dev edges
reuse the workspace's resolved packages/features, with no version upgrade.

Instance A's held pool capacity, the shared dependency interruption, and held
application connections have different owners and recovery releases. Evidence
must name which was injected, retain A/B loss/recovery and new useful-work
outcomes, and retain completion freshness/failure class. All held connections,
relay tasks, refreshers, Watch streams, listeners and fixture pools remain owned
through bounded teardown. The existing database runner owns server/compose
lifetime; no second database runner or environment is introduced.

This selects the causal boundary, not test recipes or a scenario multiplication.
Implementation can reuse one built integration candidate across these finite
observations. The actual database proof stays under
[PostgreSQL Validation](../../../docs/validation/postgres.md); a unit-only or
transport-only pass cannot supply it.

### Rejected forks and reopening

A new shipped business route is unnecessary because the current adapter
composition can carry a real dependency-backed fixture. Separate OS processes
for all database scenarios duplicate root supervision proof without proving a
new accepted scheduling claim. A pure controllable health probe is inadequate
for the real database claim, although it remains adequate for transport-only
ordering. A larger pool, new retry/queue, health-only connection or lower failure
threshold would change policy without workload evidence. Reopen only if a
retained profile cannot mount the accepted adapter composition or evidence
shows state crosses instances through a hidden singleton; missing test choices
do not reopen this design.

## Guidance and delivery closure

Update the current runtime/persistence/gRPC and Build Speed owners in place.
Readiness/liveness routing is conditional on actual platform behavior:
continuous readiness ejection and liveness restart differ from deployment-only
promotion checks. Diagnostics has a separate connection cap, liveness and
metrics, no readiness route, and the same scheduler. Without diagnostics the
application connection cap can prevent either probe from connecting; completed
pool failures remain recoverable and are not a restart signal.

Replace the old illustrative 6/12-second text with phase-plus-serial-round
arithmetic: approximately 6/11/14 seconds for fast failure/3-second acquire
failure/4-second probe timeout at current defaults and a runnable scheduler.
Keep 16-second freshness separate from those observations and platform delay.
Keep the 18.5-second teardown tail and current stronger pool admission/closure.
None of these estimates is a production SLO.

Deliver a new PR from `codex/operational-recovery-20261006` against current main.
Its description explicitly states that #254 already adopted #243's health bytes,
current `prepare_pool`/`admit_pool` lifecycle supersedes its pool hunks, and this
PR carries the remaining timing/topology guidance plus R1-R4. Reference #243
without changing its branch, force-pushing it or closing it. Do not restore its
old startup arithmetic, shutdown tail or historical completion receipts.

Planning sequences the runtime and execution owners, then one assembled
validation/review/delivery owner. It preserves current local typecheck-first
iteration, per-worktree target and Git-common lock, and runs no CPU-heavy
commands concurrently. One final candidate supplies matching build/relevant
tests and required real-database observation; the current classifier supplies
the remaining required CI route. Existing #254/#255 receipts remain qualified
historical input; requested completion also needs required CI for this PR's
actual head. No main merge or deployment is authorized here.

Reopen Research for changed baseline/source/provider evidence, System Design
for an infeasible mechanism or changed custody boundary, Rust Ownership for a
placement refinement, and Specification only for changed observable behavior
or requester scope. No user-owned decision is currently missing.
