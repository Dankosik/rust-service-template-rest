# Runtime Lifecycle

Load for startup, readiness, drain, shutdown, exit codes, or
process-resource ownership. `crates/service/src/bootstrap` (`mod.rs` and
`shutdown.rs`) is the code; the process tests in `crates/service/tests` prove
it against the built binary.

## Startup

1. `main` hands `argv` to `bootstrap::run`, which parses the loader flags,
   loads the configuration snapshot, and validates the grace budget before a
   runtime exists. A failure here prints one readable message to stderr and
   exits `1`.
2. Inside the Tokio runtime, the signal streams are installed first, so a
   `SIGTERM` that arrives during startup is handled rather than killing the
   process.
3. The tracer provider is installed, then the subscriber (so SDK warnings are
   caught), then the Prometheus recorder; the startup record
   (`service_starting`) carries the non-secret facts an operator needs:
   `app.env`, `app.version`, `app.commit`, the listeners, the budgets, the
   log level, and the exporter state (`initialized`, `disabled`, `degraded`).
4. Background tasks (metrics upkeep, Tokio runtime metrics, the readiness
   refresher) join a `TaskTracker` with child `CancellationToken`s.
5. Admit the dependencies retained by the local profile before accepting
   traffic; bootstrap owns their readiness registration and cleanup.
6. The API contract comes from `service::api::contract()`: the route tree
   and its OpenAPI document as one value. Assembly is pure, so it runs
   before readiness admission.
7. Readiness admission: the refresher evaluates every registered probe once
   under `health.probe_budget`; a failure is a startup failure (exit
   `1`). Without a selected profile the set is empty and admission proves
   the mechanism. The route tree is then given the readiness reader as
   state, wrapped by `infra_http::harden`, and bound by the bounded
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
future joins the existing `TaskTracker` with a child cancellation token.
Authentication neither adds a readiness probe nor changes public health behavior.
<!-- template:end authn:docs-lifecycle-authn -->
<!-- template:begin outbound-http:docs-lifecycle-outbound -->
A retained [outbound client](../outbound-http.md) is inert until a concrete
provider is wired. Operations are caller-owned futures and future drop releases
their admission permit. Resolver, connection-pool, and HTTP library tasks are
library-owned; bootstrap neither gives them a tracker/token nor joins them in a
shutdown stage. The client adds no readiness probe or teardown stage. JWT refresh
remains the separate process-owned task that the existing tracker cancels and
joins.
<!-- template:end outbound-http:docs-lifecycle-outbound -->
<!-- template:begin outbound-auth:docs-lifecycle-outbound-auth -->
The retained OAuth2 profile is inert until a concrete integration constructs an
authenticated client. Token acquisition is request-owned work: it consumes the
caller deadline, has no detached refresh or maintenance task, and releases its
private cache when its last owner is dropped. It adds no readiness probe,
bootstrap provider call, or teardown stage.
<!-- template:end outbound-auth:docs-lifecycle-outbound-auth -->


<!-- template:begin postgres:docs-lifecycle-postgres-startup -->
With the PostgreSQL profile retained and `postgres.enabled`, bootstrap admits
the DSN and opens the first connection inside the acquire budget
(`postgres_pool_opened`). It then verifies, in one read-only check bounded to
five seconds including acquire, that every embedded migration is applied with
its checksum: pending or divergent history is a sanitized startup failure,
while versions from a later release are admitted so a rollback still starts.
The probe joins readiness, the pool gauge task joins the tracker, and an
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
active, a background cleanup task joins the existing `TaskTracker` with a
child cancellation token and is cancelled and joined in the existing
background-join shutdown stage, beside the other background tasks. It never
adds a readiness probe or a shutdown stage of its own.
<!-- template:end http-idempotency:docs-lifecycle-http-idempotency -->

Configuration and dependency admission precede traffic acceptance.
Bootstrap, not handlers or feature code, owns process lifecycle and the
cleanup of a partial startup: a pool opened before a later stage failed is
closed explicitly under the dependency-close budget before the process
exits `1`.

## Readiness and liveness

`/health/live` is process-only and always `200 ok` while the process runs.
`/health/ready` reads the cached verdict published by the `health` crate's
refresher: healthy after every probe passed, unhealthy after
`health.failure_threshold` consecutive failures, not ready when the snapshot
is older than the staleness guard allows (a dead refresher fails closed),
and not ready as soon as teardown starts. The handler never runs a probe, so
its latency is independent of dependency latency.

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
| Cancel and join background tasks | `5s` | `background_joined` |
| Close selected dependencies | `5s` | An overrun votes `degraded`; unused capacity retains the same grace-budget arithmetic |
| Flush telemetry | `5s` | `telemetry_flushed`, then `shutdown_completed` |

<!-- template:begin postgres:docs-lifecycle-postgres-close -->
The retained PostgreSQL pool closes in the dependency-close stage and records
`postgres_pool_closed`.
<!-- template:end postgres:docs-lifecycle-postgres-close -->

<!-- template:begin oidc-jwt:docs-lifecycle-jwt-refresh -->
JWT refresh is periodic and may be triggered by an unknown key; one shared
fetch is coalesced and canceled/joined with background work during shutdown.
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
| `1` | Startup failure: invalid configuration, unknown key, malformed `APP__` name, secret in a file, bind failure, admission failure |

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

**Startup.** Each refusal below exits `1`.

| Step | What | Refusal (exit 1) |
| --- | --- | --- |
| 1 | `FromArgs::from_argv` (`--help` exits `0`) | clap error |
| 2 | Registration function is `None` | `no job kind or typed message handler is registered: register this service's retained capabilities in crates/jobs-worker/src/main.rs` |
| 3 | `service_config::load` (same sources, precedence, unknown-key and secret rules as the service) | `configuration is invalid: ...` |
| 4 | `shutdown::validate_grace_budget(&config.http)` | `http.grace_period (..) must be >= http.drain_timeout (..) plus the 17s jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush)` |
| 5 | Build the multi-thread runtime | `build tokio runtime: ...` |
| 6 | Install `Signals` (SIGINT, then SIGTERM) | `install stop signal handlers: ...` |
| 7 | Tracer provider with the worker identity, subscriber, recorder, and the retained messaging panic hook | the telemetry errors, as in the service |
| 8 | Register optional jobs and typed-message capabilities through `register(&mut kinds, &mut messages, &support)`; validate each nonempty registry | `job kind registration failed: ...`; `job kinds are invalid: ...`; `typed message handlers are invalid: ...`; no retained capability refuses with the step-2 message |
| 9 | `jobs_worker_starting` record; metrics upkeep and Tokio runtime metrics join the tracker | |
| 10 | After registration, determine whether retained capabilities need PostgreSQL; validate `postgres.enabled` and mode-aware pool capacity, then admit the DSN/pool and migration history | `postgres.enabled must be true to run the jobs worker`; capacity, DSN, pool, or history refusal |
| 11 | When messaging or outbox is retained, validate producer/consumer configuration, connect NATS under its startup budget, and admit a consumer only for registered typed handlers | messaging configuration, connection, topology, bounds, or consumer refusal |
| 12 | Construct every required ordinary and reserved publication `Engine`, then run each `Engine::check_startup` | `jobs startup check: ...` |
| 13 | Bind the health listener (`http.addr`), then the diagnostics listener (`observability.metrics.addr`, when set); `http listener bound`, `diagnostics listener bound` | `bind http listener ...` |
| 14 | Readiness admission (`refresh`, then cached verdict over retained PostgreSQL and messaging probes), raced against stop signals | `startup admission: ...` |
| 15 | Only after admission, start every `Engine` and the admitted consumer; `jobs_claiming_started` and `messaging_consuming_started` | |
| 16 | Refresher task; `jobs_worker_ready` | |
| 17 | Wait for a stop signal, an engine failure, or a consumer failure | |

Steps 1-8 open no dependency. Step 2 precedes configuration so the shipped
binary refuses the same way everywhere; registration itself follows
configuration and constructs only local registries. Signal streams exist from
step 6, so a stop during admission remains observable. A signal while NATS
connects or admits a consumer, before readiness admission, or immediately
before step 15 starts no engine and no consumer; the staged plan still closes
any resource already opened. Before admission, `/health/ready` answers `503
not ready` (not evaluated). Every refusal after the runtime started goes
through one `abort_startup` teardown: it finishes all started engines and
consumer work within 2 s, closes bound listeners within 2 s, joins background
tasks within 3 s, and closes retained pool and messaging resources within 5 s.
It flushes no telemetry.

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

When a stop signal ends startup before step 15, no engine or consumer starts;
the plan still closes resources already admitted. The tail after the drain is
2 + 2 + 3 + 5 + 5 = 17 s. `validate_grace_budget` refuses a grace period
below `http.drain_timeout` plus 17 s. The default worst case is
25 s + 17 s = 42 s inside 45 s, leaving the same 3 s the service leaves for
`Runtime::shutdown_timeout(1s)` and the tracer join slack. The worker has no
readiness propagation delay: it stops claims and pulls at once. The background
join is 3 s (the service's is 5 s) because attempts are drained and their outcomes
are finished before that stage and every joined task stops at its next await.

**Exit codes.** `exit_code` is the one mapping. `process::exit` is never
called.

| Code | When |
| --- | --- |
| `0` | A stop signal, and every stage completed inside its ceiling; the drain ended with `drained()`, so no attempt was cancelled at its end |
| `3` | A stop signal, and any stage voted degraded, including a forced drain (budget or second signal), which is the only way an attempt is cancelled at the drain's end |
| `1` | A startup refusal, or a started engine or consumer fails without a stop signal. After the failure the same staged plan runs, with its deadline starting at the failure, and its outcome does not change the code |

The [guide](../background-jobs.md#run-and-stop-the-worker) covers running and
stopping the worker. [Async Architecture](async.md) records the mechanism.
<!-- template:end jobs:docs-lifecycle-jobs-worker -->

<!-- template:begin messaging:docs-lifecycle-messaging -->
## JetStream lifecycle

Messaging startup installs process signals before broker I/O, admits the NATS
connection within the existing startup budget, requires JetStream and server
version >=2.12.3, then checks operator-created source/DLQ topology and the
named consumer. The API admits only a producer; the worker admits a consumer
only after a handler registry exists. Readiness refreshes bounded client and
topology state in the existing `health` owner, so HTTP and metrics read a
cached verdict and connection loss cannot leave stale health indefinitely.

At the first stop signal, readiness drains and no new NATS pull starts.
Handlers, DLQ transfer, and source settlement share the worker's remaining
drain deadline. At expiry, unfinished handler tasks are cancelled, aborted and
joined, leaving their source records for redelivery. Dependency close submits
NATS drain and waits for its native Closed notification within the existing
close budget; an absent notification, unjoined application work, or forced
drain yields the established degraded exit code rather than clean shutdown.
<!-- template:end messaging:docs-lifecycle-messaging -->
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
The optional gRPC listener uses the same bootstrap. It prepares descriptors,
validation, verifier and TLS before serving, remains NOT_SERVING until startup
admission, and reads cached readiness including its exact stale boundary. At
first stop it rejects new business calls and publishes terminal health before
propagation. HTTP and gRPC then drain concurrently under the remaining effective
deadline; health watchers do not own business completion. Forced drain drops and
joins connections, H2 streams and transport waiters before the existing teardown
tail. It does not change NATS/provider shutdown ownership or add a second budget.
See [gRPC](../grpc.md#health-shutdown-and-observation).
<!-- template:end grpc:docs-runtime-grpc -->

- **Tokio multi-thread runtime owned by `bootstrap::run`**, sized by
  `available_parallelism`, which honours cgroup quotas; no `GOMAXPROCS` or
  `memory_limit_ratio` equivalent exists because there is no garbage
  collector.
- **`CancellationToken` and `TaskTracker`** (`tokio-util`) for background
  work: `child_token()` is one-directional, and `TaskTracker::wait()` needs
  `close()` first.
- **Signal streams are created before anything can send a signal and kept
  alive**; a dropped `tokio::signal::unix::signal` stream swallows later
  signals.
- **Distinct exit code for a degraded shutdown** (`3`), a deviation from the
  Go template's single error code, so an expired drain budget is not read as
  a crash.
- **Readiness over `tokio::sync::watch`** rather than a per-request probe or
  a mutex: tests await `changed()` instead of sleeping, and the handler is an
  O(1) read.
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
