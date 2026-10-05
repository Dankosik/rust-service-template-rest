# Process lifecycle technical design

Status: ready

Inputs: [Intent](../intent.md), [ready Specification](../spec.md), and
[Definition transition](../definition-transition.md). Base:
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`; specification SHA256
`30b2f0124032cc290c670fbb37c983a7dc62d14c37defed0d44d193b46d9764f`.
The source and resolved-library evidence is in [evidence](evidence.md).

## Decision and drivers

Keep lifecycle policy in the two composition roots. Retain the service's
`JoinSet`, the worker's `TaskTracker`, child cancellation tokens, existing
engine/consumer handles, and the adapters' native cleanup APIs. Repair resource
handover and outcome observation at those owners. No service registration API,
supervisor crate, general component registry, or shared lifecycle framework is
introduced. The concrete resources and stages in this design already exist.

L1-L8 require cancellation to stop admission, retained partial resources,
failure observation while startup/serving run, accurate completion, one whole
process deadline, cleanup after unwind, and an understandable integration path.
Defaults, configured values, provider semantics, request contracts and durable
job/message outcomes are fixed inputs. These drivers decide the alternatives:

| Choice | Viable alternative | Decisive constraint, accepted cost, reopen condition |
| --- | --- | --- |
| Resource slots outside the admission future; concrete adapter preparation where necessary | Rely on dropping the admission future and its local handles | Drop cannot flush the globally installed provider or drain a connected broker. Explicit slots add fields to existing owners. Reopen only if a native API provides equivalent observed cleanup on cancellation. |
| Existing JoinSet and tracker plus named failure observation and abort handles | Replace both with a supervisor or one common component type | Current APIs already own admission and lifetime; a replacement duplicates engine ownership and changes the worker registration surface. Small process-local wrappers carry names and observations only. Reopen for a real additional binary with an incompatible ownership need. |
| Standard panic boundary plus `futures-util::FutureExt::catch_unwind` | Spawn bootstrap as a Tokio task, or implement a custom polling wrapper | Registration may borrow caller state; moving it to a task imposes an unnecessary `'static` contract. The already resolved library catches polling unwinds without custom unsafe/pinning code. Reopen for an accepted abort-mode process contract. |
| An absolute deadline, with existing stage ceilings and an explicit runtime reserve | Renew a timeout at each stage or append SDK/runtime slack afterwards | Renewed timers violate L5. The cost is less time for later stages after earlier overruns. Reopen only with changed grace or stage policy. |
| A concrete retained messaging-admission owner | Keep a native client only inside `Messaging::connect` until topology passes | A dropped or unwinding topology future must leave a drainable client. The holder exposes admission and close, never publication/consumption. Reopen if async-nats supplies a suitable retained admission object. |

These are lifecycle repairs, not throughput optimizations. Duration claims below
are source-derived bounds; no performance improvement or live-provider result
is claimed.

## State and failure precedence

Each root has one partially initialized state, allocated before the guarded
bootstrap future. Service state extends the existing `Dependencies`/`Serving`
ownership with optional readiness and listener slots, an optional tracer
provider, its background owner, and an `admitted` flag. Worker state extends
`Resources` with optional readiness/provider and pending messaging admission;
its existing `Background` remains the task owner. Signals and the first stop
deadline outlive the future they can cancel.

An acquired resource is written to its slot synchronously before the next
fallible operation or suspension. Teardown takes only the resource needed by
the current stage. No future owns the sole cleanup handle across an unrelated
await. Local values whose applicable native cleanup is their synchronous Drop
(such as an unstarted engine or cache owner) can be dropped while unwinding;
that is not described as an observed asynchronous close.

The terminal cause is either a stop or a primary process failure. Admission,
registration, live task/listener failure and bootstrap unwind select failure
(`1`). Cleanup records its own result without replacing that cause. A stop
selects `0` only if every voting stage completed normally; forced work, panic,
failed join or unconfirmed completion selects `3`. Only the established
diagnostics scrape drain timeout is a non-voting forced close. A diagnostics
accept-loop panic/unexpected end is a failure like any other configured
listener. A cleanup error other than that timeout votes degraded.

Fault observation is sticky: an error already observed during startup is not
erased by a later stop or a successful admission result. At a readiness/claiming
transition, check pending stop and retained task/listener faults before logging
ready or starting work. In a select poll with no prior primary failure, stop
wins before polling more admission; this prevents new acquisition after an
observed stop. An acquisition already returned in an earlier poll remains in
its resource slot. Later cleanup panics do not turn a stop into a clean result.

## L1, L2, L6: startup, handover and unwind

Install signals before asynchronous provider I/O. Both roots run all later
startup in a single guarded operation raced against stop and the independent
background-failure receiver. On stop, retain `stop_at + grace_period` immediately,
drop the admission future, and enter teardown. On admission failure or unwind
without a preceding stop, start the same bounded teardown deadline at that
failure. Never restart an existing deadline. A signal observed by `Signals`
retains its first observation time; later signals expedite the existing waits
and do not replace that time.

The worker's existing `Signals` owner directly retains the native Unix signal
streams or Windows Ctrl-C receiver, as the service already does. Remove its
detached forwarding task and watch counter; there is no application signal pump
left to supervise or join. `wait` and nonblocking `pending` consume those same
cancellation-safe native receivers and record the first observed stop time.
Unexpected receiver closure is an explicit signal-owner failure, never an
infinite pending wait or an assertion that no signal exists. Before stop it
selects primary failure; during teardown it records degraded cleanup without
replacing an existing primary cause. Native OS/runtime signal internals remain
library-owned. Signals stay alive through cleanup and the runtime's final
shutdown. The final pending/fault check occurs in the root after
the admission future releases its borrows and before ready/claiming starts.

`AssertUnwindSafe(startup_or_serve).catch_unwind()` surrounds registration,
provider/subscriber/recorder installation, admission and the serving wait.
The assertion means mutated state is used only for teardown after failure;
normal startup never resumes. Store the tracer handle immediately after
installation, before subscriber or recorder installation. Thus every later
return and unwind reaches the same optional provider stage. Install the worker's
payload-withholding panic hook before invoking registration; catch diagnostics
never format its panic payload. The existing service hook behavior is retained.

Each independent teardown stage uses the same unwind conversion around its
future and continues to the next stage on an unwind, recording failure and
remaining owned resources as unconfirmed. Provider-stage unwind is itself a
failed explicit flush attempt. A final synchronous `std::panic::catch_unwind`
around `runtime.block_on` protects the explicit runtime shutdown call against
an unwind outside these expected boundaries. It is a last fallback, not proof
that async cleanup completed. Abort, double panic, process kill and runtime
starvation retain the exclusions in the specification.

Service listeners become independent optional fields in the root state instead
of local variables returned together as `Serving`. Store HTTP before binding
diagnostics, and diagnostics before binding gRPC. Retain readiness before
listener creation. A failed later bind drains all present listeners with live
connections; `admitted == false` skips propagation, not listener cleanup. Cache
startup should likewise place the lazy owner in `Dependencies` before its
optional probe wait. Object storage construction and other synchronous native
owners retain their current Drop semantics.

Worker pool, migration history, engine checks, messaging admission and initial
readiness all run inside the cancelable admission boundary. Start no engine or
consumer until the final admission/fault/stop checks pass. Retain each returned
`Started` handle immediately, rather than collecting several started engines
into a temporary vector before assignment. Existing `Started` and consumer
failure channels remain authoritative; no changes to claiming, attempt release,
settlement, leases or replay are selected.

### PostgreSQL admission

`infra-postgres::pool` separates synchronous lazy pool construction from bounded
session admission. The public adapter functions are `prepare_pool(&Dsn,
&PoolOptions) -> PgPool` and `admit_pool(&PgPool, &PoolOptions) ->
Result<(), ConnectError>`. Construction keeps all current native pool/session
options. Process roots store that native pool first, then await admission.

Admission acquires one connection and verifies the existing settings on that
same connection. Its absolute client deadline is `ACQUIRE_TIMEOUT +
STATEMENT_TIMEOUT = 3 s + 8 s = 11 s`; native acquire still has its 3 s ceiling.
This retains the existing acquire and statement allowances and cannot hang on
a peer that stops replying after acquire. The required settings and isolation
check are unchanged. A session-admission timeout has a distinct sanitized
`ConnectError` disposition from the existing acquire-timeout error.

Keep `connect` as the existing public convenience operation, implemented through
these same two functions, with no duplicate admission path. Its failure close
uses the existing adapter-local `ACQUIRE_TIMEOUT` as a 3 s cleanup ceiling:
the convenience call is bounded by 11 s admission plus 3 s cleanup. The current
path allows an initial 3 s acquire and another 3 s acquire before the 8 s
statement; using one acquisition frees that second allowance for failed
admission cleanup, without inventing another duration. It preserves
the primary refusal and records incomplete close separately. Actual service and
worker startup instead close their retained pool in the existing 5 s dependency
stage under the process deadline. SQLx owns a cancelled connection's return;
the retained vendored 5 s native return bound is unchanged. No schema, SQL text,
session defaults, SQLx version or vendored patch changes are needed.

### Messaging admission

Do not drop a `Messaging::connect` future that alone owns an already connected
native client. Add the concrete `MessagingStartup` owner in the existing
`infra-messaging::messaging` file. `Messaging::prepare(options, deadline,
cancel) -> Result<MessagingStartup, MessagingError>` constructs it without
network I/O; `admit(&mut self)` performs the
existing authentication/connect/topology sequence and returns admitted
`Messaging`; `close(self, deadline, cancel)` applies the existing native drain
and Closed-notification rule to any retained client.

Worker `Resources` stores this holder before awaiting `admit`. The native
client and its Closed receiver enter the holder immediately after connect,
before topology or consumer awaits. On success the client transfers to the
returned `Messaging`, which is stored synchronously; the empty holder is
removed. On error, signal or unwind it stays reachable by dependency cleanup.
An unadmitted holder offers no producer, probe or consumer API. Existing
`Messaging::connect` delegates to the holder and its bounded failure close for
current callers. Broker admission remains 5 s and native operations retain
their existing ceiling. No stream, consumer or delivery semantics change.
Consumer admission borrows the already retained `Messaging`, so cancellation
can drop that future without losing connection cleanup authority.

## L3, L4, L8: task and listener completion

### Process-owned tasks

Give every process-owned service spawn a static name using a private
`Background` owner in its existing shutdown module. It contains the current
`JoinSet<()>`, root token, and a sticky watch failure channel. Its narrow
`spawn(name, start)` wraps the future with a completion guard that reports an
unexpected return or panic, including a panic after cancellation. This is the
same responsibility as the worker's existing named `Background::spawn`.
Its independent receiver permits startup to borrow the task set for spawning
while the root concurrently observes failure; no lock is held across await.
Existing metrics, readiness, pool metrics/password refresh, JWT refresh,
webhook and idempotency tasks use this owner. It is private composition API,
not a public component registration system.

Retain the worker's tracker because engines spawn into it. Extend its existing
completion guard so cancellation suppresses only normal completion, never a
panic. Keep a sticky panic/failure result through join. Retain the returned
tasks' `AbortHandle`s with the tracker as completion authority for tasks created
by `Background::spawn`; only a short synchronous lock protects
this finite startup-time collection. Engines and the consumer retain their
native cancellation/join owners. No handle collection is added for their
library-internal tasks.

At background join, cancel the root and close the tracker, then join under the
existing 5 s service / 3 s worker ceiling. Inspect each JoinSet/JoinHandle result;
normal completion after cancellation is graceful, panic is failed, and an abort
acknowledgement is forced. Tracker emptiness proves completion of tracked
futures but does not erase the sticky panic result.

If cooperative join expires, request abort for controllable application async
tasks. Account for their acknowledgements before dependencies close, using time
from the existing 5 s dependency stage: compute that stage's absolute deadline
once, spend part on forced joins, and give dependency close only the remainder.
This adds no stage allowance. If acknowledgement cannot be established by that
deadline, record `unconfirmed`, request the remaining native dependency cleanup
with the time left (possibly zero), and continue. The process remains degraded
even if aborts eventually acknowledge. An abort request or tracker close is
never logged as `background_joined`. Running blocking work cannot be forced to
terminate; runtime shutdown does not improve this evidence.

### Listener ownership

`infra-http::Server` retains its accept JoinHandle, connection TaskTracker and
graceful token. Add a separate force token observed around the entire connection
future, including the wait after Hyper's graceful-shutdown request. Add a sticky
accept-loop fault observation (`AcceptFailure::{Ended, Panicked}`) and
`Server::failure()`, returning an independently owned future that waits for an
unexpected end. The accept task's guard reports a panic even while stopping;
an ordinary return after `stop_accepting` is expected and does not resolve the
fault future. This observer does not consume the JoinHandle or claim a join.

Immediately after storing each bound listener, its root spawns a named watcher
through the existing background owner. The watcher selects the listener fault
against its child cancellation token; a fault is logged with the listener name
and ends the watcher, thereby using the root's existing failure path. A normal
listener close leaves the watcher waiting until background cancellation. This
covers HTTP, diagnostics and gRPC from acquisition, including partial startup.

`Server::drain(budget)` creates its absolute deadline before any await. It stops
acceptance and requests graceful connection finish immediately, joins the
accept loop, closes the tracker once no accept can add work, and waits for
connections under that same deadline. An accept panic is recorded but does not
skip connection cleanup. At expiry, abort the accept task if still present,
cancel the force token, and report timed-out/unconfirmed work. A deadline with
no remaining time cannot promise those requests have completed. `Complete`
requires the accept join and empty closed connection tracker, with no failure.

Preserve `Drained::TimedOut { remaining_connections }` specifically for a
connection timeout after successful accept-loop completion. Add
`ServerError::AcceptTimeout` for unconfirmed accept-loop termination, distinct
from the existing observed `AcceptTask` join failure. On either accept error,
request connection cleanup and preserve the accept error as the drain result;
an open connection does not downgrade it to connection-only `TimedOut`.
Both roots exempt only diagnostics `Drained::TimedOut`; diagnostics
`AcceptTimeout`, `AcceptTask`, or another failed close votes degraded. A live
accept failure already observed before shutdown remains primary exit `1`.

Dropping a `Server` or its drain waiter requests stop, force cancellation and
accept abort; it performs no asynchronous wait and therefore claims no
completion. `Drained`/`ServerError` remain the root's drain outcomes, with the
new accept-timeout error making the diagnostics exception enforceable.
All connection cleanup remains in this adapter; roots never manipulate Hyper
or DNS tasks. Request/transport routing and wire contracts are unchanged.

### Supported integration path

Document service integration at `bootstrap::start` and its existing
profile-owned modules: retain required dependency handles in `Dependencies`,
admit them, then call the private named background spawn with a child token.
A process-owned future runs until that token is cancelled; return before then
fails the process. If a library's driver can finish normally on final client
drop, that client's owner must remain in the component's dependency slot until
shutdown; otherwise that driver belongs to the shorter operation, not the
process task set. Native library tasks continue to use native lifecycle APIs.

The worker keeps `Registration::spawn` and `Registration::shutdown` unchanged.
Registration is synchronous, local and prompt; it must not perform blocking
provider I/O. Its tasks receive the same early-return, panic, bounded-join and
forced/unconfirmed rules. No change to `service::run` or a new public service
registration callback is justified by current consumers.

## L5: one process deadline and unchanged numeric policy

At the first stop/failure, set `D = observed_at + http.grace_period`.
Async cleanup stages must finish waiting by `A = D - 1 s`; the existing runtime
allowance is reserved in advance. Every stage uses
`min(now + stage_ceiling, A)`. The synchronous entrypoint finally calls
`runtime.shutdown_timeout(min(1 s, D - now))`, saturating remaining durations
at zero. It performs this call on normal and caught-panic paths. The return
does not certify that already-running blocking jobs terminated.

| Process | Existing sequential allocation | Whole tail |
| --- | --- | --- |
| Service | diagnostics 2 s + background 5 s + dependencies 5 s + SDK flush 5 s | 17 s + SDK join slack 0.5 s + runtime 1 s = 18.5 s |
| Worker | forced attempt cleanup 2 s + listeners 2 s + background 3 s + dependencies 5 s + SDK flush 5 s | 17 s + SDK join slack 0.5 s + runtime 1 s = 18.5 s |

Readiness propagation remains inside the service's configured drain allocation;
HTTP and gRPC drain concurrently inside its remainder. Diagnostics retain their
later 2 s stage. Worker drain/forced-attempt cleanup and listener ordering remain
as above. Unadmitted startup skips propagation and job drain when no work
started, but processes every present listener/dependency. Repeated signals keep
their existing expedite meaning.

The telemetry adapter exports its existing 500 ms slack as one constant and
adds `shutdown_until(sdk_budget, deadline)`. This attempts
`shutdown_with_timeout(min(sdk_budget, remaining - slack))` on `spawn_blocking`,
and bounds the join by the supplied deadline, including the slack. Negative
remainders saturate at zero. The root supplies at most 5.5 s including slack,
clamped by `A`; the SDK allocation never exceeds 5 s. Even at zero remaining,
an installed provider receives the explicit shutdown attempt and an incomplete
result unless completion is actually observed. Keep `shutdown(budget)` for
existing non-process callers as a delegation to the same mechanism with
`now + budget + slack`; do not change one-shot command policy.

Both pre-runtime validators require `grace >= drain + 18.5 s`, including exact
equality, using checked/saturating duration arithmetic as appropriate for the
validated duration domain. The default needs `25 + 18.5 = 43.5 s`, leaving
1.5 s inside 45 s. No value in `env/config`, no configuration key and no
infrastructure grace setting changes. Budget logs and documentation must use
this arithmetic and must not label an incomplete stage successful.

## Placement and dependency decision

Placement is mechanically forced by existing resource owners; no Rust
Code / Ownership Design fork or complementary ownership panel is triggered.
The exact action map below is part of this System / Integration Design.
New types stay in existing files. It creates no crate, module or directory.

| Responsibility | Owning files and declarations | Call-path / cleanup / allowed dependency boundary | Proof owner and reopen condition |
| --- | --- | --- | --- |
| Service partial state, startup/serving guards and primary errors | `crates/service/src/bootstrap/mod.rs`: private state, `start`, `serve`, `run`, error variants | Composition only; optional resources handed to existing shutdown module. No feature/provider policy moves here. | Service unit/process fixtures; reopen System Design if owned state cannot survive cancellation/unwind. |
| Service named work and staged deadline/outcomes | `crates/service/src/bootstrap/shutdown.rs`: private `Background`, extended optional `Serving`/plan state, `Budget`, join/drain stages | Tokio/infra native handles only; one existing JoinSet and token. `RUNTIME_SHUTDOWN_TIMEOUT` remains root-local, made accessible to this validator without a second numeric owner. | Adjacent unit tests and service process proof; reopen if stage ordering/budgets must change. |
| Service task handover | Existing `bootstrap/authn.rs`, `postgres.rs`, `idempotency.rs`, `webhooks.rs`: use private named background owner; `cache.rs`: retain before probe | `pub(super)` composition helpers, same profile markers and provider types. PostgreSQL helper accepts the retained pool slot. No new externally exported service API. | Owner-adjacent coverage; reopen only for an incompatible retained profile. |
| Worker cancelable preparation and immediate handle retention | `crates/jobs-worker/src/bootstrap.rs`: `prepare`, observability install, pool/messaging/bind/start operations; `lib.rs`: outer unwind/runtime boundary | Keep `Registration` public contract; errors sanitized; resources retained before await. Operator CLI dispatch untouched. | Worker unit/process fixtures; reopen for changed registration meaning. |
| Worker completion, signals and cleanup | `crates/jobs-worker/src/shutdown.rs`: `Resources`, `Background`, guard, `Signals`, `Budget`, common plan | Existing tracker remains engine completion owner. Native abort handles for registered tasks; optional provider/readiness and pending messaging fields. Signals owns native streams directly and reports receiver closure. Replace incomplete `abort_startup` with common staged path rather than retaining two policies. | Adjacent tests; reopen for changes to engine finality. |
| Pool construction and bounded admission | `crates/infra-postgres/src/pool.rs`, re-exports in `lib.rs`: `prepare_pool`, `admit_pool`, existing `connect`, timeout error | Native `PgPool`, unchanged query/session policy. No composition-root dependency and no vendored change. | Adapter tests; observed database claims use existing persistence validation owner. Reopen if native cancellation behavior differs from source. |
| Retained broker admission | `crates/infra-messaging/src/messaging.rs`, re-export in `lib.rs`: public concrete `MessagingStartup`, `Messaging::prepare`, delegated `connect` | Private client/options fields; only admission and native close. No publication before admission or process signal/config ownership. | Adapter and worker proof; reopen if native client handover requires changed protocol semantics. |
| Listener fault observation and full drain bound | `crates/infra-http/src/server.rs`, re-exports in `lib.rs`: `AcceptFailure`, `Server::failure`, `ServerError::AcceptTimeout`, drain and force token | Tokio/Hyper lifecycle only. Roots own listener names and process disposition; only connection timeout after an accept join can receive the diagnostics exception. Public REST APIs untouched. | Adapter connection tests plus composition proof; reopen for incompatible drain contract. |
| Deadline-aware provider flush | `crates/infra-telemetry/src/traces.rs`, exports in `lib.rs`: public existing slack constant, `shutdown_until`, delegated `shutdown` | SDK controls flush; blocking job can outlive wait and remains incomplete. No process stage policy in adapter. | Existing telemetry fixtures; reopen if resolved SDK timeout semantics differ. |
| Existing async unwind mechanism | `crates/service/Cargo.toml`, `crates/jobs-worker/Cargo.toml`, generated `Cargo.lock` package edges | Add service normal `futures-util` with `std`; move worker's existing dependency outside jobs removal marker, preserving its existing features. Use workspace version 0.3, resolved 0.3.34; no upgrade/new package. | Matching workspace build/tests and normal dependency checks under Implementation; reopen if resolution changes beyond these edges. |

The library choice is reuse of an already resolved maintained dependency, not a
new mechanism search: `futures-util` 0.3.34 `std` provides `catch_unwind` directly;
the existing Tokio/Tokio-util task APIs supply cancellation/join. Standard
`catch_unwind` alone remains suitable for the synchronous runtime boundary, but
cannot catch a panic in a future merely by constructing that future. A new
supervisor crate or homegrown polling adaptor buys no missing capability.
There is no dependency upgrade reason. Default-features remain disabled;
the service's gRPC-only dev dependency entry is removed/reconciled because the
normal dependency covers it. Cargo updates the lock's dependency edges; nobody
edits it by hand. This closes the `rust-dependencies` decision before execution.

Documentation changes belong to the current owners:
`docs/architecture/runtime-lifecycle.md` (integration, startup, stage/outcome and
blocking limits), `docs/configuration-source-policy.md` (18.5 s arithmetic),
`docs/architecture/persistence.md` (pool admission and failed close bounds), and
the worker registration rustdoc in `lib.rs`. Reconcile any directly contradictory
statements in retained profile guides through those same semantics; this is not
a documentation redesign. No generated API/schema source changes are selected.

## Template compatibility and proving boundaries

Keep profile markers around resource fields, imports, spawns and cleanup for
PostgreSQL, auth, cache, object storage, idempotency, webhooks, gRPC, jobs,
messaging and outbox. The service's new private background owner and async
unwind dependency are unconditional. Worker async unwind support is unconditional
while the worker exists, including messaging-only output. Profile removal must
not leave a method referring to a removed provider type. Empty/absent resources
skip only their stage work, not the common exit path. The crate graph, feature
boundaries, gRPC/REST and durable semantics retain their authorities.

There is one source revision for both binaries and adapters, and no storage or
mixed-version protocol transition. A normal replacement deployment suffices;
deployment itself is outside this phase and the authorized PR deliverable.
No runtime migration or compatibility shim is introduced. Existing public
adapter convenience calls delegate to the repaired implementation because they
have actual callers; their removal is not required by this lifecycle repair.

| Required trace | Enforcing boundary | Evidence that can falsify the decision |
| --- | --- | --- |
| Stop during pending SQL/history/engine/broker/readiness admission -> no ready/claim -> retained cleanup -> original deadline | Root cancel select, resource slots, adapter deadlines | Controlled pending admission, retained owner observation, elapsed absolute deadline and terminal result; database claims require existing real-database proof. |
| Subscriber/recorder/registration failure or bootstrap unwind -> provider attempt and resource stages -> runtime shutdown -> 1 | Optional provider slot, guarded bootstrap/common teardown, entrypoint runtime call | Existing unit/process fixtures can observe cleanup and exit without real OTLP; live collector flush is a separate claim. |
| Later bind fails -> earlier connected listener drains -> telemetry -> 1 | Immediate listener slots and full-budget `Server::drain` | Connected peer and partial bind failure must prove release, not only an event string. |
| Required task/accept loop ends or panics -> immediate failure -> readiness withdrawal -> cleanup | Sticky named failure observation plus root select | Controlled completion before ready/while serving, and panic after cancellation; same-task normal cancelled completion must stay graceful. |
| Signal delivery remains observable through process lifetime | Existing Signals directly owns native receivers; no detached forwarding task | Receiver closure is a failure rather than permanent pending; repeated signals retain expedite behavior and the first deadline. |
| Diagnostics accept completion is unconfirmed while a scrape remains open -> degraded | Distinct accept-timeout error, connection-only diagnostics exception | The same open scrape must not hide the absence of an accept join; confirmed accept plus scrape-only timeout retains its exception. |
| Join expires -> abort requested -> acknowledged or unconfirmed -> dependencies -> final degraded | Native joins/tracker plus fixed dependency-stage deadline | A task that ignores cooperative cancellation; join-result inspection and dependency-close ordering. An abort request alone fails the oracle. |
| SDK slack and runtime tail -> `drain + 18.5 s` validation and bounded wait | Root budget and provider absolute deadline | Equality and below-boundary arithmetic; controlled late stages; runtime return alone cannot prove blocking-work termination. |
| Retained/absent profiles compile with same contract | Existing markers and template validation owner | Existing generated-profile checks; no new matrix or test harness is defined here. |

Implementation chooses concrete cases and commands from these behavioral
boundaries and the repository Validation budget; this document is not a test
plan. Reuse existing fixtures and validation owners. Source analysis of NATS,
SQLx, SDK and blocking behavior is not live provider evidence. The earlier macOS
run of Linux-only gRPC process proof executed zero cases; do not call it a pass.

No open product, ownership, API, budget, dependency or infrastructure decision
is delegated to Planning. Reopen this design if a native API cannot support the
specified retained ownership or completion semantics; reopen Specification only
for a necessary observable behavior change. Normal selection of tests and
implementation-local naming refinements do not reopen Technical Design.
