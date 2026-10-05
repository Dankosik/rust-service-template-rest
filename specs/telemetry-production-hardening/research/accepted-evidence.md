# Accepted telemetry research evidence

This note carries the parent coordinator's accepted TELEM-R2 result into
Definition. The parent reported independent research review PASS. That verdict
is inherited evidence, not a new runtime validation or a claim that this actor
repeated the research. Definition independently reopened the current provider
shutdown and panic mapping and the repository's lifecycle/configuration owners.
The coordinator's completeness check also identified the same caller-controlled
host/User-Agent path in the shared gRPC observer; Definition confirmed it in
[gRPC observation](../../../crates/infra-grpc/src/observe.rs), `make_span`.
The server privacy rule therefore covers it while preserving client destination
identity and existing admitted RPC method labels.

Baseline: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`, fetched main,
2026-10-05. Locked authorities: Rust 1.99, `opentelemetry_sdk` and
`opentelemetry-otlp` 0.33, `tracing-opentelemetry` 0.34,
`tracing-subscriber` 0.3.23, `metrics-exporter-prometheus` 0.18.3.
Dependency sources were inspected in the Cargo registry under
`/Users/daniil/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.
Reopen version-sensitive conclusions when the resolved versions change.

| Evidence and authority | Decision effect |
| --- | --- |
| [Baseline JSON formatter](https://github.com/Dankosik/rust-service-template-rest/blob/5927ffbba351af2f7fb8635316bbfa4ae5b31da6/crates/infra-telemetry/src/logging/json.rs) writes synchronously to stdout and ignores write failure; scratch storage retains its largest allocation and record fields have no byte cap. | Isolate stopped sinks, bound records and retained queue data, and expose local loss/error evidence. A queue count alone is insufficient. |
| [Trace provider](../../../crates/infra-telemetry/src/traces.rs) uses a stock dedicated serial batch processor: default queue 2048 finished sampled spans, batch 512, interval 5s. SDK-native bounded retries exist. A blocking HTTP attempt can outlive the nominal retry deadline; export-timeout/concurrency environment knobs do not control this processor. | Preserve stock processor and retry ownership; document actual limits instead of adding retries or assuming cancellation. |
| `Counted` observes SDK batch completion. HTTP partial rejection logs a warning but returns success; malformed success bodies can also return success. SDK shutdown discards the inner shutdown result and a multi-batch flush retains only the last result. | Do not equate SDK success, `ProviderShutdown::Flushed`, `telemetry_flushed`, or exit 0 with loss-free export or backend delivery. Preserve known failure across the final flush. |
| [HTTP observation](../../../crates/infra-http/src/observe.rs) retains raw path, query, User-Agent and server address; query redaction lists only four literal keys. [Panic recovery](../../../crates/infra-http/src/harden.rs) and the shared hook can log raw panic payloads. SDK diagnostics include receiver messages, bodies, and URLs. | Admit safe diagnostic fields before either local output or OTLP export. A sanitized HTTP Problem and Collector redaction do not repair source leakage. |
| [Metrics](../../../crates/infra-telemetry/src/metrics.rs) runs upkeep independently of scraping, but that requires worker progress; the registry has no global series cap or TTL. Existing HTTP labels and gRPC client method limits already constrain those emitters. | Document emitter ownership/cardinality and scrape/upkeep limits; no universal metric cap or bucket rewrite. |
| [Logging](../../../crates/infra-telemetry/src/logging.rs) preserves INFO spans under event filtering; trace sampling does not sample logs or metrics. Parent-based sampling can honor a sampled remote parent even at a lower root ratio. Existing typed endpoint/header and secret-source controls prevent credential crossover. | Preserve these contracts, explain sampling scope, and keep operational sampling choices outside the PR. |

`tracing-appender` is a candidate maintained bounded lossy writer, not an
accepted mechanism. Technical Design owns current crate comparison, byte
accounting, write-error visibility, lifecycle ownership, and admission.

Existing [HTTP/TLS delivery fixtures](../../../crates/infra-telemetry/src/traces/tests/delivery.rs)
and [process lifecycle owner](../../../docs/architecture/runtime-lifecycle.md)
provide local proof surfaces. Happy-path tests do not establish failure bounds.
No runtime fault or current performance outcome was measured in this research.
[Telemetry performance](../../../docs/infra-telemetry-performance.md) and
[the profiling report](../../hotpath-profiling/report.md) retain their original
measurement scope; no speedup or production capacity is inferred here.
