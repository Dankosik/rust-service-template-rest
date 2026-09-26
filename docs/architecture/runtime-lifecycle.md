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
| 2 | `register` is `None` | `no job kind is registered: register this service's job kinds in crates/jobs-worker/src/main.rs` |
| 3 | `service_config::load` (same sources, precedence, unknown-key and secret rules as the service) | `configuration is invalid: ...` |
| 4 | `postgres.enabled` | `postgres.enabled must be true to run the jobs worker` |
| 5 | `config.jobs.required_connections(&config.postgres)` | `configuration is invalid: postgres.max_connections: must be at least jobs.max_workers + 2 (N) for the jobs worker` |
| 6 | `shutdown::validate_grace_budget(&config.http)` | `http.grace_period (..) must be >= http.drain_timeout (..) plus the 17s jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush)` |
| 7 | Build the multi-thread runtime | `build tokio runtime: ...` |
| 8 | Install `Signals` (SIGINT, then SIGTERM) | `install stop signal handlers: ...` |
| 9 | Tracer provider with the worker identity, subscriber, recorder with the jobs buckets | the telemetry errors, as in the service |
| 10 | `register(&mut kinds, &support)`, then `kinds.validate()` | `job kind registration failed: ...`; `job kinds are invalid: ...` (a set with no kind says `no job kind is registered`) |
| 11 | `jobs_worker_starting` record | |
| 12 | Metrics upkeep and Tokio runtime metrics tasks join the tracker | |
| 13 | `Dsn::admit`, `infra_postgres::connect` with the derived `application_name` and `READ COMMITTED` default; `postgres_pool_opened`; pool gauge task | `configuration is invalid: postgres.dsn: ...`; `postgres connect: ...` |
| 14 | `Engine::check_startup` (UTF-8 server, writable session, and `READ COMMITTED` default) | `jobs startup check: the jobs store requires READ COMMITTED session isolation` / `the PostgreSQL session is not writable` / `the jobs store is unavailable` |
| 15 | Bind the health listener (`http.addr`), then the diagnostics listener (`observability.metrics.addr`, when set); `http listener bound`, `diagnostics listener bound` | `bind http listener ...` |
| 16 | If a stop signal is already pending (`Signals::pending()`), start no engine and run the shutdown plan with no engine. Otherwise `Engine::start` (claiming begins); `jobs_claiming_started` | |
| 17 | Readiness admission (`refresh`, then `verdict` over `[PostgresProbe]`), raced against the stop signals: a signal abandons admission, and the shutdown plan runs with the started engine | `startup admission: ...` |
| 18 | Refresher task; `jobs_worker_ready` | |
| 19 | Wait for a stop signal or `Started::failed()` | |

Steps 1-10 do no database I/O. Step 2 (no registration) precedes
configuration so the shipped binary refuses the same way everywhere, and a
derived service's registration runs after the configuration checks. Signal
streams exist from step 8, so a stop during steps 9-15 stays pending. A stop
pending before claiming starts no engine, and a signal during admission ends
startup at once. Before admission, `/health/ready` answers `503 not ready`
(not evaluated). Every refusal after the runtime started goes through the
one teardown, `abort_startup`: finish attempts within 2 s when the engine started,
close bound listeners within 2 s, join background tasks within 3 s, and
close the pool within 5 s. It flushes no telemetry.

**Readiness.** `/health/ready` uses the service's cached-verdict semantics
with the PostgreSQL probe. The worker is ready only after startup completed
and claiming began. It is not ready at the first stop trigger. The health
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
| Readiness off, claiming stopped | immediate | `shutdown_started`, `readiness_disabled`, `claiming_stopped` (in-flight count) | never; a claim already dispatched may settle under its existing backstop |
| Drain in-flight attempts until `Started::drained()`; a second signal ends it | `http.drain_timeout` (25 s); no propagation delay | `drain_started`, then `drain_completed` or `drain_forced` (in-flight attempts, reason `budget` or `second_signal`) | the drain ends before `drained()` resolved (forced) |
| Only after a forced drain: `Started::cancel_and_finish` | 2 s | `attempts_finished` (known results, cancelled handlers, acknowledged releases, uncertainty) | `DrainEnd::timed_out` |
| Close the health listener and the diagnostics listener concurrently | 2 s | `listeners_stopped`; `diagnostics_forced` for a scrape overrun | the health listener overruns (a diagnostics overrun is forced closed without a vote, as in the service) |
| Cancel and join background tasks (claim loop, retention, sampler, metrics, refresher) | 3 s | `background_joined` | the join overruns |
| Close the pool | 5 s | `postgres_pool_closed` | the close overruns |
| Flush telemetry | 5 s | `telemetry_flushed`, `shutdown_completed` | the flush is incomplete |

When a stop signal ended startup before claiming, the plan has no engine, so
the drain and cleanup stages are skipped. The tail after the drain is
2 + 2 + 3 + 5 + 5 = 17 s. `validate_grace_budget` refuses a grace period
below `http.drain_timeout` plus 17 s. The default worst case is
25 s + 17 s = 42 s inside 45 s, leaving the same 3 s the service leaves for
`Runtime::shutdown_timeout(1s)` and the tracer join slack. The worker has no
readiness propagation delay: it stops claiming at once. The background join
is 3 s (the service's is 5 s) because attempts are drained and their outcomes
are finished before that stage and every joined task stops at its next await.

**Exit codes.** `exit_code` is the one mapping. `process::exit` is never
called.

| Code | When |
| --- | --- |
| `0` | A stop signal, and every stage completed inside its ceiling; the drain ended with `drained()`, so no attempt was cancelled at its end |
| `3` | A stop signal, and any stage voted degraded, including a forced drain (budget or second signal), which is the only way an attempt is cancelled at the drain's end |
| `1` | A startup refusal, or `Started::failed()` resolved without a stop signal. After the failure the same staged plan runs, with its deadline starting at the failure, and its outcome does not change the code |

The [guide](../background-jobs.md#run-and-stop-the-worker) covers running and
stopping the worker. [Async Architecture](async.md) records the mechanism.
<!-- template:end jobs:docs-lifecycle-jobs-worker -->

## Decisions Recorded Here

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
