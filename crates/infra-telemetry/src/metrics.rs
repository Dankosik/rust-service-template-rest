use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::Router;
use axum::http::header::CONTENT_TYPE;
use axum::routing::get;
use metrics_exporter_prometheus::{
    BuildError, LabelSet, Matcher, PrometheusBuilder, PrometheusHandle, formatting,
};
use parking_lot::Mutex;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::traces::{ExporterState, TRACE_EXPORTER_ACTIVE_METRIC};

macro_rules! static_metric_key {
    ($name:expr, $(($key:expr, $value:expr)),+ $(,)?) => {{
        static LABELS: [metrics::Label; [$($key),+].len()] = [
            $(metrics::Label::from_static_parts($key, $value)),+
        ];
        static KEY: metrics::Key = metrics::Key::from_static_parts($name, &LABELS);
        &KEY
    }};
}

pub(crate) use static_metric_key;

pub(crate) fn static_counter(
    key: &'static metrics::Key,
    metadata: &'static metrics::Metadata<'static>,
) -> metrics::Counter {
    metrics::with_recorder(|recorder| recorder.register_counter(key, metadata))
}

const DEFAULT_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

const HISTOGRAM_UPKEEP_INTERVAL: Duration = Duration::from_secs(1);

const MEMORY_LIMIT_METRIC: &str = "process_memory_limit_bytes";
const CGROUP_MEMORY_LIMIT_FILES: [&str; 2] = [
    "/sys/fs/cgroup/memory.max",
    "/sys/fs/cgroup/memory/memory.limit_in_bytes",
];
const CGROUP_V1_UNLIMITED_FLOOR: u64 = 1 << 60;

#[expect(
    clippy::disallowed_methods,
    reason = "one finite read of a control-group file at startup, before a request is served"
)]
fn cgroup_memory_limit() -> Option<u64> {
    CGROUP_MEMORY_LIMIT_FILES
        .iter()
        .find_map(|path| parse_memory_limit(&std::fs::read_to_string(path).ok()?))
}

fn parse_memory_limit(contents: &str) -> Option<u64> {
    contents
        .trim()
        .parse()
        .ok()
        .filter(|bytes| *bytes < CGROUP_V1_UNLIMITED_FLOOR)
}

fn describe_and_publish_memory_limit() {
    metrics::describe_gauge!(
        MEMORY_LIMIT_METRIC,
        metrics::Unit::Bytes,
        "Memory limit of the control group the process started in; absent when the group has no limit."
    );
    if let Some(limit) = cgroup_memory_limit() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a byte count above 2^53 loses only sub-page precision in a gauge"
        )]
        metrics::gauge!(MEMORY_LIMIT_METRIC).set(limit as f64);
    }
}

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const SCHEDULER_FRESHNESS_LIMIT: Duration = Duration::from_secs(1);
const SCHEDULER_LAG_METRIC: &str = "runtime_scheduler_lag_seconds";
const SCHEDULER_LAG_MAX_METRIC: &str = "runtime_scheduler_lag_max_seconds";
const SCHEDULER_SAMPLES_METRIC: &str = "runtime_scheduler_samples_total";
const SCHEDULER_AGE_METRIC: &str = "runtime_scheduler_sample_age_seconds";
const SCHEDULER_FRESHNESS_METRIC: &str = "runtime_scheduler_freshness_limit_seconds";
const SCHEDULER_LAG_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0,
];

#[derive(Debug, Default)]
struct Progress {
    completed: Mutex<Option<Instant>>,
}

impl Progress {
    fn describe_and_initialize() {
        metrics::describe_histogram!(
            SCHEDULER_LAG_METRIC,
            "Monotonic lateness of the runtime progress sampler."
        );
        metrics::describe_gauge!(
            SCHEDULER_LAG_MAX_METRIC,
            "Largest actual observed monotonic lateness since sampler startup; NaN before the first sample."
        );
        metrics::describe_counter!(
            SCHEDULER_SAMPLES_METRIC,
            "Completed runtime progress observations."
        );
        metrics::describe_gauge!(
            SCHEDULER_AGE_METRIC,
            "Monotonic age of the last completed observation; NaN before the first sample."
        );
        metrics::describe_gauge!(
            SCHEDULER_FRESHNESS_METRIC,
            "Observation freshness threshold in seconds; not a readiness policy."
        );
        metrics::gauge!(SCHEDULER_LAG_MAX_METRIC).set(f64::NAN);
        metrics::counter!(SCHEDULER_SAMPLES_METRIC).absolute(0);
        metrics::gauge!(SCHEDULER_AGE_METRIC).set(f64::NAN);
        metrics::gauge!(SCHEDULER_FRESHNESS_METRIC).set(SCHEDULER_FRESHNESS_LIMIT.as_secs_f64());
    }

    fn record(&self, completed: Instant) {
        *self.completed.lock() = Some(completed);
    }

    fn age_seconds(&self) -> f64 {
        let completed = *self.completed.lock();
        completed.map_or(f64::NAN, |completed| completed.elapsed().as_secs_f64())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct MetricsError(MetricsErrorKind);

#[derive(Debug, thiserror::Error)]
enum MetricsErrorKind {
    #[error("install metrics recorder: {0}")]
    Install(#[source] BuildError),
    #[error("configure histogram buckets for {metric}: {source}")]
    HistogramBuckets { metric: String, source: BuildError },
}

impl MetricsError {
    fn install(source: BuildError) -> Self {
        Self(MetricsErrorKind::Install(source))
    }

    fn histogram_buckets(metric: &str, source: BuildError) -> Self {
        Self(MetricsErrorKind::HistogramBuckets {
            metric: metric.to_owned(),
            source,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Metrics {
    handle: PrometheusHandle,
    process: metrics_process::Collector,
    progress: Arc<Progress>,
}

impl Metrics {
    pub fn install(histograms: &[(&str, &[f64])]) -> Result<Self, MetricsError> {
        let mut builder = PrometheusBuilder::new()
            .set_buckets(DEFAULT_BUCKETS)
            .map_err(MetricsError::install)?;
        for &(name, buckets) in histograms {
            builder = builder
                .set_buckets_for_metric(Matcher::Full(name.to_owned()), buckets)
                .map_err(|source| MetricsError::histogram_buckets(name, source))?;
        }
        let handle = builder
            .set_buckets_for_metric(
                Matcher::Full(SCHEDULER_LAG_METRIC.to_owned()),
                SCHEDULER_LAG_BUCKETS,
            )
            .map_err(MetricsError::install)?
            .install_recorder()
            .map_err(MetricsError::install)?;
        let process = metrics_process::Collector::default();
        process.describe();
        crate::traces::describe_metrics();
        crate::logging::describe_metrics();
        crate::logging::publish_observations();
        Progress::describe_and_initialize();
        describe_and_publish_memory_limit();
        Ok(Self {
            handle,
            process,
            progress: Arc::default(),
        })
    }

    pub fn record_trace_exporter(&self, state: &ExporterState) {
        let initialized = matches!(state, ExporterState::Initialized { .. });
        metrics::gauge!(TRACE_EXPORTER_ACTIVE_METRIC).set(f64::from(initialized));
    }

    #[must_use]
    pub fn render(&self) -> String {
        self.process.collect();
        crate::logging::publish_observations();
        metrics::gauge!(SCHEDULER_AGE_METRIC).set(self.progress.age_seconds());
        self.handle.render()
    }

    pub async fn runtime_progress(self, cancel: CancellationToken) {
        let mut expected = Instant::now() + PROGRESS_INTERVAL;
        let mut max_lag = Duration::ZERO;
        let cancelled = cancel.cancelled();
        tokio::pin!(cancelled);
        loop {
            tokio::select! {
                biased;
                () = &mut cancelled => return,
                () = tokio::time::sleep_until(expected) => {}
            }
            let observed = Instant::now();
            let lag = observed.saturating_duration_since(expected);
            metrics::histogram!(SCHEDULER_LAG_METRIC).record(lag);
            metrics::counter!(SCHEDULER_SAMPLES_METRIC).increment(1);
            let completed = Instant::now();
            self.progress.record(completed);
            max_lag = max_lag.max(lag);
            metrics::gauge!(SCHEDULER_LAG_MAX_METRIC).set(max_lag.as_secs_f64());
            expected = completed + PROGRESS_INTERVAL;
        }
    }

    pub async fn upkeep(self, cancel: CancellationToken) {
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

pub async fn runtime_metrics(interval: Duration, cancel: CancellationToken) {
    let reporter = tokio_metrics::RuntimeMetricsReporterBuilder::default()
        .with_interval(interval)
        .describe_and_run();
    let _ = cancel.run_until_cancelled(reporter).await;
}

const READY_METRIC: &str = "readiness_ready";

fn append_readiness_gauge(body: &mut String, labels: &LabelSet, ready: bool) {
    static READY_PREFIX: LazyLock<String> = LazyLock::new(|| {
        let mut prefix = String::new();
        formatting::write_help_line(
            &mut prefix,
            READY_METRIC,
            None,
            None,
            "1 if ready at scrape time, 0 otherwise.",
        );
        formatting::write_type_line(&mut prefix, READY_METRIC, None, None, "gauge");
        prefix
    });
    body.push_str(&READY_PREFIX);
    formatting::write_metric_line(
        body,
        READY_METRIC,
        None,
        labels,
        None::<(&str, &str)>,
        u8::from(ready),
        None,
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "the private Prometheus listener is outside the application OpenAPI contract"
)]
pub fn diagnostics_router(
    metrics: Metrics,
    is_ready: impl Fn() -> bool + Clone + Send + Sync + 'static,
) -> Router {
    let ready_labels =
        LabelSet::from_key_and_global(&metrics::Key::from_static_name(READY_METRIC), &[].into());
    Router::new().route(
        "/metrics",
        get(async move || {
            let mut body = metrics.render();
            append_readiness_gauge(&mut body, &ready_labels, is_ready());
            (
                [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
                body,
            )
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use futures_util::FutureExt;

    use super::*;

    #[test]
    fn invalid_histogram_buckets_report_the_metric_and_cause() {
        let error = Metrics::install(&[("request_latency_seconds", &[])])
            .expect_err("empty histogram buckets must fail");

        assert!(error.to_string().contains("request_latency_seconds"));
        let source = error
            .source()
            .expect("histogram configuration error source");
        std::assert_matches!(
            source.downcast_ref::<BuildError>(),
            Some(BuildError::EmptyBucketsOrQuantiles)
        );
    }

    #[test]
    fn a_control_group_without_a_memory_limit_publishes_none() {
        assert_eq!(parse_memory_limit("536870912\n"), Some(536_870_912));
        assert_eq!(parse_memory_limit("max\n"), None);
        assert_eq!(parse_memory_limit("9223372036854771712\n"), None);
        assert_eq!(parse_memory_limit(""), None);
    }

    fn value(exposition: &str, series: &str) -> f64 {
        exposition
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(' ')?;
                (name == series).then(|| value.parse().expect("numeric metric"))
            })
            .unwrap_or_else(|| panic!("missing metric {series}"))
    }

    #[tokio::test(start_paused = true)]
    async fn progress_scrape_preserves_unknown_startup_late_ticks_and_staleness() {
        let metrics = Metrics::install(&[]).expect("install recorder");
        let scraper = metrics.clone();
        let cancel = CancellationToken::new();
        let sampler = metrics.runtime_progress(cancel.clone());
        tokio::pin!(sampler);
        assert!(sampler.as_mut().now_or_never().is_none());
        let startup = scraper.render();
        assert_eq!(value(&startup, SCHEDULER_SAMPLES_METRIC), 0.0);
        assert!(value(&startup, SCHEDULER_AGE_METRIC).is_nan());
        assert!(value(&startup, SCHEDULER_LAG_MAX_METRIC).is_nan());
        assert_eq!(value(&startup, SCHEDULER_FRESHNESS_METRIC), 1.0);
        assert!(
            !startup
                .lines()
                .any(|line| line.starts_with("runtime_scheduler_lag_seconds_count "))
        );

        tokio::time::advance(PROGRESS_INTERVAL).await;
        assert!(sampler.as_mut().now_or_never().is_none());
        let first = scraper.render();
        assert_eq!(value(&first, SCHEDULER_SAMPLES_METRIC), 1.0);
        assert_eq!(value(&first, "runtime_scheduler_lag_seconds_sum"), 0.0);
        assert_eq!(value(&first, SCHEDULER_AGE_METRIC), 0.0);
        assert_eq!(value(&first, SCHEDULER_LAG_MAX_METRIC), 0.0);

        tokio::time::advance(Duration::from_millis(2100)).await;
        assert_eq!(value(&scraper.render(), SCHEDULER_AGE_METRIC), 2.1);
        assert!(sampler.as_mut().now_or_never().is_none());
        let resumed = scraper.render();
        assert_eq!(value(&resumed, SCHEDULER_SAMPLES_METRIC), 2.0);
        assert_eq!(value(&resumed, SCHEDULER_LAG_MAX_METRIC), 2.0);
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
        assert_eq!(value(&scraper.render(), SCHEDULER_SAMPLES_METRIC), 2.0);

        tokio::time::advance(PROGRESS_INTERVAL).await;
        assert!(sampler.as_mut().now_or_never().is_none());
        let recovered = scraper.render();
        assert_eq!(value(&recovered, SCHEDULER_SAMPLES_METRIC), 3.0);
        assert_eq!(value(&recovered, SCHEDULER_LAG_MAX_METRIC), 2.0);
        assert_eq!(value(&recovered, "runtime_scheduler_lag_seconds_sum"), 2.0);

        tokio::time::advance(Duration::from_millis(1500)).await;
        assert_eq!(value(&scraper.render(), SCHEDULER_AGE_METRIC), 1.5);
        cancel.cancel();
        assert!(sampler.as_mut().now_or_never().is_some());
        assert_eq!(value(&scraper.render(), SCHEDULER_SAMPLES_METRIC), 3.0);
        tokio::time::advance(Duration::from_secs(1)).await;
        let stopped = scraper.render();
        assert_eq!(value(&stopped, SCHEDULER_AGE_METRIC), 2.5);
        assert_eq!(value(&stopped, SCHEDULER_LAG_MAX_METRIC), 2.0);
    }
}
