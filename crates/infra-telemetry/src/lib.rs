//! Telemetry adapters: the tracing subscriber, the OpenTelemetry tracer
//! provider, Prometheus metrics, and the diagnostics router.
//!
//! An OTLP exporter that cannot be built degrades to a logged reason and a
//! no-op instead of stopping startup. Ambient-credential conflicts, malformed
//! typed OTLP headers, unparsable log directives, logger installation
//! (including its writer thread), and recorder installation failures stop
//! it. Shutdown is bounded by the caller's deadline.

mod logging;
mod metrics;
mod traces;

pub use logging::{
    DropReason, LogSnapshot, LoggerGuard, LoggerShutdown, LoggerShutdownReasons, LoggingError,
    LoggingFormat, LoggingOptions, SinkOperation, install_panic_hook, install_subscriber,
};
pub use metrics::{Metrics, MetricsError, diagnostics_router, runtime_metrics};
pub use traces::{
    EndpointSource, ExporterState, ProviderShutdown, ProviderShutdownReasons, ResolvedSampler,
    SHUTDOWN_JOIN_SLACK, TracerProviderHandle, TracingError, TracingOptions,
    install_tracer_provider,
};
