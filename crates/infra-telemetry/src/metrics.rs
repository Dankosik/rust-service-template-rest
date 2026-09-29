//! Prometheus metrics through the `metrics` facade.
//!
//! One recorder per process. HTTP server metrics come from the HTTP adapter,
//! process metrics from `metrics-process`, runtime metrics from
//! `tokio-metrics`; this module owns the recorder, the periodic upkeep, and
//! the scrape route. Each crate that emits a histogram
//! owns its name and buckets.

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

/// The recorder keeps every histogram sample, 24 bytes with its timestamp,
/// until upkeep folds it into the buckets. Each second bounds that to one
/// second of traffic: at 26k requests per second it cut the service's peak
/// resident memory from 16.8 to 13 MB with unchanged CPU and throughput.
const HISTOGRAM_UPKEEP_INTERVAL: Duration = Duration::from_secs(1);

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
    /// Install the process-global recorder with explicit buckets for each
    /// `(metric, buckets)` histogram and describe the process metrics. The
    /// crate that emits a histogram owns its name and buckets; without an
    /// entry here the exporter renders that histogram as a summary.
    ///
    /// # Errors
    ///
    /// Returns [`MetricsError::Install`] when a recorder is already
    /// installed or the buckets are invalid.
    pub fn install(histograms: &[(&str, &[f64])]) -> Result<Self, MetricsError> {
        let mut builder = PrometheusBuilder::new();
        for &(name, buckets) in histograms {
            builder = builder
                .set_buckets_for_metric(Matcher::Full(name.to_owned()), buckets)
                .map_err(MetricsError::Install)?;
        }
        let handle = builder.install_recorder().map_err(MetricsError::Install)?;
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

    /// Drain histogram samples every [`HISTOGRAM_UPKEEP_INTERVAL`] so the
    /// recorder does not grow between scrapes. Run under the background task
    /// tracker.
    pub async fn upkeep(self, cancel: CancellationToken) {
        // An already cancelled token never polls the work; no detached
        // task is created.
        let _ = cancel
            .run_until_cancelled(async {
                let mut ticker = tokio::time::interval(HISTOGRAM_UPKEEP_INTERVAL);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    self.handle.run_upkeep();
                }
            })
            .await;
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

/// The diagnostics router: `GET /metrics` only. Serve it on the private
/// diagnostics listener, never on the application listener.
///
/// That listener is intentionally outside the application `harden` chain:
/// Prometheus text, not Problem JSON; no scrape-on-scrape HTTP metrics,
/// spans, or access logs; the address is trusted-private. `http.*`
/// `ServerOptions` still apply as shared transport policy (header timeout
/// and size, connection cap, drain), not request-level policy.
#[allow(
    clippy::disallowed_methods,
    reason = "the private Prometheus listener is outside the application OpenAPI contract"
)]
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
