//! Telemetry adapters: the tracing subscriber, the OpenTelemetry tracer
//! provider, Prometheus metrics, and the diagnostics router.
//!
//! Setup never blocks the service: OTLP exporter construction degrades to a
//! logged reason and a no-op; ambient-credential conflicts, unparsable log
//! directives, and recorder install failures still fail startup. Shutdown is
//! bounded by the caller's budget. The crate decisions are recorded in
//! `docs/configuration-source-policy.md`.

pub mod logging;
pub mod metrics;
pub mod traces;

pub use logging::{LogFormat, LoggingError, LoggingOptions, install_subscriber};
pub use metrics::{Metrics, MetricsError, TRACE_EXPORTER_ACTIVE_METRIC, diagnostics_router};
pub use traces::{
    ExporterState, ProviderShutdown, Sampler, TracerProviderHandle, TracingError, TracingOptions,
    install_tracer_provider,
};
