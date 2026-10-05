//! Prometheus metrics through the `metrics` facade.
//!
//! One recorder per process. HTTP server metrics come from the HTTP adapter,
//! process metrics from `metrics-process`, runtime metrics from
//! `tokio-metrics`; this module owns the recorder, the periodic upkeep, and
//! the scrape route. Each crate that emits a histogram
//! owns its name and buckets; a histogram nobody registered gets
//! [`DEFAULT_BUCKETS`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use metrics_exporter_prometheus::{BuildError, Matcher, PrometheusBuilder, PrometheusHandle};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Gauge: 1 when the OTLP trace exporter was configured and initialized at
/// startup, 0 otherwise. A startup-configuration signal, not delivery health.
pub const TRACE_EXPORTER_ACTIVE_METRIC: &str = "service_startup_trace_exporter_active";

/// Counter: spans in batches whose SDK exporter returned success or failure.
/// `error_type` is finite. Success does not prove receiver acceptance or delivery;
/// SDK queue loss and reported rejection have separate observed metrics.
pub const TRACE_SPANS_EXPORTED_METRIC: &str = "otel_sdk_exporter_span_exported_total";

/// Buckets in seconds for a histogram no emitter registered, the Prometheus
/// client default. Without buckets the exporter renders a summary, whose
/// quantiles cannot be aggregated across replicas.
pub const DEFAULT_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// The recorder keeps every histogram sample, 24 bytes with its timestamp,
/// until upkeep folds it into the buckets. Each second bounds that to one
/// second of traffic: at 26k requests per second it cut the service's peak
/// resident memory from 16.8 to 13 MB with unchanged CPU and throughput.
const HISTOGRAM_UPKEEP_INTERVAL: Duration = Duration::from_secs(1);

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const SCHEDULER_LAG: &str = "runtime_scheduler_lag_seconds";
const SCHEDULER_LAG_MAX: &str = "runtime_scheduler_lag_max_seconds";
const SCHEDULER_SAMPLES: &str = "runtime_scheduler_samples_total";
const SCHEDULER_AGE: &str = "runtime_scheduler_sample_age_seconds";
const SCHEDULER_FRESHNESS: &str = "runtime_scheduler_freshness_limit_seconds";
const SCHEDULER_LAG_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0,
];

/// Shared with the scrape owner; no callbacks or await while holding the lock.
#[derive(Debug, Default)]
struct Progress {
    sample: Mutex<Option<ProgressSample>>,
}

#[derive(Clone, Copy, Debug)]
struct ProgressSample {
    completed: Instant,
    max_lag: Duration,
}

impl Progress {
    fn describe() {
        metrics::describe_histogram!(
            SCHEDULER_LAG,
            "Monotonic lateness of the runtime progress sampler."
        );
        metrics::describe_gauge!(
            SCHEDULER_LAG_MAX,
            "Largest actual observed monotonic lateness since sampler startup; NaN before the first sample."
        );
        metrics::describe_counter!(
            SCHEDULER_SAMPLES,
            "Completed runtime progress observations."
        );
        metrics::describe_gauge!(
            SCHEDULER_AGE,
            "Monotonic age of the last completed observation; NaN before the first sample."
        );
        metrics::describe_gauge!(
            SCHEDULER_FRESHNESS,
            "Observation freshness threshold in seconds; not a readiness policy."
        );
        metrics::gauge!(SCHEDULER_LAG_MAX).set(f64::NAN);
        metrics::counter!(SCHEDULER_SAMPLES).absolute(0);
        metrics::gauge!(SCHEDULER_AGE).set(f64::NAN);
        metrics::gauge!(SCHEDULER_FRESHNESS).set(1.0);
    }

    fn age(&self) -> f64 {
        let sample = *self
            .sample
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sample.map_or(f64::NAN, |sample| sample.completed.elapsed().as_secs_f64())
    }
}

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
    progress: Arc<Progress>,
}

impl Metrics {
    /// Install the process-global recorder with explicit buckets for each
    /// `(metric, buckets)` histogram and describe the process metrics. The
    /// crate that emits a histogram owns its name and buckets; without an
    /// entry here that histogram gets [`DEFAULT_BUCKETS`].
    ///
    /// # Errors
    ///
    /// Returns [`MetricsError::Install`] when a recorder is already
    /// installed or the buckets are invalid.
    pub fn install(histograms: &[(&str, &[f64])]) -> Result<Self, MetricsError> {
        let mut builder = PrometheusBuilder::new()
            .set_buckets(DEFAULT_BUCKETS)
            .map_err(MetricsError::Install)?;
        for &(name, buckets) in histograms {
            builder = builder
                .set_buckets_for_metric(Matcher::Full(name.to_owned()), buckets)
                .map_err(MetricsError::Install)?;
        }
        let builder = builder
            .set_buckets_for_metric(
                Matcher::Full(SCHEDULER_LAG.to_owned()),
                SCHEDULER_LAG_BUCKETS,
            )
            .map_err(MetricsError::Install)?;
        let handle = builder.install_recorder().map_err(MetricsError::Install)?;
        let process = metrics_process::Collector::default();
        process.describe();
        metrics::describe_gauge!(
            TRACE_EXPORTER_ACTIVE_METRIC,
            "1 when the OTLP trace exporter is configured and initialized, 0 otherwise."
        );
        metrics::describe_counter!(
            TRACE_SPANS_EXPORTED_METRIC,
            "Spans in batches whose SDK exporter returned success or failure; error_type marks failure, not receiver acceptance or persistence."
        );
        crate::logging::publish_observations();
        Progress::describe();
        Ok(Self {
            handle,
            process,
            progress: Arc::default(),
        })
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
        crate::logging::publish_observations();
        metrics::gauge!(SCHEDULER_AGE).set(self.progress.age());
        self.handle.render()
    }

    /// Observe scheduler progress every 100 ms after the previous completion.
    /// Run once under the process background tracker with its child token.
    /// The first sample waits a full interval; resumed lateness is retained in
    /// the histogram and cumulative maximum, with no reset or catch-up samples.
    /// Scrapes compute age
    /// independently, so an observer that stops making progress becomes stale.
    pub async fn runtime_progress(self, cancel: CancellationToken) {
        let mut expected = Instant::now() + PROGRESS_INTERVAL;
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                () = tokio::time::sleep_until(expected) => {}
            }
            let observed = Instant::now();
            let lag = observed.saturating_duration_since(expected);
            metrics::histogram!(SCHEDULER_LAG).record(lag.as_secs_f64());
            metrics::counter!(SCHEDULER_SAMPLES).increment(1);
            let completed = Instant::now();
            let max_lag = {
                let mut sample = self
                    .progress
                    .sample
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let max_lag = sample.map_or(lag, |previous| previous.max_lag.max(lag));
                *sample = Some(ProgressSample { completed, max_lag });
                max_lag
            };
            metrics::gauge!(SCHEDULER_LAG_MAX).set(max_lag.as_secs_f64());
            expected = completed + PROGRESS_INTERVAL;
        }
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
                    crate::logging::publish_observations();
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

#[cfg(test)]
mod tests {
    use futures_util::FutureExt;

    use super::*;

    fn value(exposition: &str, series: &str) -> f64 {
        exposition
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(' ')?;
                (name == series).then(|| value.parse().expect("numeric metric"))
            })
            .unwrap_or_else(|| panic!("missing metric {series}"))
    }

    // This owns the scheduler observation contract: a sampler-only age update,
    // an immediate startup tick, catch-up ticks or cancellation losing to a
    // ready timer all fail here. Existing runtime reporter tests cannot cover
    // the separate monotonic snapshot read by Metrics clones.
    #[allow(
        clippy::float_cmp,
        reason = "paused-clock durations and integral counts have exact expected values"
    )]
    #[tokio::test(start_paused = true)]
    async fn progress_scrape_preserves_unknown_startup_late_ticks_and_staleness() {
        let metrics = Metrics::install(&[]).expect("install recorder");
        let scraper = metrics.clone();
        let cancel = CancellationToken::new();
        let sampler = metrics.runtime_progress(cancel.clone());
        tokio::pin!(sampler);
        assert!(sampler.as_mut().now_or_never().is_none());
        let startup = scraper.render();
        assert_eq!(value(&startup, SCHEDULER_SAMPLES), 0.0);
        assert!(value(&startup, SCHEDULER_AGE).is_nan());
        assert!(value(&startup, SCHEDULER_LAG_MAX).is_nan());
        assert_eq!(value(&startup, SCHEDULER_FRESHNESS), 1.0);
        assert!(
            !startup
                .lines()
                .any(|line| line.starts_with("runtime_scheduler_lag_seconds_count "))
        );

        tokio::time::advance(PROGRESS_INTERVAL).await;
        assert!(sampler.as_mut().now_or_never().is_none());
        let first = scraper.render();
        assert_eq!(value(&first, SCHEDULER_SAMPLES), 1.0);
        assert_eq!(value(&first, "runtime_scheduler_lag_seconds_sum"), 0.0);
        assert_eq!(value(&first, SCHEDULER_AGE), 0.0);
        assert_eq!(value(&first, SCHEDULER_LAG_MAX), 0.0);

        // Keep the sampler unpolled while the scrape path still makes progress.
        tokio::time::advance(Duration::from_millis(2100)).await;
        assert_eq!(value(&scraper.render(), SCHEDULER_AGE), 2.1);
        assert!(sampler.as_mut().now_or_never().is_none());
        let resumed = scraper.render();
        assert_eq!(value(&resumed, SCHEDULER_SAMPLES), 2.0);
        assert_eq!(value(&resumed, SCHEDULER_LAG_MAX), 2.0);
        assert_eq!(value(&resumed, "runtime_scheduler_lag_seconds_sum"), 2.0);
        assert_eq!(
            value(&resumed, "runtime_scheduler_lag_seconds_bucket{le=\"1\"}"),
            1.0
        );
        assert_eq!(
            value(&resumed, "runtime_scheduler_lag_seconds_bucket{le=\"2\"}"),
            2.0
        );
        assert!(sampler.as_mut().now_or_never().is_none());
        assert_eq!(value(&scraper.render(), SCHEDULER_SAMPLES), 2.0);

        tokio::time::advance(PROGRESS_INTERVAL).await;
        assert!(sampler.as_mut().now_or_never().is_none());
        let recovered = scraper.render();
        assert_eq!(value(&recovered, SCHEDULER_SAMPLES), 3.0);
        assert_eq!(value(&recovered, SCHEDULER_LAG_MAX), 2.0);
        assert_eq!(value(&recovered, "runtime_scheduler_lag_seconds_sum"), 2.0);

        tokio::time::advance(Duration::from_millis(1500)).await;
        assert_eq!(value(&scraper.render(), SCHEDULER_AGE), 1.5);
        cancel.cancel();
        assert!(sampler.as_mut().now_or_never().is_some());
        assert_eq!(value(&scraper.render(), SCHEDULER_SAMPLES), 3.0);
        tokio::time::advance(Duration::from_secs(1)).await;
        let stopped = scraper.render();
        assert_eq!(value(&stopped, SCHEDULER_AGE), 2.5);
        assert_eq!(value(&stopped, SCHEDULER_LAG_MAX), 2.0);
    }
}
