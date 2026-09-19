//! Prometheus metrics through the `metrics` facade.
//!
//! One recorder per process. HTTP server metrics come from `axum-prometheus`
//! in the HTTP adapter, process metrics from `metrics-process`, runtime
//! metrics from `tokio-metrics`; this module owns the recorder, the
//! histogram buckets, the periodic upkeep, and the scrape route.

use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use metrics_exporter_prometheus::{BuildError, Matcher, PrometheusBuilder, PrometheusHandle};
use tokio_util::sync::CancellationToken;

/// Gauge: 1 when the OTLP trace exporter was configured and initialized at
/// startup, 0 otherwise. A startup-configuration signal, not delivery health.
pub const TRACE_EXPORTER_ACTIVE_METRIC: &str = "service_startup_trace_exporter_active";

/// Request-duration buckets in seconds, shaped for an HTTP API.
const HTTP_DURATION_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("install metrics recorder: {0}")]
    Install(#[source] BuildError),
}

/// The installed recorder and its collectors.
#[derive(Clone, Debug)]
pub struct Metrics {
    handle: PrometheusHandle,
    process: metrics_process::Collector,
}

impl Metrics {
    /// Install the process-global recorder with the HTTP duration buckets
    /// and describe the process metrics.
    ///
    /// # Errors
    ///
    /// Returns [`MetricsError::Install`] when a recorder is already
    /// installed or the buckets are invalid.
    pub fn install(http_duration_metric: &str) -> Result<Self, MetricsError> {
        let handle = PrometheusBuilder::new()
            .set_buckets_for_metric(
                Matcher::Full(http_duration_metric.to_owned()),
                HTTP_DURATION_BUCKETS,
            )
            .map_err(MetricsError::Install)?
            .install_recorder()
            .map_err(MetricsError::Install)?;
        let process = metrics_process::Collector::default();
        process.describe();
        metrics::describe_gauge!(
            TRACE_EXPORTER_ACTIVE_METRIC,
            "1 when the OTLP trace exporter is configured and initialized, 0 otherwise."
        );
        Ok(Self { handle, process })
    }

    /// Record the trace exporter startup state.
    pub fn record_trace_exporter_initialized(&self, initialized: bool) {
        metrics::gauge!(TRACE_EXPORTER_ACTIVE_METRIC).set(if initialized { 1.0 } else { 0.0 });
    }

    /// Render the Prometheus text exposition, refreshing process metrics
    /// first so a scrape sees current values.
    #[must_use]
    pub fn render(&self) -> String {
        self.process.collect();
        self.handle.render()
    }

    /// Drain histogram samples periodically so the recorder does not grow
    /// unboundedly between scrapes. Run under the background task tracker.
    pub async fn upkeep(self, interval: Duration, cancel: CancellationToken) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                _ = ticker.tick() => self.handle.run_upkeep(),
            }
        }
    }

    /// Publish Tokio runtime metrics on `interval` until cancelled. Uses the
    /// stable subset; `--cfg tokio_unstable` adds poll and queue detail.
    pub async fn runtime_metrics(interval: Duration, cancel: CancellationToken) {
        let reporter = tokio_metrics::RuntimeMetricsReporterBuilder::default()
            .with_interval(interval)
            .describe_and_run();
        let _ = cancel.run_until_cancelled(reporter).await;
    }
}

/// The diagnostics router: `GET /metrics` only. Serve it on the private
/// diagnostics listener, never on the application listener.
pub fn diagnostics_router(metrics: Metrics) -> Router {
    Router::new()
        .route("/metrics", get(render))
        .with_state(metrics)
}

async fn render(State(metrics): State<Metrics>) -> Response {
    (
        [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        metrics.render(),
    )
        .into_response()
}
