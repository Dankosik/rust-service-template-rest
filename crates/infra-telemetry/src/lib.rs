//! Telemetry adapters: the tracing subscriber, the OpenTelemetry tracer
//! provider, Prometheus metrics, and the diagnostics router.
//!
//! Setup never blocks the service: OTLP exporter construction degrades to a
//! logged reason and a no-op; ambient-credential conflicts, malformed typed
//! OTLP headers, unparsable log directives, and recorder install failures
//! still fail startup. Shutdown is
//! bounded by the caller's budget. The crate decisions are recorded in
//! `docs/configuration-source-policy.md`.

pub mod logging;
pub mod metrics;
pub mod traces;

pub use logging::{
    LogSnapshot, LoggerGuard, LoggerIncomplete, LoggerShutdown, LoggingError, LoggingFormat,
    LoggingOptions, install_panic_hook, install_subscriber,
};
pub use metrics::{
    DEFAULT_BUCKETS, Metrics, MetricsError, TRACE_EXPORTER_ACTIVE_METRIC,
    TRACE_SPANS_EXPORTED_METRIC, diagnostics_router, runtime_metrics,
};
pub use traces::{
    EndpointSource, ExporterState, ProviderShutdown, ProviderShutdownReasons, ResolvedSampler,
    TracerProviderHandle, TracingError, TracingOptions, install_tracer_provider,
};
