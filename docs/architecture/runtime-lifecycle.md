# Runtime Lifecycle

Load for startup, readiness, drain, shutdown, exit codes, or
process-resource ownership. `crates/service/src/bootstrap` is the code:
`mod.rs` is the startup order, each retained profile keeps its step in a
module beside it, and `shutdown.rs` is the teardown. The process-test surface
is `crates/service/tests`; the current validation receipt owns what has been
proved against a built binary.

## Startup

1. `main` hands `argv` to `bootstrap::run`, which parses the loader flags
   (`LoadOptions::parse_from`; `--help` exits `0`, a flag error exits `2`),
   loads the configuration snapshot, and validates the grace budget before a
   runtime exists. A later failure here prints one readable message to stderr
   and exits `1`.
2. Inside the Tokio runtime, the signal streams are installed first, so a
   `SIGTERM` that arrives during startup is handled rather than killing the
   process. Guarded startup races those streams and required-task failures.
   A stop cancels pending admission, retains its first observed deadline, and
   starts cleanup. No new listener or task is admitted after that stop;
   resources already acquired remain in the outer partial-startup state.
3. The tracer provider is installed and immediately retained for cleanup,
   then the subscriber and its `LoggerGuard` (so SDK diagnostics are observed
   through the finite numeric boundary). The guard is retained immediately,
   including across cancellation and unwind. The payload-free panic hook and
   Prometheus recorder follow; the startup record
   (`service_starting`) carries the non-secret facts an operator needs:
   `app.env`, `app.version`, `app.commit`, the listeners, the budgets, the
   log level, and the exporter state (`initialized`, `disabled`, `degraded`).
4. Background tasks (metrics upkeep, Tokio runtime metrics, the readiness
   refresher) use the private named `Background` owner: one `JoinSet`, child
   `CancellationToken`s, and a sticky failure observer.
5. Admit the dependencies retained by the local profile before accepting
   traffic; bootstrap owns their readiness registration and cleanup.
6. The API contract comes from `service::api::contract()`: the route tree
   and its OpenAPI document as one value. Assembly is pure, so it runs
   before readiness admission.
7. Readiness admission: the refresher evaluates every registered probe once
   under `health.probe_budget`; a failure is a startup failure (exit
   `1`). Without a selected profile the set is empty and admission proves
   the mechanism. The route tree is then given the application state
   (`service::AppState`), wrapped by `infra_http::harden`, and bound by the bounded
   `Server`; the diagnostics listener binds second when
   `observability.metrics.addr` is set. Each bound listener is retained
   immediately, before the next bind, and its accept loop is watched.
   `service_ready` is logged only after all required binds and a final check
   for pending stop and retained faults; the platform's first `/health/ready`
   poll answers from the admission evaluation.

<!-- template:begin authn:docs-lifecycle-authn -->
With authentication retained, bootstrap prepares the selected verifier before
finalizing the complete contract and binding the server. Disabled runtime mode
uses public finalization without a verifier; protected policy refuses startup.
Bootstrap validates introspection cache options even when caching is disabled,
before any listener or provider I/O. Introspection construction does no provider I/O; JWT startup discovers metadata
and installs a usable JWKS inside its bounded startup budget. A JWT refresh
future joins the existing background `JoinSet` with a child cancellation token.
Authentication neither adds a readiness probe nor changes public health behavior.
<!-- template:end authn:docs-lifecycle-authn -->
<!-- template:begin outbound-http:docs-lifecycle-outbound -->
A retained [outbound client](../outbound-http.md) is inert until a concrete
provider is wired. Operations are caller-owned futures; dropping one ends its
exchange. Resolver, connection-pool, and HTTP library tasks are
library-owned; bootstrap neither gives them a task set or token nor joins them in a
shutdown stage. The client adds no readiness probe or teardown stage. JWT refresh
remains the separate process-owned task that bootstrap cancels and joins.
<!-- template:end outbound-http:docs-lifecycle-outbound -->
<!-- template:begin outbound-auth:docs-lifecycle-outbound-auth -->
The retained OAuth2 profile is inert until a concrete integration prepares an
authenticated client and its `RefreshDriver`. Token acquisition is
request-owned work and consumes the caller deadline. The integration drives
`driver.run(existing_shutdown)` while credential/client clones are live and
awaits normal completion after cancellation in the existing background-join
stage before dependencies drop. A process-lifetime integration retains its
credential/client owner in `Dependencies` until that stage; returning before
cancellation is a process fault. Final credential/client-owner drop also ends
the driver for a shorter integration lifetime, which stays outside the process
task set. Dropping the driver closes any surviving clients, whose subsequent
calls fail through the normal closed-owner path. This adds no readiness probe,
bootstrap provider call, or teardown stage.
<!-- template:end outbound-auth:docs-lifecycle-outbound-auth -->


<!-- template:begin postgres:docs-lifecycle-postgres-startup -->
With the PostgreSQL profile retained and `postgres.enabled`, bootstrap admits
the DSN and retains a lazily prepared native pool before awaiting admission.
One five-second client deadline covers connection acquisition and the complete
session-settings readback; the native acquire ceiling remains three seconds
inside that deadline. Only admitted pools record `postgres_pool_opened`. On
rejection or cancellation, the retained pool closes under the root's normal
shutdown deadline. The standalone `connect` convenience path instead allows
five seconds for rejection cleanup and preserves the original admission error.
These waits require a runnable scheduler and do not create a whole-bootstrap
deadline. Bootstrap then verifies, in a separate read-only check bounded to
five seconds including acquire, that every embedded migration is applied with
its checksum: pending or divergent history is a sanitized startup failure,
while versions from a later release are admitted so a rollback still starts.
The probe joins readiness, the pool gauge task joins the background set, and an
unreachable database is a startup failure
([Persistence](persistence.md)).
<!-- template:end postgres:docs-lifecycle-postgres-startup -->
<!-- template:begin http-idempotency:docs-lifecycle-http-idempotency -->
With the HTTP idempotency profile retained, each `Composer::route` validates
its route locally and returns `CompositionError` before serving on failure.
After all routes compose, `finish` activates their count without inspecting
the assembled document. An `Active` result requires `postgres.enabled`, a set
retention, admitted migration history, and a writable session, then starts the boundary before
admission continues; an `Inactive` result does nothing further. While
active, a background cleanup task joins the existing background `JoinSet` with a
child cancellation token and is cancelled and joined in the existing
background-join shutdown stage, beside the other background tasks. It never
adds a readiness probe or a shutdown stage of its own.
<!-- template:end http-idempotency:docs-lifecycle-http-idempotency -->

Configuration and dependency admission precede traffic acceptance.
Bootstrap owns one partially initialized state outside the cancelable startup
future. Each dependency, listener, tracer provider, logger guard and task handle is retained
before the next fallible operation or await. Failed, stopped or unwinding startup
runs the same staged cleanup over every present resource; startup that never
became ready skips readiness propagation, not listener drain. A later bind
failure therefore drains earlier listeners and their live connections. Every
installed provider receives a bounded explicit shutdown attempt, including a
subscriber or recorder installation failure.

Required background work must run until its cancellation token fires. A normal
early return or panic is a process fault, observed during startup and serving;
a configured listener's unexpected accept-loop end or panic has the same effect.
The fault remains sticky: a later stop or successful admission cannot erase it,
and a retained startup fault prevents the ready transition. Normal completion
after cancellation is expected; an explicit error from a registered feature
manager or a panic during cancellation/join still fails cleanup. Diagnostics name the task, listener or stage without exposing secrets.

Registration, bootstrap and the serving wait have an unwind boundary. Caught
unwind proceeds only to teardown and exit `1`; normal startup never resumes.
Each cleanup stage is guarded so one unwind records failed or unconfirmed work
and later stages still run. The outer synchronous guard preserves the explicit
runtime shutdown call, but cannot prove asynchronous cleanup completed. Abort-mode
panic, double panic, process kill and runtime starvation are outside this bound.

### Integrating process-owned work

Wire service work in `bootstrap::start` or its existing profile-owned module.
Keep dependency handles in `Dependencies` before awaiting admission, then use
private `Background::spawn(name, future)` with a static task name and a child of
its shutdown token. The future returns normally only after cancellation; an early
return or any panic stops the process. Bootstrap observes failures, shutdown
cancels and joins the work, and the dependency-close stage releases its retained
dependencies afterwards. A library driver that also exits on final client drop
needs that client retained until shutdown. Shorter operation-owned work stays
with its operation; native library tasks keep their native lifecycle owners.

<!-- template:begin grpc:docs-lifecycle-grpc-registration -->
For a derived service's existing gRPC registration callback, bootstrap lends
`BackgroundRegistration` as its third argument. Its `spawn` factory receives a
child cancellation token and a static-name `BackgroundFailureReporter`, and
returns one fallible process-lifetime future into the same private `Background`.
The borrowed handle cannot escape into handlers or change the runtime, task set,
root cancellation or deadline. The default `run` remains inert.

On requested cancellation a feature manager closes its admission and joins all
admitted work before returning `Ok`. On live work failure it closes admission,
calls the reporter immediately, then retains join custody until every admitted
operation retires before returning `Err`. Reporting only changes the existing
sticky failure latch; it neither completes the manager nor starts another
channel or task. The root starts its failure transition while that manager
remains tracked. An explicit error is retained even after cancellation, with
only the static name observed and no formatting of the error payload. A live
failure is primary exit `1`; failed requested-stop cleanup votes exit `3`.
Existing primary failure always wins. A manager that panics cannot establish
that nested work ended; forced or unconfirmed joining remains degraded.

Registered managers are retained before registration returns or listeners bind.
If a later startup step refuses or unwinds, the same teardown joins them. Their
join and any abort acknowledgement spend the existing background/dependency
allocations; the capability creates no feature-specific timeout. The
[registration guide](../grpc.md#register-a-service) describes the Rust
callback change and keeps the generated/wire contract unchanged.
<!-- template:end grpc:docs-lifecycle-grpc-registration -->

## Business-work admission and lifetime

Keep short, bounded synchronous computation inline when its worst-case input
and cost fit the caller's budget. An `async` function does not move computation
off a Tokio worker. For blocking I/O, prefer an existing asynchronous API;
otherwise use `spawn_blocking` for a finite operation. Sustained CPU work needs
an explicit concurrency bound and a concrete workload decision before selecting
a separately bounded CPU executor. The template supplies no general CPU pool
and does not require blanket JSON or cryptographic offload.

Admit blocking or CPU-heavy work before submission, not inside the submitted
closure. Bound waiting inputs and choose admission wait or rejection from the
feature's accepted workload and deadline. Move the owned capacity permit into
the actual execution so it covers both queued and running work until completion
or unwind. A request timeout or cancelled waiter must not release that capacity
while its closure still runs; otherwise repeated timeouts bypass the bound.
Do not leave an unbounded queue behind a bounded thread count.

Give submitted work a lifetime owner that retains completion and panic
observation after the request or job waiter ends. On cancellation, request
cooperative stopping where the operation supports it; loops check that request
between bounded chunks. Dropping or aborting a handle, or timing out its await,
does not stop a started blocking closure. At shutdown the owner closes
admission, requests cancellation, and observes completion within the existing
budget, accounting for unfinished execution without claiming it stopped.
`Runtime::shutdown_timeout` limits the wait, not execution: residual closures
can continue until process exit, and an implicit runtime drop can wait
indefinitely. These are the existing
[Tokio blocking-task semantics](https://docs.rs/tokio/1.53.1/tokio/task/fn.spawn_blocking.html).

Tokio's shared blocking pool also serves library filesystem and DNS work.
Sustained CPU demand can compete with those operations; adding Tokio workers
or raising the blocking-thread limit does not provide admission or progress
guarantees. Select CPU isolation only when an actual workload justifies its
capacity and lifecycle policy, rather than deriving another pool from the host
core count.

A loop over immediately ready futures can keep running through every `.await`.
Bound work per poll or iteration and return control cooperatively; a custom
future returning `Pending` after consuming its quantum must arrange a wakeup.
Cooperative yielding neither makes blocking calls nonblocking nor reserves CPU
for another task. Apply the same actual-lifetime rule to job handlers: a retry
may overlap a previous attempt's blocking work, so cancellation never substitutes
for the job's existing effect and fencing contract.

## Readiness and liveness

`/health/live` is process-only and always `200 ok` while the process runs.
Both probe routes are admitted without an in-flight permit, so a service
shedding at `http.max_in_flight` still answers them; a probe's connection
still counts toward `http.max_connections`, and a connection over that cap
is closed without an answer.

The diagnostics listener therefore serves `GET /health/live` as well. It has
its own connection cap and no caller traffic, so a full application listener
cannot fail liveness there. Point the platform's liveness probe at the
diagnostics port and its readiness probe at the application port: an
instance that cannot accept a connection should leave rotation, not be
restarted. Without a diagnostics listener (`observability.metrics.addr`
empty), liveness is served on the application listener only and shares its
cap.

`/health/ready` reads the cached verdict published by the `health` crate's
refresher: ready after every probe passed; a failure is published at once
while the instance is not ready yet. A fresh Ready publication absorbs failures
below `health.failure_threshold`. At each new completion, the prior publication
must still be fresh to absorb that failure: a round that starts fresh and
finishes after expiry cannot revive Ready. A successful round restores Ready
and resets the failure streak. Readers apply `Draining > NotEvaluated > Stale >
published verdict`; age equal to the stale bound remains fresh. The bound is
`probe_budget + 3 * max(interval, probe_budget)`, currently 16 seconds. A stalled
refresher therefore fails closed even if its task has not ended. An ended or
panicked task instead follows bootstrap supervision above. Teardown's drain flag
wins immediately and a completed refresh cannot undo it. The handler never runs
a probe, so its latency is independent of dependency latency, and an
unauthenticated caller cannot turn a probe request into a dependency
round-trip. Every probe of one check runs at the same time under
`health.probe_budget`, so a slow dependency does not spend another probe's
budget; the verdict names the first probe, in registration order, that
failed or ran out of it.

Dependency probes are a trade-off. Registering a shared dependency such as
PostgreSQL makes every instance unready together when that dependency fails,
so the load balancer has no backend and clients see its error instead of the
service's own `503` Problem. The template registers the probes because an
instance that cannot reach its database cannot serve any route; a service
whose routes degrade gracefully without a dependency should leave that
dependency's probe out and watch it through metrics.

The refresher reports itself through five metrics and four log events.
`readiness_checks_total{outcome}` (`ok`, `failed`, `timed_out`) counts completed
checks. `readiness_probe_checks_total{probe,outcome}` counts each probe's own
outcome, including failures absorbed by the threshold. `readiness_ready` is the
published verdict: `1` while ready, `0` before the first check, while the probe
verdict is withdrawn, and from drain start. It is not the time-adjusted endpoint
answer: a stalled refresher can leave this gauge at 1.

`readiness_last_completed_timestamp_seconds` dates the last completed check in
Unix seconds; `0` means no check has completed, and `NaN` means a completion
occurred but the wall clock could not supply a positive Unix timestamp.
Success, failure and timeout all update it, including a completion during drain;
an in-progress or cancelled check and drain alone do not. The existing publisher
writes it without a second monitoring loop. `readiness_stale_after_seconds`
exposes the policy's stale bound (16 with current defaults).

With a current successful scrape and comparable clocks, timestamp `T > 0`,
bound `B` and observer Unix time `N`, `0 <= N - T <= B` means fresh and
`N - T > B` means expired. Prometheus can recognize expiry even if refresh has
stopped and no health endpoint is polled:

```promql
(time() - readiness_last_completed_timestamp_seconds > readiness_stale_after_seconds)
and (readiness_last_completed_timestamp_seconds > 0)
```

Match the usual per-target labels. Freshness does not imply probe success.
Application clock jumps and collector skew can overstate or understate age;
a future timestamp or NaN means unknown freshness. Missing samples or a failed
scrape mean unknown observation, not NotEvaluated: consider scrape health and
sample recency too. Scrape and evaluation cadence add delay. Individual gauge
writes are not an atomic snapshot, so a scrape crossing publication can mix
adjacent completions. None of these metrics controls endpoint readiness; its
reader uses monotonic time and the precedence above.

`readiness_lost` and `readiness_recovered` log a published flip,
`readiness_check_failed` logs an absorbed failure, and `readiness_refresh_late`
logs a completion after its predecessor expired. Time passing alone does not
emit a transition log; the timestamp exposes that stale interval.

The failure threshold and the platform's own probe threshold add up. With the
defaults, a dependency that fails fast withdraws readiness within about `6s`
(three `2s` rounds) and one that hangs within about `12s` (three rounds at
the `4s` budget); the platform then counts its own failures on top
(Kubernetes: `periodSeconds` times `failureThreshold`). Size the platform
threshold for detection, not for smoothing: the service already absorbs a
single slow round-trip. The default budget exceeds the PostgreSQL acquire
budget (`3s`), so a saturated pool is reported by the probe's own error
rather than as a budget timeout.

## Shutdown

Every stage draws from one deadline started at the first observed stop or
primary failure (`http.grace_period`, default `45s`). The final 1 s is reserved
for runtime shutdown; each asynchronous stage uses the lesser of its ceiling
and the remaining time before that reserve. A later stage cannot renew time
spent by an earlier one, and repeated signals expedite cleanup without moving
the deadline.

| Stage | Budget | Observable record |
| --- | --- | --- |
| Readiness off (drain flag) | immediate | `readiness_disabled` |
| Propagation delay: keep serving while load balancers notice | `http.readiness_propagation_delay` (`15s`); a second signal skips it | `readiness_propagation_wait` |
| HTTP drain: stop accepting, finish in-flight requests | `http.drain_timeout` minus the delay (`10s`) | `drain_started`, then `drain_completed`, or `shutdown_forced` with `remaining` connections |
| Diagnostics listener close | `2s` | `diagnostics_stopped` or `diagnostics_forced` |
| Cancel and join background tasks | `5s` | `background_joined` only after confirmed normal completion; forced, failed or unconfirmed work is recorded separately |
| Account for forced background completion, then close selected dependencies | `5s` shared absolute deadline | Failed or unconfirmed completion votes `degraded` |
| Close trace provider and local logger | Shared `5s` telemetry allowance plus `0.5s` SDK join slack, inside remaining process time | `trace_shutdown_completed` or `trace_shutdown_incomplete`, then `shutdown_finishing` with `logger_pending=true` |

The trace provider and local logger consume one telemetry-stage deadline,
clamped to the process time remaining before the runtime reserve. They share
five seconds of work allowance; the existing 500 ms SDK join slack stays within
the complete 18.5-second tail. Logger closure gets the remaining stage time,
never a new five-second window. The composition root keeps the logger live
through trace cleanup and final process records and reports primary failures
before closing logger admission.

`trace_shutdown_completed` means provider shutdown and join completed with no
observed final-drain failure, and records `delivery_confirmed=false`. Any export
failure completing after the drain marker remains latched, including an export
already in flight and a failure followed by a successful batch. Receiver
acceptance, persistence and queryability remain unconfirmed. Finite reason bits
make an observed final failure incomplete even if the SDK shutdown call returns
success.

`shutdown_finishing` carries the known stage outcome and `logger_pending=true`;
it cannot confirm its own delivery. No `shutdown_completed` or final scrape is
promised after logger/diagnostics closure. Logger shutdown uses a bounded
runtime-independent completion wait for its one OS writer. Its destructor
closes admission and detaches without I/O or another wait; a blocked writer and
its bounded queue may survive until process exit. There is no synchronous
post-install fallback output. Final-record admission loss or a final
write/flush/drain/join failure is incomplete and votes in the typed exit result.
Earlier runtime log drops alone do not change a later clean stop into failure.

<!-- template:begin postgres:docs-lifecycle-postgres-close -->
The retained PostgreSQL pool closes in the dependency-close stage and records
`postgres_pool_closed`.

The finite migrator preserves its primary result (`0` or `1`) even when final
logging is incomplete, and emits exactly one `migration_run` business terminal
record. Its existing one-second cleanup allowance covers runtime termination,
that record and explicit logger drain under one deadline, including runtime
construction failure before a runtime exists. An incomplete telemetry result
never replays a committed migration.
<!-- template:end postgres:docs-lifecycle-postgres-close -->

<!-- template:begin oidc-jwt:docs-lifecycle-jwt-refresh -->
JWT refresh is periodic and may be triggered by an unknown key or a kid-less
signature miss; one shared fetch is coalesced and canceled/joined with background work during shutdown.
Failed refresh keeps the last usable keys. There is no maximum cached-key age
and this is not an immediate-revocation mechanism.
<!-- template:end oidc-jwt:docs-lifecycle-jwt-refresh -->

The tail is 17 s of stage ceilings plus the existing 0.5 s SDK join slack and
1 s runtime allowance: **18.5 s**. `validate_grace_budget` accepts equality and
refuses `grace_period < drain_timeout + 18.5s` before building the runtime.
The default required bound is `25 + 18.5 = 43.5s` inside `45s`, leaving 1.5 s.
No duration default changes. Platform grace derives from
[Configuration Source Policy](../configuration-source-policy.md#runtime-budget-policy).

Listener drain spends one supplied deadline on both accept-loop join and
connection completion. Expiry requests accept abort and connection cleanup;
only a successful accept join plus an empty closed connection tracker establishes
completion. Dropping a server or its drain waiter requests cleanup without
confirming it. Only a diagnostics connection timeout after successful accept
completion is exempt from a degraded vote; an accept failure or unconfirmed
accept termination is not exempt, even with an open scrape.

When cooperative background join expires, request abort of controllable async
work and account for acknowledgement using the dependency stage's existing
absolute deadline. Dependencies get only its remainder. An acknowledged abort
is forced; missing acknowledgement is unconfirmed. Neither is `background_joined`,
and both keep the process degraded even if work later completes. A panic during
join remains failed. Stage records distinguish completed, forced, failed and
unconfirmed work; cleanup errors never replace a primary process failure.

After the stages, the entrypoint calls `Runtime::shutdown_timeout` with at most
1 s and no more than the time left to the original deadline, including on caught
panic paths. Running blocking work may outlive that wait. Neither abort requests
nor runtime shutdown return prove all work has terminated; these are bounded
teardown waits, not hard real-time scheduling guarantees.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Stop signal and every outcome-voting stage, including final trace and logger cleanup, completed normally |
| `3` | Stop signal with forced drain, background panic or failed join, or incomplete outcome-voting cleanup, including final trace/logger failure or final-record loss |
| `1` | Configuration, admission or startup failure, caught bootstrap unwind, or unexpected live task/listener failure; cleanup runs without replacing this cause |

`--help` exits `0`. `--version` is not a loader flag: identity is
`BuildInfo` / `app.version`. `process::exit` is never called, so
destructors run.
<!-- template:begin jobs:docs-lifecycle-jobs-worker -->

## Jobs worker

The code is `crates/jobs-worker/src/lib.rs` (`run`, the synchronous startup
phases, and `exit_code`, the one exit-code mapping) and
`crates/jobs-worker/src/{bootstrap,shutdown}.rs` (the asynchronous startup
with its refusals; signals, the stage budget, and the common shutdown plan).
The process proof is `crates/jobs-worker/tests/process.rs`
for the shipped binary, and the test-only `jobs-worker-fixture` suite in
`test/tests/jobs/`.

**Ordinary startup.** With no subcommand, each refusal below exits `1`,
except step 1, which exits `2`.

| Step | What | Refusal (exit 1) |
| --- | --- | --- |
| 1 | `WorkerArgs` flattens `LoadOptions` (`--help` exits `0`) | clap usage error (exit 2) |
| 2 | `service_config::load` (same sources, precedence, unknown-key and secret rules as the service) | `configuration is invalid: ...` |
| 3 | `shutdown::validate_grace_budget(&config.http)` | `http.grace_period (..) must be >= http.drain_timeout (..) plus the 18.5s jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush, SDK join slack, runtime shutdown)` |
| 4 | Build the multi-thread runtime | `build tokio runtime: ...` |
| 5 | Install `Signals` (SIGINT, then SIGTERM) | `install stop signal handlers: ...` |
| 6 | Payload-free panic hook, then tracer provider with the worker identity, subscriber and recorder; the hook records the panic's file, line and column plus admitted correlation, never its payload or thread name | the telemetry errors, as in the service |
| 7 | Register optional jobs and typed-message capabilities through `register(&mut registration)`, which fills `Registration::jobs` and `Registration::messages`; validate each nonempty registry. A composition with no retained capability refuses after configuration is loaded | `job kind registration failed: ...`; `job kinds are invalid: ...`; `typed message handlers are invalid: ...`; `no job kind or typed message handler is registered: register this service's retained capabilities in crates/jobs-worker/src/main.rs` |
| 8 | `jobs_worker_starting` record; metrics upkeep and Tokio runtime metrics join the tracker | |
| 9 | After registration, determine whether retained capabilities need PostgreSQL; validate `postgres.enabled` and mode-aware pool capacity, then admit the DSN/pool and migration history | `postgres.enabled must be true to run the jobs worker`; capacity, DSN, pool, or history refusal |
| 10 | When messaging or outbox is retained, validate producer/consumer configuration, connect NATS under its startup budget, and admit a consumer only for registered typed handlers | messaging configuration, connection, topology, bounds, or consumer refusal |
| 11 | Construct every required ordinary and reserved publication `Engine`, then run each `Engine::check_startup` | `jobs startup check: ...` |
| 12 | Bind the health listener (`http.addr`), then the diagnostics listener (`observability.metrics.addr`, when set), which serves `/metrics` and `GET /health/live`; `http listener bound`, `diagnostics listener bound` | `bind http listener ...` |
| 13 | Readiness admission (`refresh`, then cached verdict over retained PostgreSQL and messaging probes), raced against stop signals | `startup admission: ...` |
| 14 | Only after admission, start every `Engine` and the admitted consumer; `jobs_claiming_started` and `messaging_consuming_started` | |
| 15 | Refresher task; `jobs_worker_ready` | |
| 16 | Wait for a stop signal or an engine, consumer, named background-task or listener fault | |

Registration follows configuration and constructs local registries; it must
return promptly and perform no blocking provider I/O. The tracer provider and
any registration-spawned task are already lifecycle resources. `Signals`
directly retains native receivers for the process lifetime, including cleanup
and runtime shutdown; no detached forwarding task owns delivery. Unexpected
receiver closure is a signal-owner failure: before stop it selects exit `1`;
during cleanup it votes degraded without replacing an existing primary failure.

All asynchronous pool/session/history checks, engine checks, broker admission
and readiness run inside the guarded cancelable startup boundary. Stop ends
further admission and preserves its original deadline. Each acquired resource
and each started engine/consumer handle is retained immediately. Pending stop
and sticky faults are checked before starting engines or consumers and before
recording ready. Before admission, `/health/ready` answers `503 not ready`.
Every later refusal, stop or caught unwind uses the common staged plan over the
resources present, including bound listeners and an explicit provider flush.
A startup that has not begun work skips its job drain and readiness propagation.
Worker panic diagnostics retain file, line and column plus admitted correlation,
never the payload, thread name or backtrace.

**Readiness.** `/health/ready` uses the service's cached-verdict semantics
with the retained PostgreSQL and messaging probes. The worker is ready only
after admission has started every required engine and consumer. It is not ready
at the first stop trigger. The health
listener keeps answering, not ready, through the drain, and closes with the
listeners after it.

**Identity.** `{service_name}-jobs-worker`, from
`observability.otel.service_name`, is the OpenTelemetry `service.name`, the
tracer name, and the startup record's `service.name`. `application_name` is
the longest prefix of the service name of at most 51 bytes that ends on a
character boundary, followed by `-jobs-worker` (at most 63 bytes,
PostgreSQL's limit). `service.instance.id`, version, commit, and environment
follow the service's rules. No configuration key controls it.

**Shutdown.** One deadline, `http.grace_period`, starts at the first observed
stop or primary failure. Each stage takes the lesser of its ceiling and the time
left before the 1 s runtime reserve. A degraded stage makes a signal-stop exit
`3`; a primary process failure remains `1`.

| Stage | Ceiling | Records | Votes degraded (exit 3) when |
| --- | --- | --- | --- |
| Readiness off; stop every jobs claim loop and messaging pull | immediate | `shutdown_started`, `readiness_disabled`, `claiming_stopped` (in-flight count), `messaging_pulls_stopped` | never; admitted work may settle under its existing backstop |
| Drain all started engines and the consumer; a second signal ends it | `http.drain_timeout` (25 s); no propagation delay | `drain_started`, then `drain_completed` or `drain_forced` (in-flight attempts, reason `budget`, `second_signal`, or `messaging`) | any engine or consumer drain does not finish inside the shared budget |
| Only after a forced drain: finish every engine attempt and abort/finish the consumer | 2 s | `attempts_finished` (known results, cancelled handlers, acknowledged releases, uncertainty) | cleanup overrun |
| Close the health listener and the diagnostics listener concurrently | 2 s | `health_listener_stopped`, `diagnostics_stopped`; `diagnostics_forced` for a scrape overrun | health close fails or overruns, or diagnostics accept completion fails/is unconfirmed; only diagnostics connection timeout after accept completion is exempt |
| Cancel and join background tasks (claim loops, retention, sampler, metrics, refresher) | 3 s | `background_joined` only on confirmed normal completion | the join overruns or a task panics/fails |
| Account for forced background completion, then close retained pool and messaging dependency | 5 s shared absolute deadline | confirmed closes or forced/failed/unconfirmed outcome | forced work or either close is incomplete |
| Close trace provider and local logger | Shared 5 s telemetry allowance plus 0.5 s SDK join slack, inside remaining process time | `trace_shutdown_completed`/`trace_shutdown_incomplete`, then `shutdown_finishing` with `logger_pending=true` | either final cleanup is incomplete, including final-record loss |

When a stop signal ends startup before engines/consumers start, the plan still
cleans all acquired resources. Stage ceilings total 2 + 2 + 3 + 5 + 5 = 17 s;
SDK join slack and the runtime reserve make the whole tail 18.5 s. Validation
accepts `grace_period >= drain_timeout + 18.5s`; defaults require 43.5 s inside
45 s. The worker has no readiness propagation delay: it stops claims and pulls
at once. Its cooperative background join ceiling remains 3 s. Abort
acknowledgement shares the following dependency stage's fixed deadline, exactly
as in the service; tracker closure alone is not completion evidence.

**Background tasks.** `Registration::spawn(name, |cancel| task)` and
`Registration::shutdown` retain their existing contract. Each process-owned task
runs until cancellation. Unexpected return or panic is observed during startup
and serving and blocks a ready transition; panic after cancellation remains
failed cleanup. The worker retains its tracker as completion authority and
abort handles for registered tasks. Its metrics upkeep, runtime metrics, pool
metrics, password refresh, readiness refresher and registration-spawned tasks
record `background_task_stopped` with `task` and `panicked`, without panic
payloads. An engine's claim loop, retention, listener and sampler report
`jobs_engine_task_stopped`. Engine and consumer failure channels remain
authoritative for their own work; no second supervisor owns their internal tasks.

**Ordinary exit codes.** `exit_code` is the one mapping. `process::exit` is never
called.

| Code | When |
| --- | --- |
| `0` | A stop signal, and every voting stage including final trace/logger cleanup completed normally; the drain ended with `drained()` |
| `3` | A stop signal with any degraded stage, including forced drain, background panic/failed join, unconfirmed cleanup or final trace/logger failure or final-record loss |
| `1` | Startup refusal, caught bootstrap unwind, or unexpected live engine/consumer/task/listener failure; the same staged cleanup preserves the primary cause and its original deadline |

**Operator mode.** `cli.rs` selects optional inspect/failed/unhandled/redrive/
discard commands before ordinary configuration and startup; `operator.rs`
loads only `JobsOperatorConfig`, installs signal streams and admits a one-slot
PostgreSQL pool with fixed `application_name=jobs-worker-operator`. It starts
no registry, engine, broker, listener, exporter, password refresher or maintenance.
The same history verifier and shared jobs UTF-8/writable/READ COMMITTED session
check admit every mode against the canonical writable queue before one operation.
Inspection then uses a read-only transaction; mutation uses the caller-owned
transaction. Both set a two-second local statement timeout, before the mutation's
initial row lock, without changing ordinary pooled session budgets.

After argument/configuration/file admission, connect/history/session/operation/
pool-close/runtime ceilings are 5/5/5/12/5/1 seconds. Signals cancel the current
future then close admitted resources. Mutation after invocation is conservatively
unknown on interruption; inspection is unavailable. Only acknowledged commit
produces `redriven` or `discarded`. Central mapping returns 0 for success, 1 for
failure/unknown or incomplete cleanup, and 2 for usage. Cleanup or stdout failure
cannot undo a committed mutation; an absent receipt requires inspection.
Messaging-only projection keeps the loader parser and clap while removing
operator modes. See the [safe command contract](../background-jobs.md#inspect-and-recover-retained-jobs).

The [guide](../background-jobs.md#run-and-stop-the-worker) covers running and
stopping the worker. [Async Architecture](async.md) records the mechanism.
<!-- template:end jobs:docs-lifecycle-jobs-worker -->

<!-- template:begin messaging:docs-lifecycle-messaging -->
## JetStream lifecycle

Messaging startup installs process signals before broker I/O, admits the NATS
connection within the existing startup budget, requires JetStream and server
version >=2.12.3, then checks operator-created source/DLQ streams and declares
the named consumer. Only `jobs-worker` connects; the API publishes through the
outbox. The worker admits a consumer only after a handler registry exists.
Readiness refreshes local connection state in the existing `health` owner, so
HTTP and metrics read a cached verdict and connection loss cannot leave stale
health indefinitely.

At the first stop signal, readiness drains and no new NATS pull starts.
Handlers, DLQ transfer, and source settlement share the worker's remaining
drain deadline. At expiry, unfinished delivery tasks are aborted, leaving their
source records for redelivery. Dependency close submits
NATS drain and waits for its native Closed notification within the existing
close budget; an absent notification, unjoined application work, or forced
drain yields the established degraded exit code rather than clean shutdown.
<!-- template:end messaging:docs-lifecycle-messaging -->
<!-- template:begin cache:docs-lifecycle-cache -->
## Cache lifecycle

`Cache::connect_lazy` admits configuration and starts one owned supervisor over
canonical multiplexed connections, without waiting for network I/O. Setup and
recovery advance without traffic. Each setup attempt is bounded at 1 s, with
capped backoff and repeated retry chains. Generation identity fences retirement
so late failures cannot remove a successor. A periodic PING every 2 s has a
`min(command_timeout, 1 s)` response budget. Password refresh every 5 s shares
a 1 s read/direct-AUTH budget; rejected unchanged credentials remain retryable.

Startup retains the lazy cache owner before running one probe check inside its
existing 1 s bound. Success logs `cache_connected`; failure logs `cache_unavailable_at_startup` and startup
continues. The cache is not a readiness probe unless composition pushes
`cache.probe()` into the probe list. Probe acquisition uses its caller's
budget; a connected PING has a 1 s ceiling and ends on generation retirement.
It is never a liveness check.

Shutdown drops `Option<Cache>` inside `Dependencies::close`, in the dependency
stage after HTTP drain. Cache, namespace and probe handles retain one shared
application owner. Its final drop withdraws the connection and cancels/aborts
the supervisor, which holds no owner cycle. Runtime scheduling completes
destruction and last-clone drop aborts the canonical connection driver. A
retained namespace or probe legitimately keeps the cache alive. The synchronous
drop adds no wait to `DEPENDENCY_CLOSE`; the same path covers failed and
interrupted startup. See the [guide](../cache.md) for recovery bounds and
readiness opt-in.
<!-- template:end cache:docs-lifecycle-cache -->
<!-- template:begin object-storage:docs-lifecycle-object-storage -->
## Object storage lifecycle

`ObjectStorage::new` admits the provider tuple and builds the SDK client. It
does no network I/O, and startup runs no bucket check: a misconfiguration
fails startup before the listener, while a provider outage is left to the
calls that need the bucket. Storage is not a readiness probe unless
composition pushes `storage.probe()`; it is never a liveness check.

Shutdown drops `Option<ObjectStorage>` inside `Dependencies::close`, after the
HTTP drain and background completion handling. Forced or unconfirmed work
remains recorded as such; releasing this handle does not certify completion.
Idle connections close with the last clone. The same drop runs on the
startup-failure and stopped-startup paths.
<!-- template:end object-storage:docs-lifecycle-object-storage -->
<!-- template:begin outbox:docs-lifecycle-outbox -->
With the outbox profile retained, worker startup admits the mode-aware pool
capacity before starting either engine: `N + 5` with ordinary jobs and outbox,
or three for outbox-only. A stop signal halts both claim loops immediately;
the publication engine drains, cancels, and closes within the same absolute
process deadlines as ordinary jobs and NATS work. It gets no additional grace
period, and a forced drain leaves unfenced work for the existing jobs recovery
path.
<!-- template:end outbox:docs-lifecycle-outbox -->

## Decisions Recorded Here

<!-- template:begin grpc:docs-runtime-grpc -->
The optional gRPC listener uses the same bootstrap and `infra_http::Server`.
It builds the tonic router and any TLS config before serving. Health reads
cached readiness and is `NOT_SERVING` until admission succeeds. At first stop,
readiness drain publishes `NOT_SERVING` before the propagation delay. HTTP and
gRPC then drain concurrently under the remaining effective drain budget.
Health watchers end after `NOT_SERVING` and do not hold that drain.
Configuration refuses a `grpc.request_timeout` longer than that budget. It does
not add a second budget or change NATS/provider shutdown ownership. See
[gRPC](../grpc.md#health-shutdown-and-observation).
<!-- template:end grpc:docs-runtime-grpc -->

- **Tokio multi-thread runtime owned by `bootstrap::run`**, sized by
  typed `runtime.worker_threads`, or the standard library's
  `available_parallelism` estimate when unset (falling back to one).
  That estimate is not a guarantee of the container's CPU allocation; the
  [runtime configuration policy](../configuration-source-policy.md#runtime)
  owns the override. No `GOMAXPROCS` or
  `memory_limit_ratio` equivalent exists because there is no garbage
  collector.
- **`CancellationToken` (`tokio-util`) and a Tokio `JoinSet`** for the
  service's background work: `child_token()` is one-directional, and the set
  reports a task that ends early, panics included, which a `TaskTracker`
  does not. The worker keeps a tracker, which the engines spawn on, and
  gives each task a guard that reports the same early end by name.
- **Signal streams are created before anything can send a signal and kept
  alive**; a dropped `tokio::signal::unix::signal` stream swallows later
  signals.
- **Distinct exit code for a degraded shutdown** (`3`), a deviation from the
  Go template's single error code, so an expired drain budget is not read as
  a crash.
- **Readiness over `tokio::sync::watch`** rather than a per-request probe or
  a mutex: a streaming reader such as the gRPC health `Watch` wakes on each
  publication, and the handler is an O(1) read.
- **Build metadata**: `app.version` is `CARGO_PKG_VERSION` of the calling
  binary; `app.commit` is `vergen-gitcl` in `crates/config/build.rs` with
  `default_on_error()`, overridable through `VERGEN_GIT_SHA`, which the image
  build sets from `VCS_REF` (or Railway's `RAILWAY_GIT_COMMIT_SHA`).
- **Process tests** use the Cargo executable environment key for the main
  binary declared in `crates/service/Cargo.toml`, an ephemeral port
  (`APP__HTTP__ADDR=127.0.0.1:0`) read back from the JSON startup log, a
  readiness poll, `nix` `SIGTERM` (`Child::kill` is `SIGKILL`), and assert
  the exit code and drain timing. The trace provider runs under `spawn_blocking` and an absolute deadline;
  local writer completion uses a runtime-independent bounded wait. Neither
  successful call establishes Collector/backend delivery.
