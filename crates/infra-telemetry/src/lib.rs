//! Telemetry adapters: the tracing subscriber, the OpenTelemetry tracer
//! provider, Prometheus metrics, and the diagnostics router.
//!
//! Setup never blocks the service: every builder failure degrades to a logged
//! reason and a no-op. Shutdown is bounded by the caller's budget. The crate
//! decisions are recorded in `specs/runtime-core/research/synthesis.md`.

pub mod logging;
pub mod metrics;
pub mod tracing;

pub use logging::{LogFormat, LoggingError, LoggingOptions, install_subscriber};
pub use metrics::{Metrics, MetricsError, TRACE_EXPORTER_ACTIVE_METRIC, diagnostics_router};
pub use tracing::{
    ExporterState, Sampler, TracerProviderHandle, TracingError, TracingOptions,
    build_tracer_provider,
};
