# Stage 2 research synthesis: runtime core

Decisions for roadmap stage 2 (configuration, logging, telemetry, hardened
HTTP, readiness, drain). Evidence lives in the four lane reports beside this
file: [configuration.md](configuration.md), [observability.md](observability.md),
[http-hardening.md](http-hardening.md), [lifecycle.md](lifecycle.md). All
versions were read from crates.io on 2026-09-17; every behavioural claim
marked *verified* was executed against Rust 1.98.1.

The Go template supplies the problems and the reasons behind its choices.
Where Rust solves a problem differently, the Rust way wins and the deviation
is recorded in the last section.

## Decisions

| Area | Decision | Crates (version) | Template-owned code (named gaps) |
| --- | --- | --- | --- |
| Config loading | Layered `config` builder: code defaults via `#[serde(default)]` + `impl Default` per section → `--config` file → ordered `--config-overlay` files → `APP__SECTION__KEY` env; `#[serde(deny_unknown_fields)]` on every section | `config` 0.15 (`toml` feature only), `clap` 4 (derive), `serde`, `humantime-serde` 1.1, `bytesize` 2, `secrecy` 0.10 | Pre-scan of `APP__*` names for empty segments (config-rs reports `unknown field ".x"`, not the variable); pre-scan of each file for non-empty secret-like values (`toml::Table` walk); `validate()` per section with cross-field rules naming the key |
| Config file format | TOML default; `env/config/local.toml` | `toml` 1.1 (through config-rs) | — |
| Secrets | `secrecy::SecretString` fields; `Debug` prints `[REDACTED]` (verified); env is the only secret source | `secrecy` | Secret-like key predicate (password, secret, token, dsn, authorization, api_key, private_key, otlp_headers) shared by the file pre-scan |
| Logs | `tracing` + `tracing-subscriber` `EnvFilter`; `log.format = json` → `json-subscriber` with flattened event and span fields plus `openTelemetry.traceId/spanId` on every record inside a request (verified); `log.format = text` → `fmt::layer()` for local use; `log` records bridged automatically by `try_init()` | `tracing` 0.1, `tracing-subscriber` 0.3, `json-subscriber` 0.3 (`tracing-opentelemetry-0-33`, `tracing-log`) | None |
| Traces | OTel SDK with a tracer provider always installed (trace ids exist for log correlation even without a collector); OTLP HTTP/protobuf batch exporter added only when a typed endpoint or a standard `OTEL_EXPORTER_OTLP_*ENDPOINT` variable resolves one; typed sampler always set from config (`parentbased_traceidratio` 0.10 default); resource = detectors (`Env`, `SdkProvided`, `Telemetry`) under typed `service.name`, `service.version`, `vcs.revision`, `service.instance.id`, `deployment.environment.name`; `TraceContextPropagator` installed explicitly | `opentelemetry` 0.32, `opentelemetry_sdk` 0.32, `opentelemetry-otlp` 0.32 (`http-proto`, `reqwest-blocking-client`, `trace`), `opentelemetry-semantic-conventions` 0.32, `tracing-opentelemetry` 0.33, `axum-tracing-opentelemetry` 0.39 (`tracing_level_info`) | Endpoint resolution (typed wins, else env) and the ambient-credential refusal: when the typed endpoint selects the destination, present `OTEL_EXPORTER_OTLP_*HEADERS`, `*_CERTIFICATE`, `*_CLIENT_KEY`, `*_CLIENT_CERTIFICATE` variables fail validation so one collector's credential is never sent to another; startup state `initialized`/`disabled`/`degraded` |
| Metrics | `metrics` facade; Prometheus text rendered by `metrics-exporter-prometheus` from a diagnostics router on `observability.metrics.addr` (default `0.0.0.0:9090`, empty disables); HTTP server metrics from `axum-prometheus` with `MatchedPathWithFallbackFn(\|_\| "UNMATCHED")` for bounded route labels (verified); process metrics from `metrics-process`; runtime metrics from `tokio-metrics`; `http_server_shed_requests_total` and `service_startup_trace_exporter_active` through the facade | `metrics` 0.24, `metrics-exporter-prometheus` 0.18 (`default-features = false`), `axum-prometheus` 0.10, `metrics-process` 2.4, `tokio-metrics` 0.5 (`metrics-rs-integration`) | The diagnostics router (one route) |
| Request middleware | tower-http layers in `Router::layer` so 404/405 are covered: request-id set + propagate, `X-Content-Type-Options: nosniff`, OTel server span, access log, `HandleErrorLayer` mapping `Overloaded` → 503 + `Retry-After: 1` and `Elapsed` → 504, `LoadShedLayer` outside `GlobalConcurrencyLimitLayer` (a plain `ConcurrencyLimitLayer` becomes per-route under `Router::layer`, verified in source), `tower::timeout`, `CatchPanicLayer::custom`, body limit, `DefaultBodyLimit`; `Router::fallback` and `method_not_allowed_fallback` return Problem bodies, axum adds `Allow`; no `CorsLayer` (fail-closed: an empty layer would answer every `OPTIONS` with 200) | `tower-http` 0.7 (`request-id`, `set-header`, `catch-panic`, `limit`, `trace`), `tower` 0.5 (`limit`, `load-shed`, `timeout`) | Inbound `X-Request-ID` validation (`^[A-Za-z0-9._~-]{1,128}$`; tower-http trusts any present header); one-line access log via `from_fn` with `Option<MatchedPath>` and route-based probe suppression; 413 Problem body (tower-http's short-circuit is `text/plain`); the `Problem` type and closed catalog; the three error closures |
| Problem Details | Template-owned `Problem` type (~60 lines) with `code`, `request_id`, `invalid_params` first-class and `application/problem+json`; `problem_details` 0.10 is the acceptable crate alternative, `problemdetails` 0.7 is not (pins tower-http 0.6) | `serde`, `serde_json` | The type and catalog |
| HTTP server | Hand-rolled accept loop over `hyper_util::server::conn::auto::Builder` with `TokioTimer`, `TowerToHyperService`, `GracefulShutdown`, a `Semaphore(max_connections)` permit per connection task, and a bounded `peek` before handing the socket to hyper (hyper #3756: the `auto` builder starts no timer until the first byte); `axum::serve` sets no timer and exposes no limits, so hyper's default header timeout is silently disabled there (axum #2741) | `hyper` 1.11, `hyper-util` 0.1 (`server`, `server-auto`, `server-graceful`, `service`, `tokio`), `tokio` | The accept loop (~80 lines) |
| Readiness | Background refresher publishing a snapshot through `tokio::sync::watch`; `/health/ready` reads the snapshot in O(1); failure threshold before flipping unhealthy; staleness guard turns a dead refresher into not-ready; drain flag read first. No maintained crate does this (`health` unmaintained since 2022, `axum-health` probes per request) | `tokio` (`sync`, `time`) | The `health` crate (~150 lines) |
| Lifecycle | Runtime owned in `bootstrap::run`; `CancellationToken` + `TaskTracker` for background tasks; signal streams created before anything can send a signal and kept alive; stages draw from one grace-period deadline: readiness off → propagation delay → HTTP drain (`timeout(budget, graceful.shutdown())`) → cancel and join tasks → close dependencies → flush telemetry; `Runtime::shutdown_timeout` force-drops connection tasks that outlived the drain | `tokio-util` 0.7 | The stage sequencer (~120 lines); exit codes 0 graceful, 3 degraded shutdown, 1 startup failure |
| Build metadata | `env!("CARGO_PKG_VERSION")` for `app.version`; `vergen-gitcl` 10 with `default_on_error()` for `app.commit`, overridable through `VERGEN_GIT_SHA` from the image build argument | `vergen-gitcl` 10 (build dependency) | `build.rs` (5 lines) |
| Process tests | `env!("CARGO_BIN_EXE_service")`, ephemeral port through `APP__HTTP__ADDR=127.0.0.1:0` and the bound address from the JSON startup log, readiness poll, `nix::sys::signal::kill(SIGTERM)` (`Child::kill` is SIGKILL), assert exit code and drain timing | `nix` 0.31 (`signal`), `ureq` 3 (`default-features = false`), `serde_json` (dev) | The test |

## Version set

All OpenTelemetry crates stay on one minor and move together;
`tracing-opentelemetry` is always one ahead (0.33 ↔ 0.32). A dependency that
pins another OpenTelemetry minor creates a second `global::` and its data
goes to a no-op provider silently: check with `cargo tree -d -i opentelemetry`
after every dependency change. The lane resolved and compiled the set below
with exactly one `opentelemetry`, `opentelemetry_sdk` and
`tracing-opentelemetry`; the only duplicate is `tower-http` 0.6 pulled by
`reqwest`, which is benign.

## Deviations from the Go template

| Go template | Rust template | Why |
| --- | --- | --- |
| YAML baseline files | TOML baseline files | TOML is the Rust ecosystem convention and `toml` is actively maintained; YAML has no canonical maintained serde crate (`serde_yaml` archived, `serde_yml` under RUSTSEC-2025-0068, `saphyr-serde` a placeholder). config-rs's `yaml` feature remains available for a service that must consume YAML |
| `http.read_timeout`, `http.write_timeout`, `http.idle_timeout` connection deadlines | `http.header_read_timeout` only, which hyper 1.x restarts whenever an HTTP/1 connection goes idle, so it is both the header and the keep-alive idle bound; the handler budget `http.request_timeout` bounds body reads because extractors run inside the handler future | hyper has no per-connection read/write deadlines. A "5 s header / 60 s idle" split is not expressible without a custom IO wrapper; one value covers both risks. Streaming response bodies remain unbounded and are a candidate for tower-http's `ResponseBodyTimeoutLayer` when a streaming operation exists |
| `readiness_timeout <= write_timeout`, `request_timeout + 1s <= write_timeout` | `readiness_timeout` bounds the background probe only; `request_timeout <= shutdown_timeout - readiness_propagation_delay` so in-flight requests can finish inside the drain | The readiness handler never runs a probe, and there is no write deadline to reserve against |
| `runtime.memory_limit_ratio`, GOMAXPROCS awareness | Not ported | No garbage collector; `available_parallelism` honours cgroup quotas since Rust 1.61/1.64 and Tokio sizes its pool from it |
| `observability.pprof.enabled` | Not ported | No standard-library profiler to expose; the diagnostics listener serves `/metrics` only |
| OpenTelemetry SDK metrics with a Prometheus exporter and OTLP metric push | `metrics` facade with a Prometheus exporter; OTLP metric push deferred | The facade is the dominant Rust idiom (order-of-magnitude adoption gap), process and Tokio metrics have no OTel-native crates, and `opentelemetry-prometheus` was deprecated, un-deprecated in 0.32 and is still Beta on an experimental SDK feature. A collector `prometheus` receiver scraping `:9090` serves OTLP-only platforms; native push returns when a platform needs it |
| OTLP HTTP endpoint typed plus ambient `OTEL_EXPORTER_OTLP_*` fallback, ambient credentials rejected under a typed endpoint | Same policy; the SDK already implements the endpoint fallback and per-signal precedence itself | The safety property is kept as a validation rule; the mechanism is the SDK's |
| Trace exporter `disabled` when no endpoint | Tracer provider always installed, exporter only when an endpoint resolves | Trace ids in every log line for correlation cost nothing without an exporter and avoid connection-refused noise against the SDK's `localhost:4318` default |
| Per-request readiness probes with cached verdict | Same model, implemented over `tokio::sync::watch` | Idiomatic snapshot publication; tests await `changed()` instead of sleeping |
| `log.level` only | `log.level` (an `EnvFilter` directive) and `log.format` (`json`/`text`) | Human-readable local logs are a Rust convention; `RUST_LOG` is not read, `APP__LOG__LEVEL` is the override channel as for every other key |
| `-ldflags -X` version and commit | `CARGO_PKG_VERSION` and `vergen-gitcl` | Cargo owns the version; vergen owns the SHA fallbacks |
| Request id from `rand.Text()` | UUIDv4 from `tower_http::request_id::MakeRequestUuid` | Fits the accepted grammar; no custom generator |
| Manual `Allow` computation on 405 | axum's automatic `Allow` | Built in |
| One error exit code | 0 graceful, 3 degraded shutdown, 1 startup failure | The process test and the platform can distinguish an expired drain budget from a crash |

## Deferred

- OTLP metric push and `opentelemetry-appender-tracing` (OTLP logs): no
  current platform requirement; both resolve into the same version set.
- `ResponseBodyTimeoutLayer`: no streaming operation exists yet.
- Rate limiting (`tower_governor`) and CORS enablement: profile decisions.
- Allocator choice (`jemalloc`/`mimalloc`): a measured decision for the
  performance stage.

## Gotchas carried into implementation

1. `global::set_text_map_propagator` must be called explicitly or every
   request starts a new root trace silently.
2. `axum-tracing-opentelemetry` spans are TRACE-level without the
   `tracing_level_info` feature.
3. `SdkMeterProvider::shutdown_with_timeout` ignores its argument; bound
   telemetry shutdown with `spawn_blocking` + `timeout`.
4. `try_init()` for the subscriber; SDK warnings emitted during provider
   construction are lost if the subscriber is not installed yet, so install
   the subscriber before building providers and add the OTel layer through
   `tracing_subscriber::reload`, or accept the loss for the sampler warning
   that config validation already prevents.
5. `TaskTracker::wait()` needs `close()`; `CancellationToken::child_token()`
   is one-directional.
6. `tokio::signal::unix::signal` streams must outlive the process; a dropped
   stream swallows later signals.
7. `header_read_timeout(Some(_))` without `timer()` panics at
   `serve_connection`; `max_buf_size` below 8192 panics.
8. `MatchedPath` is absent in `Router::fallback`; the access log and metrics
   label unmatched routes explicitly.
9. `metrics-exporter-prometheus` default features pull `push-gateway` and a
   TLS stack; disable defaults.
10. Feature unification: any dependency enabling
    `opentelemetry-otlp/reqwest-client` flips the exporter to the async
    client, which the default batch processor does not support. Check
    `cargo tree -e features -i opentelemetry-otlp` after dependency changes.
