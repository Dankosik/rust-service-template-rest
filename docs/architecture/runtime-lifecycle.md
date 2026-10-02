# Runtime Lifecycle

Load for startup, readiness, drain, shutdown, exit codes, or
process-resource ownership. `crates/service/src/bootstrap` is the code:
`mod.rs` is the startup order, each retained profile keeps its step in a
module beside it, and `shutdown.rs` is the teardown; the process tests in `crates/service/tests` prove
it against the built binary.

## Startup

1. `main` hands `argv` to `bootstrap::run`, which parses the loader flags
   (`LoadOptions::parse_from`; `--help` exits `0`, a flag error exits `2`),
   loads the configuration snapshot, and validates the grace budget before a
   runtime exists. A later failure here prints one readable message to stderr
   and exits `1`.
2. Inside the Tokio runtime, the signal streams are installed first, so a
   `SIGTERM` that arrives during startup is handled rather than killing the
   process. Startup then runs raced against those streams: a stop signal
   drops the unfinished startup at its next await, no listener is bound, and
   the teardown below runs without the listener stages.
3. The tracer provider is installed, then the subscriber (so SDK warnings are
   caught), then the panic hook that turns a panic into an ERROR record,
   then the Prometheus recorder; the startup record
   (`service_starting`) carries the non-secret facts an operator needs:
   `app.env`, `app.version`, `app.commit`, the listeners, the budgets, the
   log level, and the exporter state (`initialized`, `disabled`, `degraded`).
4. Background tasks (metrics upkeep, Tokio runtime metrics, the readiness
   refresher) join one `JoinSet` with child `CancellationToken`s.
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
   `observability.metrics.addr` is set. `service_ready` is logged only after
   both binds; the platform's first `/health/ready` poll answers from the
   admission evaluation.

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
The retained OAuth2 profile is inert until a concrete integration constructs an
authenticated client. Token acquisition is request-owned work: it consumes the
caller deadline and releases its private cache when its last owner is dropped.
The only detached work is one early refresh attempt of at most five seconds,
which the runtime cancels rather than joins. It adds no readiness probe,
bootstrap provider call, or teardown stage.
<!-- template:end outbound-auth:docs-lifecycle-outbound-auth -->


<!-- template:begin postgres:docs-lifecycle-postgres-startup -->
With the PostgreSQL profile retained and `postgres.enabled`, bootstrap admits
the DSN and opens the first connection inside the acquire budget
(`postgres_pool_opened`). It then verifies, in one read-only check bounded to
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
Bootstrap, not handlers or feature code, owns process lifecycle and the
cleanup of a partial startup. Startup records every opened dependency in one
`Dependencies` value, and a failed or stopped startup runs the same staged
teardown as a stop signal without the listener stages: background tasks
join, opened dependencies close under the dependency-close budget, and
telemetry flushes. A failed startup then exits `1`; a startup stopped by a
signal exits by the teardown outcome, `0` when every stage fit its budget.

Every background task runs until its token is cancelled, and nothing cancels
before teardown. A task that ends while the service is serving has therefore
panicked or hit a defect. Bootstrap observes it beside the stop signal, logs
`service failed`, runs the full teardown, and exits `1`, so the platform
replaces the instance instead of keeping one that serves without, for
example, its JWKS refresh. The watch covers the serving phase only: a task
that ends during startup is reported when startup completes, and one that
ends during teardown is joined like any other.

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
while the instance is not ready yet, and after a ready verdict only once
`health.failure_threshold` checks in a row have failed. A verdict older than
the staleness bound is refused (a dead or hung refresher fails closed), and
the instance is not ready as soon as teardown starts. The handler never runs
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

The refresher reports itself through three metrics and four log events.
`readiness_checks_total{outcome}` (`ok`, `failed`, `timed_out`) has one
increment per completed check; a rate of zero on a running process is a
stopped refresher. `readiness_probe_checks_total{probe,outcome}` counts each
probe's own outcome in every check, so it shows which dependency fails,
including a second one behind the probe the verdict names and one whose
failures the threshold still absorbs. The `readiness_ready` gauge is the
published answer: `1` while ready, `0` before the first check, while a probe
verdict is withdrawn, and from the start of the drain. The refresher and the
drain write it, so a stopped refresher leaves its last value standing; the
check rate is the signal for that. The events are `readiness_lost` and
`readiness_recovered` for a published flip, `readiness_check_failed` for a
failure the threshold absorbed, and `readiness_refresh_late` when a check
completes after the previous verdict already went stale.

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

Every stage draws from one deadline started at the first stop signal
(`http.grace_period`, default `45s`); a slow stage shortens the ones after it
instead of pushing the process into `SIGKILL`.

| Stage | Budget | Observable record |
| --- | --- | --- |
| Readiness off (drain flag) | immediate | `readiness_disabled` |
| Propagation delay: keep serving while load balancers notice | `http.readiness_propagation_delay` (`15s`); a second signal skips it | `readiness_propagation_wait` |
| HTTP drain: stop accepting, finish in-flight requests | `http.drain_timeout` minus the delay (`10s`) | `drain_started`, then `drain_completed`, or `shutdown_forced` with `remaining` connections |
| Diagnostics listener close | `2s` | `diagnostics_stopped` or `diagnostics_forced` |
| Cancel and join background tasks; tasks that outlive the budget are aborted | `5s` | `background_joined` |
| Close selected dependencies | `5s` | An overrun votes `degraded`; unused capacity retains the same grace-budget arithmetic |
| Flush telemetry | `5s` | `telemetry_flushed`, then `shutdown_completed` |

<!-- template:begin postgres:docs-lifecycle-postgres-close -->
The retained PostgreSQL pool closes in the dependency-close stage and records
`postgres_pool_closed`.
<!-- template:end postgres:docs-lifecycle-postgres-close -->

<!-- template:begin oidc-jwt:docs-lifecycle-jwt-refresh -->
JWT refresh is periodic and may be triggered by an unknown key or a kid-less
signature miss; one shared fetch is coalesced and canceled/joined with background work during shutdown.
Failed refresh keeps the last usable keys. There is no maximum cached-key age
and this is not an immediate-revocation mechanism.
<!-- template:end oidc-jwt:docs-lifecycle-jwt-refresh -->

The `17s` tail after the drain is process structure, not configuration;
`validate_grace_budget` refuses a configuration whose grace period cannot
hold `drain_timeout` plus the tail. Tracer-provider shutdown may still
spend a short join slack after the telemetry flush budget; that slack is
not part of the encoded tail. The default worst case is `42s`
inside `45s`; the platform grace derivation lives in
[Configuration Source Policy](../configuration-source-policy.md#runtime-budget-policy)
and the image check proves it with `docker stop --time 45`.

After the stages, `Runtime::shutdown_timeout(1s)` force-drops connection
tasks that outlived the drain.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Every stage completed inside its budget |
| `3` | The process shut down on its own but a stage overran (degraded shutdown); the platform and the process test can tell it from a crash |
| `1` | Startup failure: invalid configuration, unknown key, malformed `APP__` name, secret in a file, bind failure, admission failure. Also a background task that ended while serving; the teardown still runs first |

`--help` exits `0`. `--version` is not a loader flag: identity is
`BuildInfo` / `app.version`. `process::exit` is never called, so
destructors run.
<!-- template:begin jobs:docs-lifecycle-jobs-worker -->

## Jobs worker

The code is `crates/jobs-worker/src/lib.rs` (`run`, the synchronous startup
phases, and `exit_code`, the one exit-code mapping) and
`crates/jobs-worker/src/{bootstrap,shutdown}.rs` (the asynchronous startup
with its refusals; signals, the stage budget, the shutdown plan, and
`abort_startup`). The process proof is `crates/jobs-worker/tests/process.rs`
for the shipped binary, and the test-only `jobs-worker-fixture` suite in
`test/tests/jobs/`.

**Ordinary startup.** With no subcommand, each refusal below exits `1`,
except step 1, which exits `2`.

| Step | What | Refusal (exit 1) |
| --- | --- | --- |
| 1 | `WorkerArgs` flattens `LoadOptions` (`--help` exits `0`) | clap usage error (exit 2) |
| 2 | `service_config::load` (same sources, precedence, unknown-key and secret rules as the service) | `configuration is invalid: ...` |
| 3 | `shutdown::validate_grace_budget(&config.http)` | `http.grace_period (..) must be >= http.drain_timeout (..) plus the 17s jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush)` |
| 4 | Build the multi-thread runtime | `build tokio runtime: ...` |
| 5 | Install `Signals` (SIGINT, then SIGTERM) | `install stop signal handlers: ...` |
| 6 | Tracer provider with the worker identity, subscriber, recorder, and the panic hook (it records the panic's file, line, column, and thread, never its message) | the telemetry errors, as in the service |
| 7 | Register optional jobs and typed-message capabilities through `register(&mut registration)`, which fills `Registration::jobs` and `Registration::messages`; validate each nonempty registry. A composition with no retained capability refuses after configuration is loaded | `job kind registration failed: ...`; `job kinds are invalid: ...`; `typed message handlers are invalid: ...`; `no job kind or typed message handler is registered: register this service's retained capabilities in crates/jobs-worker/src/main.rs` |
| 8 | `jobs_worker_starting` record; metrics upkeep and Tokio runtime metrics join the tracker | |
| 9 | After registration, determine whether retained capabilities need PostgreSQL; validate `postgres.enabled` and mode-aware pool capacity, then admit the DSN/pool and migration history | `postgres.enabled must be true to run the jobs worker`; capacity, DSN, pool, or history refusal |
| 10 | When messaging or outbox is retained, validate producer/consumer configuration, connect NATS under its startup budget, and admit a consumer only for registered typed handlers | messaging configuration, connection, topology, bounds, or consumer refusal |
| 11 | Construct every required ordinary and reserved publication `Engine`, then run each `Engine::check_startup` | `jobs startup check: ...` |
| 12 | Bind the health listener (`http.addr`), then the diagnostics listener (`observability.metrics.addr`, when set), which serves `/metrics` and `GET /health/live`; `http listener bound`, `diagnostics listener bound` | `bind http listener ...` |
| 13 | Readiness admission (`refresh`, then cached verdict over retained PostgreSQL and messaging probes), raced against stop signals | `startup admission: ...` |
| 14 | Only after admission, start every `Engine` and the admitted consumer; `jobs_claiming_started` and `messaging_consuming_started` | |
| 15 | Refresher task; `jobs_worker_ready` | |
| 16 | Wait for a stop signal, an engine failure, a consumer failure, or a background task that ended | |

Steps 1-7 open no dependency. Registration follows configuration and
constructs only local registries. Signal streams exist from step 5, so a
stop during admission remains observable. A signal while NATS connects or
admits a consumer, before readiness admission, or immediately before step
14 starts no engine and no consumer; the staged plan still closes any
resource already opened. Before admission, `/health/ready` answers `503
not ready` (not evaluated). A failed signal install returns before anything
is open. Every later refusal goes through one `abort_startup` teardown: it
finishes all started engines and consumer work within 2 s, closes bound
listeners within 2 s, joins background tasks within 3 s, and closes
retained pool and messaging resources within 5 s. It flushes no telemetry.

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

**Shutdown.** One deadline, `http.grace_period`, starts at the first signal.
Each stage takes the lesser of its ceiling and the remaining time. Any stage
that votes degraded makes the exit code `3`.

| Stage | Ceiling | Records | Votes degraded (exit 3) when |
| --- | --- | --- | --- |
| Readiness off; stop every jobs claim loop and messaging pull | immediate | `shutdown_started`, `readiness_disabled`, `claiming_stopped` (in-flight count), `messaging_pulls_stopped` | never; admitted work may settle under its existing backstop |
| Drain all started engines and the consumer; a second signal ends it | `http.drain_timeout` (25 s); no propagation delay | `drain_started`, then `drain_completed` or `drain_forced` (in-flight attempts, reason `budget`, `second_signal`, or `messaging`) | any engine or consumer drain does not finish inside the shared budget |
| Only after a forced drain: finish every engine attempt and abort/finish the consumer | 2 s | `attempts_finished` (known results, cancelled handlers, acknowledged releases, uncertainty) | cleanup overrun |
| Close the health listener and the diagnostics listener concurrently | 2 s | `listeners_stopped`; `diagnostics_forced` for a scrape overrun | the health listener overruns (a diagnostics overrun is forced closed without a vote, as in the service) |
| Cancel and join background tasks (claim loops, retention, sampler, metrics, refresher) | 3 s | `background_joined` | the join overruns |
| Close retained pool and messaging dependency | 5 s | `postgres_pool_closed` and messaging close outcome | either close overruns or messaging close is unobserved |
| Flush telemetry | 5 s | `telemetry_flushed`, `shutdown_completed` | the flush is incomplete |

When a stop signal ends startup before step 14, no engine or consumer starts;
the plan still closes resources already admitted. The tail after the drain is
2 + 2 + 3 + 5 + 5 = 17 s. `validate_grace_budget` refuses a grace period
below `http.drain_timeout` plus 17 s. The default worst case is
25 s + 17 s = 42 s inside 45 s, leaving the same 3 s the service leaves for
`Runtime::shutdown_timeout(1s)` and the tracer join slack. The worker has no
readiness propagation delay: it stops claims and pulls at once. The background
join is 3 s (the service's is 5 s) because attempts are drained and their outcomes
are finished before that stage and every joined task stops at its next await.

**Background tasks.** Every task the worker spawns runs until its token is
cancelled, and nothing cancels before teardown, so a task that ends earlier
is a panic or a defect. The worker then stops rather than run without it, as
the service does. Its own tasks (metrics upkeep, runtime metrics, pool
metrics, password refresh when `postgres.password_file` is set, the readiness
refresher, and tasks a registration spawned) record `background_task_stopped`
with `task` and `panicked`; an engine's claim loop, retention, listener, and
sampling record `jobs_engine_task_stopped` and fail that engine. A task that
ends during startup is reported when startup completes.

**Ordinary exit codes.** `exit_code` is the one mapping. `process::exit` is never
called.

| Code | When |
| --- | --- |
| `0` | A stop signal, and every stage completed inside its ceiling; the drain ended with `drained()`, so no attempt was cancelled at its end |
| `3` | A stop signal, and any stage voted degraded, including a forced drain (budget or second signal), which is the only way an attempt is cancelled at the drain's end |
| `1` | A startup refusal, or a started engine, consumer, or background task ends without a stop signal; the reported failure names which one stopped, and a consumer failure carries its cause. After the failure the same staged plan runs, with its deadline starting at the failure, and its outcome does not change the code |

**Operator mode.** `cli.rs` selects optional inspect/failed/unhandled/redrive/
discard commands before ordinary configuration and startup; `operator.rs`
loads only `JobsOperatorConfig`, installs signal streams and admits a one-slot
PostgreSQL pool with fixed `application_name=jobs-worker-operator`. It starts
no registry, engine, broker, listener, exporter, password refresher or maintenance.
The same history verifier and shared jobs session check run before one operation.
Inspection uses read-only transactions and a two-second statement limit;
mutation uses the caller-owned transaction and existing session budgets.

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

`Cache::connect_lazy` admits configuration and builds a lazy `ConnectionManager`.
It waits for no network I/O; the connection is dialed in the background from
then on. Startup then runs one probe check inside a 1 s bound.
Success logs `cache_connected`. Failure logs `cache_unavailable_at_startup`
and startup continues. The cache is not a readiness probe unless composition
pushes `cache.probe()` into the probe list. It is never a liveness check. A
gate would turn an outage into total unavailability.

Shutdown drops `Option<Cache>` inside `Dependencies::close`, in the dependency
stage after HTTP drain. The connection closes when its last clone drops, and
the drop does not add to `DEPENDENCY_CLOSE`. The same drop runs on the
startup-failure and stopped-startup paths. The [guide](../cache.md) shows the
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
HTTP drain and the background join, so an in-flight call has finished or been
dropped. Idle connections close with the last clone. The same drop runs on the
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
  `available_parallelism`, which honours cgroup quotas; no `GOMAXPROCS` or
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
  the exit code and drain timing. `SdkMeterProvider::shutdown_with_timeout`
  ignores its argument, so telemetry shutdown is bounded with
  `spawn_blocking` plus `timeout`.
