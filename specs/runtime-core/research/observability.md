# Lane report: structured logs, distributed traces, metrics

Raw research lane output, 2026-09-17. Versions, dependency requirements and
features were pulled from the crates.io API; behaviour was checked against
upstream source (opentelemetry-rust at tag `opentelemetry-otlp-0.32.0`) and
then executed: the recommended set resolved with `cargo tree -d` to a single
OpenTelemetry version, the init/shutdown sketch compiled, and a throw-away
probe service under `/tmp` confirmed traceparent extraction, `http.route`
templates, bounded metric labels, `log` bridging and trace/request IDs in
JSON logs. Decisions are consolidated in [synthesis.md](synthesis.md).

## Logs / formatting

| Crate | Latest (date) | Maintenance | Fit |
|---|---|---|---|
| `tracing` | 0.1.44 (2025-12-18) | tokio-rs; 192M recent dl | The facade |
| `tracing-subscriber` | 0.3.23 (2026-03-13) | tokio-rs; 143M recent dl | `EnvFilter`, `fmt`, `registry`. `json` feature is rigid: fixed key names, RFC3339 strings only, dotted keys not nested, no trace id (issues #2129, #1718, #663, #2206, #3148, #2381). Default `tracing-log` feature makes `try_init()` install `LogTracer` automatically (verified) |
| `json-subscriber` | 0.3.0 (2026-07-08) | mladedav (tracing-opentelemetry contributor); 1.09M recent dl | Drop-in JSON layer: `flatten_event`, `flatten_span_list_on_top_level`, static/dynamic fields, renames, `with_opentelemetry_ids(true)` → `openTelemetry.traceId/spanId`; feature `tracing-opentelemetry-0-33`. Verified running. Enable `tracing-log` feature to normalise bridged records |
| `tracing-bunyan-formatter` | 0.3.10 (2024-11-26) | last push 2024-11 | Bunyan JSON, noisy span events, no OTel ids, `tracing-log ^0.1`. Stale |
| `tracing-logfmt` | 0.3.7 (2026-01-30) | Embark | logfmt only |
| `tracing-log` | 0.2.0 (2023-10-25) | tokio-rs; pulled by tracing-subscriber default features | The `log` → `tracing` bridge; nothing to add directly. Verified `log::warn!` appears in JSON with no extra code |
| `opentelemetry-appender-tracing` | 0.32.0 (2026-05-09) | otel-rust, "Stable" | `tracing` → OTel Logs; records get trace/span ids from the OTel context activated by tracing-opentelemetry ≥0.32 |
| `opentelemetry-stdout` | 0.32.0 | otel-rust | Debug exporter only |

## Traces (core)

| Crate | Latest (date) | Fit |
|---|---|---|
| `opentelemetry` | 0.32.0 (2026-05-08) | API. Repo status: Traces Beta, Metrics/Logs API+SDK Stable, OTLP metrics/logs RC. Cadence: 0.29 (Mar-25), 0.30 (May-25), 0.31 (Sep-25), 0.32 (May-26) |
| `opentelemetry_sdk` | 0.32.1 (2026-05-26) | Since 0.28 the `BatchSpanProcessor`/`PeriodicReader`/`BatchLogProcessor` run on their own std thread; `rt-tokio` only for `experimental_*_with_async_runtime`. `SdkTracerProvider::shutdown_with_timeout(d)` is real; `SdkMeterProvider::shutdown_with_timeout(_d)` ignores its argument |
| `opentelemetry-otlp` | 0.32.0 (2026-05-08) | Defaults: `http-proto`, `reqwest-blocking-client`. `grpc-tonic` (tonic 0.14) must be built inside a Tokio runtime. 0.32 added transport auto-selection from `OTEL_EXPORTER_OTLP_PROTOCOL`, per-signal protocol vars, refuses `https://` without a TLS feature. Retries experimental in 0.32.0 |
| `tracing-opentelemetry` | 0.33.0 (2026-05-18) | Version offset: tracing-opentelemetry = opentelemetry + 1 (0.33 ↔ 0.32). 0.32+ activates the OTel context on span enter; 0.33 made `OtelData` private (PR #251), use `get_otel_context` or `OpenTelemetrySpanExt::context()` |
| `opentelemetry-semantic-conventions` | 0.32.1 (2026-06-26) | `attribute::{HTTP_ROUTE, HTTP_REQUEST_METHOD, HTTP_RESPONSE_STATUS_CODE, SERVER_ADDRESS, URL_PATH}` and `resource::{SERVICE_NAME, SERVICE_VERSION, SERVICE_INSTANCE_ID, DEPLOYMENT_ENVIRONMENT_NAME}` stable; `HTTP_SERVER_ACTIVE_REQUESTS`, `PROCESS_*` need `semconv_experimental` |
| `opentelemetry-resource-detectors` | 0.12.0 (2026-08-28) | contrib, beta. `ServiceInstanceIdResourceDetector` (UUIDv7 once per process), process/os/host/k8s/container detectors. The SDK does not generate `service.instance.id` itself |
| `opentelemetry-prometheus` | 0.32.0 (2026-05-08) | 0.29.1 → skipped 0.30 with a discontinuation notice → 0.31.0 (Dec-25) → 0.32.0 explicitly un-deprecated (#3288). README now: "for new projects consider OTLP → Prometheus native OTLP receiver". Depends on `prometheus ^0.14` and SDK `experimental_metrics_custom_reader`. Second-tier |

## HTTP instrumentation for axum

| Crate | Latest (date) | Deps (verified) | `http.route` | Fit |
|---|---|---|---|---|
| `axum-tracing-opentelemetry` + `tracing-opentelemetry-instrumentation-sdk` | 0.39.1 / 0.38.3 (2026-08-30) | otel ^0.32, tracing-otel ^0.33, axum ^0.8, MSRV 1.91; repo pushed 2026-09-06; 2.0M/2.9M recent dl | `MatchedPath` from extensions; `otel.name = "{method} {route}"`; unmatched → `""` | `OtelAxumLayer` extracts W3C context through the global propagator, span kind Server, semconv attributes (`http.request.method`, `server.address/port`, `url.path/query/scheme`, `user_agent.original`, `http.response.status_code`), `otel.status_code=ERROR` on 5xx; `OtelInResponseLayer` puts `traceparent` in the response; span pre-declares `request_id = Empty`. Gotcha: span level is TRACE unless feature `tracing_level_info` (reproduced: `RUST_LOG=info` dropped every HTTP span). Recommended |
| `tower-otel` | 0.10.0 (2026-05-18) | otel ^0.32, tracing-otel ^0.33; 14 stars; 0 rev deps | `MatchedPath` → attribute omitted when unmatched | Spans and semconv metrics (`http.server.request.duration`, `active_requests`). Found in source: span name is the constant `"HTTP"` and every request/response header is copied into span attributes without redaction. Metrics layer fine; spans not yet |
| `tower-otel-http-metrics` | 0.16.0 (2025-06-22) | otel ^0.30 | — | Stale, pulls a second `opentelemetry` → metrics vanish silently |
| `axum-otel-metrics` | 0.14.1 (2026-07-11) | otel ^0.32, semconv ^0.32 | `MatchedPath` | OTel-native HTTP metrics with bounded labels; fine for OTel-metrics-only |
| `tower-http` `TraceLayer` | 0.7.1 (2026-08-31) | http 1 / tower 0.5 | axum docs fall back to `req.uri().path()` (cardinality trap) | Logging hooks, not OTel propagation |
| `axum-prometheus` | 0.10.1 (2026-07-31) | axum ^0.8, metrics ^0.24.6, metrics-exporter-prometheus ^0.18.3, tower-http ^0.7; 1.37M recent dl | `EndpointLabel::{Exact, MatchedPath (default, falls back to raw path), MatchedPathWithFallbackFn}` | metrics-rs based `axum_http_requests_total`, `_duration_seconds`, `_pending`; must use `MatchedPathWithFallbackFn` for bounded labels (verified `endpoint="UNMATCHED"` for 404s) |

## Metrics facades and exporters

| Crate | Latest (date) | Maintenance | Fit |
|---|---|---|---|
| `metrics` | 0.24.6 (2026-05-13) | metrics-rs; 19.9M recent dl; 1195 rev deps | The de-facto Rust metrics facade |
| `metrics-exporter-prometheus` | 0.18.3 (2026-04-30) | metrics-rs; 459 rev deps | `install_recorder()` → `PrometheusHandle::render()` for a diagnostics router, or `with_http_listener(addr).install()`. Bucket configuration by matcher, idle timeout, global labels. Default features include `push-gateway` (pulls hyper-rustls/aws-lc): disable defaults |
| `metrics-process` | 2.4.3 (2026-02-03) | pushed 2026-09-05 | Standard `process_*` family (cpu seconds, RSS, fds, start time, threads) on Linux/macOS/Windows/FreeBSD; call `collect()` per scrape (verified) |
| `tokio-metrics` | 0.5.2 (2026-08-28) | tokio-rs | `RuntimeMetricsReporterBuilder::default().describe_and_run()` with `metrics-rs-integration` exports worker/task/busy/queue metrics without `tokio_unstable` (verified) |
| `prometheus` (tikv) | 0.14.0 (2025-03-27) | 96 open issues | Only as the sink for `opentelemetry-prometheus` |
| `prometheus-client` | 0.25.1 (2026-09-01) | official | OpenMetrics-native, no axum/tokio/process glue |
| `metrics-exporter-opentelemetry` | 0.2.1 (2025-11-15) | pins otel 0.31 | Lags one release; do not build on it |

## Recommended compatible version set (resolved and compiled on Rust 1.98.1)

```toml
tracing = "0.1"                                   # 0.1.44
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }  # 0.3.23
json-subscriber = { version = "0.3", features = ["tracing-opentelemetry-0-33", "tracing-log"] }
opentelemetry = "0.32"
opentelemetry_sdk = { version = "0.32", features = ["trace", "metrics"] }   # 0.32.1
opentelemetry-otlp = { version = "0.32", default-features = false,
  features = ["grpc-tonic", "http-proto", "reqwest-blocking-client", "trace", "metrics", "internal-logs"] }
opentelemetry-semantic-conventions = { version = "0.32", features = ["semconv_experimental"] }
opentelemetry-resource-detectors = "0.12"
tracing-opentelemetry = "0.33"
axum-tracing-opentelemetry = { version = "0.39", features = ["tracing_level_info"] }
metrics = "0.24"
metrics-exporter-prometheus = { version = "0.18", default-features = false, features = ["http-listener"] }
axum-prometheus = "0.10"
metrics-process = "2.4"
tokio-metrics = { version = "0.5", features = ["metrics-rs-integration"] }
```

`cargo tree -d`: exactly one `opentelemetry 0.32.0`, `opentelemetry_sdk
0.32.1`, `tracing-opentelemetry 0.33.0`; the only duplicate is `tower-http`
(0.6.11 via `reqwest`, 0.7.1 direct), benign.

## Recommendations

- Logs: `tracing` + `tracing-subscriber` (`EnvFilter`, `fmt::layer().pretty()`
  in dev) + `json-subscriber` in production with `flatten_event`,
  `flatten_span_list_on_top_level`, `with_opentelemetry_ids(true)`. Verified
  a handler log line carrying `openTelemetry.traceId/spanId`, `http.route`,
  `otel.name`, `request_id` with no per-call boilerplate. `log` bridge is
  automatic through `try_init()`. `request_id`: tower-http request-id layers
  plus a small `from_fn` doing `Span::current().record("request_id", ..)`
  into the pre-declared field.
- Traces: SDK 0.32 + OTLP 0.32 + tracing-opentelemetry 0.33 +
  axum-tracing-opentelemetry 0.39 (`tracing_level_info`). Exporter
  `grpc-tonic` when you control the collector, `http-proto` +
  `reqwest-blocking-client` for zero runtime coupling; both supported by the
  dedicated-thread processors. Only call `.with_sampler()` when typed config
  sets one; otherwise the SDK reads `OTEL_TRACES_SAMPLER(_ARG)` (verified
  `always_on`, `always_off`, `traceidratio`, `parentbased_*`). Resource:
  `Resource::builder_empty().with_detector(ServiceInstanceIdResourceDetector).with_detectors([SdkProvided, Telemetry, Env]).with_attributes(typed)`
  gives UUID < env < typed precedence (verified). Set
  `global::set_text_map_propagator(TraceContextPropagator::new())` yourself.
- Metrics: `metrics` + `metrics-exporter-prometheus` (render from a
  diagnostics router on `:9090`) + `axum-prometheus` with
  `MatchedPathWithFallbackFn(|_| "UNMATCHED")` and ignore patterns for
  `/metrics` and health routes + `metrics-process` + `tokio-metrics`. This is
  the Rust equivalent of the Go `promhttp` path and has by far the widest
  adoption. OTLP metric push is an optional addition (`SdkMeterProvider` +
  `PeriodicReader` + `axum-otel-metrics` or `tower-otel` metrics layer);
  process and Tokio metrics have no OTel-native crates, so a collector
  `prometheus` receiver scraping `:9090` is the path for OTLP-only platforms.
- Failure tolerance: every builder returns `Result`; log and continue with
  `None` provider. Verified with the collector down: spans buffered, one
  export error log, clean exit in ~0.3 s.
- Shutdown: `provider.shutdown_with_timeout(budget)` inside `spawn_blocking`,
  wrapped in `tokio::time::timeout`, before the runtime is dropped.

## What the SDK reads from the environment (verified at the 0.32.0 tag)

| Variable | Where |
|---|---|
| `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES` | `Resource::builder()` Env + SdkProvided detectors; `.with_attributes()` afterwards overrides env; not read by `builder_empty()` |
| `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG` | `trace::Config::default()`; `with_sampler()` overrides; unknown → warn + `ParentBased(AlwaysOn)` |
| `OTEL_SPAN_{ATTRIBUTE,EVENT,LINK}_COUNT_LIMIT` | `trace::Config::default()` |
| `OTEL_BSP_*`, `OTEL_BLRP_*` | batch processors |
| `OTEL_METRIC_EXPORT_INTERVAL` (60 s), `OTEL_METRIC_EXPORT_TIMEOUT` | `PeriodicReader` |
| `OTEL_EXPORTER_OTLP_{ENDPOINT,PROTOCOL,TIMEOUT,HEADERS,COMPRESSION,INSECURE}` and per-signal forms, `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE` | otlp builders; builder methods win; signal-specific wins over generic; HTTP appends `/v1/traces` etc.; defaults `localhost:4318`/`:4317` |
| Not read: `OTEL_PROPAGATORS`, `OTEL_SDK_DISABLED`, `OTEL_LOG_LEVEL`, `OTEL_{TRACES,METRICS,LOGS}_EXPORTER`, `OTEL_EXPORTER_OTLP_{CERTIFICATE,CLIENT_KEY,CLIENT_CERTIFICATE}`, `OTEL_ATTRIBUTE_VALUE_LENGTH_LIMIT` | typed config must own these |

## Gotchas

1. Propagator is Noop until `set_text_map_propagator`; both HTTP layers
   extract through the global, so forgetting it yields a new root trace per
   request silently.
2. davidB HTTP spans are TRACE-level without `tracing_level_info`.
3. Version offset (tracing-opentelemetry = otel + 1); a crate pinning another
   otel minor creates a second `global::` and its data goes to Noop. Check
   `cargo tree -d -i opentelemetry`.
4. `OtelData` private since 0.33; use `get_otel_context`. Unsampled spans
   still carry a valid trace id in logs.
5. Default processors drive exporters with `block_on` on a dedicated thread;
   `grpc-tonic` captures the Tokio handle (build inside the runtime, shut
   down before it drops); `reqwest-client`/`hyper-client` are not supported
   by default processors, and feature unification from any dependency can
   flip the exporter (`cargo tree -e features -i opentelemetry-otlp`). Never
   call `shutdown()` on a `current_thread` runtime thread.
6. Meter provider ignores the shutdown timeout argument; bound with
   `spawn_blocking` + `timeout`.
7. `init()` panics on a second dispatcher; use `try_init()`. SDK warnings
   emitted during provider construction are lost if the subscriber is not
   installed yet (reproduced).
8. `http.route` cardinality: `MatchedPath` absent on 404/fallback and
   `nest_service`; axum docs and `axum-prometheus` default fall back to the
   raw path. Use the fallback fn and ignore patterns; keep `server.address`,
   `url.path`, `user_agent` off metrics.
9. `tower-otel` (main 2026-09-16): span name `"HTTP"`, all headers recorded
   unredacted.
10. `opentelemetry-prometheus`: un-deprecated in 0.32 but Beta, depends on
    tikv `prometheus` and an experimental SDK feature.
11. tracing-opentelemetry turns every event into a span event; adding
    `opentelemetry-appender-tracing` duplicates them unless per-layer filtered.
12. OTel Rust is Beta for traces and every minor is breaking; pin `=0.32.x`
    and bump the whole family together.

## Unverified

Adoption inferred from crates.io downloads and reverse dependencies, not a
survey. docs.rs pages were not rendered; API and env-var claims come from
source. `tower-otel` findings are from `main`, not diffed against 0.10.0.
`tracing-bunyan-formatter` and `tracing-logfmt` assessed from metadata. OTLP
export was exercised only against a down collector.

## Sources

crates.io API (crate, versions, dependencies, reverse dependencies) for every
crate above; open-telemetry/opentelemetry-rust (README status table,
CHANGELOGs of sdk/otlp/prometheus/appender-tracing, `trace/config.rs`,
`trace/provider.rs`, `trace/span_processor.rs`, `resource/{mod,env}.rs`,
`metrics/{periodic_reader,meter_provider}.rs`, `logs/logger.rs`, otlp
`lib.rs` and exporter modules, semantic-conventions sources,
`global/propagation.rs`; shallow clone at tag `opentelemetry-otlp-0.32.0`);
opentelemetry-rust-contrib resource detectors; opentelemetry.io Rust status;
tokio-rs/tracing-opentelemetry (README, CHANGELOG, `layer.rs`,
`otel_context.rs`, `span_ext.rs`, PR #251); tokio-rs/tracing
(`tracing-subscriber/src/util.rs`, tracing-log, JSON issues); tokio-rs/axum
`matched_path.rs`; davidB/tracing-opentelemetry-instrumentation-sdk (README,
CHANGELOG, `trace_extractor.rs`, `http_server.rs`, `tools.rs`);
mattiapenati/tower-otel sources; ttys3/axum-otel-metrics;
francoposa/tower-otel-http-metrics; mladedav/json-subscriber (README,
CHANGELOG, `layer/mod.rs`, `fmt/layer.rs`); metrics-rs
`metrics-exporter-prometheus/src/exporter/builder.rs`; Ptrskay3/axum-prometheus
(`lib.rs`, `builder.rs`, `utils.rs`); lambdalisue/rs-metrics-process;
tokio-rs/tokio-metrics; tower-rs/tower-http CHANGELOG; GitHub REST API for
repo activity. Probe project executed under `/tmp` (outside the repository).
