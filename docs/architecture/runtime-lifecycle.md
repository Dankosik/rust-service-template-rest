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
5. Dependency pools open when their profile is selected. With
   `postgres.enabled`, the DSN is admitted, the pool's first connection is
   established inside the acquire budget (`postgres_pool_opened`), the
   probe joins the readiness set, and the pool gauge task joins the tracker;
   an unreachable database is a startup failure, not a readiness that never
   passes ([Persistence](persistence.md)).
6. Readiness admission: the refresher evaluates every registered probe once
   under `health.readiness_timeout`; a failure is a startup failure (exit
   `1`). Without a selected profile the set is empty and admission proves
   the mechanism.
7. The route tree comes from `service::api::contract()`, is given the
   readiness reader as state, wrapped by `infra_http::harden`, and bound by
   the bounded `Server`; the diagnostics listener binds second when
   `observability.metrics.addr` is set. `service_ready` is logged only after
   both binds; the platform's first `/health/ready` poll answers from the
   admission evaluation.

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
| HTTP drain: stop accepting, finish in-flight requests | `http.shutdown_timeout` minus the delay (`10s`) | `drain_started`, then `drain_completed`, or `shutdown_forced` with `remaining` connections |
| Diagnostics listener close | `2s` | `diagnostics_stopped` or `diagnostics_forced` |
| Cancel and join background tasks | `5s` | `background_joined` |
| Close pooled dependencies (the PostgreSQL pool when enabled) | `5s` | `postgres_pool_closed`; an overrun votes `degraded` |
| Flush telemetry | `5s` | `telemetry_flushed`, then `shutdown_completed` |

The `17s` tail after the drain is process structure, not configuration;
`validate_grace_budget` refuses a configuration whose grace period cannot
hold `shutdown_timeout` plus the tail. The default worst case is `42s`
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

`--help` and `--version` exit `0`. `process::exit` is never called, so
destructors run.

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
- **Build metadata**: `app.version` is `CARGO_PKG_VERSION`; `app.commit` is
  `vergen-gitcl` with `default_on_error()`, overridable through
  `VERGEN_GIT_SHA`, which the image build sets from `VCS_REF` (or Railway's
  `RAILWAY_GIT_COMMIT_SHA`).
- **Process tests** use `CARGO_BIN_EXE_service`, an ephemeral port
  (`APP__HTTP__ADDR=127.0.0.1:0`) read back from the JSON startup log, a
  readiness poll, `nix` `SIGTERM` (`Child::kill` is `SIGKILL`), and assert
  the exit code and drain timing. `SdkMeterProvider::shutdown_with_timeout`
  ignores its argument, so telemetry shutdown is bounded with
  `spawn_blocking` plus `timeout`.
