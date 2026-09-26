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

// template:begin outbound-http:telemetry-outbound-buckets-constants
/// Explicit seconds buckets for bounded outbound exchanges.
const OUTBOUND_HTTP_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];
const OUTBOUND_HTTP_DURATION_METRIC: &str = "http_client_request_duration_seconds";
// template:end outbound-http:telemetry-outbound-buckets-constants

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
        let builder = PrometheusBuilder::new()
            .set_buckets_for_metric(
                Matcher::Full(http_duration_metric.to_owned()),
                HTTP_DURATION_BUCKETS,
            )
            .map_err(MetricsError::Install)?;
        Self::installed(builder)
    }

    /// Install `builder` and describe the process and trace-exporter metrics.
    fn installed(builder: PrometheusBuilder) -> Result<Self, MetricsError> {
        // template:begin outbound-http:telemetry-outbound-buckets-install
        let builder = outbound_histogram_builder(builder).map_err(MetricsError::Install)?;
        // template:end outbound-http:telemetry-outbound-buckets-install
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

    /// Drain histogram samples periodically so the recorder does not grow
    /// unboundedly between scrapes. Run under the background task tracker.
    pub async fn upkeep(self, interval: Duration, cancel: CancellationToken) {
        // An already cancelled token never polls the work; no detached
        // task is created.
        let _ = cancel
            .run_until_cancelled(async {
                let mut ticker = tokio::time::interval(interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    self.handle.run_upkeep();
                }
            })
            .await;
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

// template:begin jobs:telemetry-jobs-histograms
impl Metrics {
    /// `install` plus explicit buckets for further histograms.
    ///
    /// # Errors
    ///
    /// Returns [`MetricsError::Install`] when a recorder is already
    /// installed or the buckets are invalid.
    pub fn install_with_histograms(
        http_duration_metric: &str,
        histograms: &[(&str, &[f64])],
    ) -> Result<Self, MetricsError> {
        let mut builder = PrometheusBuilder::new()
            .set_buckets_for_metric(
                Matcher::Full(http_duration_metric.to_owned()),
                HTTP_DURATION_BUCKETS,
            )
            .map_err(MetricsError::Install)?;
        for &(name, buckets) in histograms {
            builder = builder
                .set_buckets_for_metric(Matcher::Full(name.to_owned()), buckets)
                .map_err(MetricsError::Install)?;
        }
        Self::installed(builder)
    }
}
// template:end jobs:telemetry-jobs-histograms

// template:begin outbound-http:telemetry-outbound-buckets-helper
fn outbound_histogram_builder(builder: PrometheusBuilder) -> Result<PrometheusBuilder, BuildError> {
    builder.set_buckets_for_metric(
        Matcher::Full(OUTBOUND_HTTP_DURATION_METRIC.to_owned()),
        OUTBOUND_HTTP_DURATION_BUCKETS,
    )
}
// template:end outbound-http:telemetry-outbound-buckets-helper

/// The diagnostics router: `GET /metrics` only. Serve it on the private
/// diagnostics listener, never on the application listener.
///
/// That listener is intentionally outside the application `harden` chain:
/// Prometheus text, not Problem JSON; no scrape-on-scrape HTTP metrics,
/// spans, or access logs; the address is trusted-private. `http.*`
/// `ServerOptions` still apply as shared transport policy (header timeout
/// and size, connection cap, drain), not request-level policy.
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

// template:begin outbound-http:telemetry-outbound-histogram-test
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbound_duration_uses_the_selected_prometheus_buckets() {
        let recorder = outbound_histogram_builder(PrometheusBuilder::new())
            .expect("outbound histogram buckets are valid")
            .build_recorder();
        let _local = metrics::set_default_local_recorder(&recorder);
        metrics::describe_histogram!(
            OUTBOUND_HTTP_DURATION_METRIC,
            metrics::Unit::Seconds,
            "Outbound HTTP attempt duration in seconds"
        );
        metrics::histogram!(OUTBOUND_HTTP_DURATION_METRIC, "server.address" => "provider.test")
            .record(0.075);
        let scrape = recorder.handle().render();
        assert!(scrape.contains("http_client_request_duration_seconds_bucket"));
        assert!(scrape.contains("le=\"0.075\""));
    }
}
// template:end outbound-http:telemetry-outbound-histogram-test
