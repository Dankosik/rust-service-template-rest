use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use temporalio_common::telemetry::metrics::{
    CoreMeter, Counter, CounterBase, Gauge, GaugeBase, GaugeF64, GaugeF64Base, Histogram,
    HistogramBase, HistogramDuration, HistogramDurationBase, HistogramF64, HistogramF64Base,
    MetricAttributable, MetricAttributes, MetricParameters, NewAttributes, OrderedPromLabelSet,
    TemporalMeter, UpDownCounter, UpDownCounterBase,
};
use temporalio_common::telemetry::{TaskQueueLabelStrategy, TelemetryOptions};

pub const METRIC_PREFIX: &str = "temporal_";

const CREDENTIAL_RELOADS_METRIC: &str = "temporal_credential_reloads_total";
const DRAIN_FORCED_METRIC: &str = "temporal_drain_forced_total";

pub fn describe_metrics() {
    metrics::describe_counter!(
        CREDENTIAL_RELOADS_METRIC,
        metrics::Unit::Count,
        "Completed live Temporal credential reload attempts by credential kind and outcome."
    );
    metrics::describe_counter!(
        DRAIN_FORCED_METRIC,
        metrics::Unit::Count,
        "Forced Temporal worker shutdown drains that preempted active activities."
    );
}

pub const HISTOGRAMS: &[(&str, &[f64])] = &[
    (
        "temporal_request_latency",
        &[
            0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
        ],
    ),
    (
        "temporal_long_request_latency",
        &[0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 70.0],
    ),
    (
        "temporal_workflow_endtoend_latency",
        &[
            0.1, 0.5, 1.0, 1.5, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
            30600.0, 86400.0,
        ],
    ),
    (
        "temporal_workflow_task_schedule_to_start_latency",
        &[0.1, 0.5, 1.0, 5.0, 10.0, 100.0, 1000.0],
    ),
    (
        "temporal_workflow_task_replay_latency",
        &[0.001, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0],
    ),
    (
        "temporal_workflow_task_execution_latency",
        &[0.001, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0],
    ),
    (
        "temporal_activity_schedule_to_start_latency",
        &[0.1, 0.5, 1.0, 5.0, 10.0, 100.0, 1000.0],
    ),
    (
        "temporal_activity_execution_latency",
        &[0.05, 0.1, 0.5, 1.0, 5.0, 10.0, 60.0],
    ),
];

#[derive(Clone, Debug)]
pub struct WorkspaceMetricsMeter {
    prefix: &'static str,
}

impl WorkspaceMetricsMeter {
    #[must_use]
    pub const fn new(prefix: &'static str) -> Self {
        Self { prefix }
    }

    #[must_use]
    pub fn core_meter(&self) -> Arc<dyn CoreMeter> {
        Arc::new(self.clone())
    }

    #[must_use]
    pub fn temporal_meter(&self) -> TemporalMeter {
        TemporalMeter::new(
            self.core_meter(),
            NewAttributes::default(),
            TaskQueueLabelStrategy::UseNormal,
        )
    }

    #[must_use]
    pub fn telemetry_options(&self) -> TelemetryOptions {
        TelemetryOptions::builder()
            .metrics(self.core_meter())
            .attach_service_name(false)
            .metric_prefix(String::new())
            .task_queue_label_strategy(TaskQueueLabelStrategy::UseNormal)
            .build()
    }

    #[allow(
        dead_code,
        reason = "crate-internal worker drain counter consumed by the worker module"
    )]
    pub(crate) fn record_drain_forced() {
        metrics::counter!(DRAIN_FORCED_METRIC).increment(1);
    }

    pub(crate) fn record_credential_reload(kind: &'static str, outcome: &'static str) {
        metrics::counter!(CREDENTIAL_RELOADS_METRIC, "kind" => kind, "outcome" => outcome)
            .increment(1);
    }

    fn instrument_name(&self, raw: &str) -> Arc<str> {
        let normalized = raw.replace('-', "_");
        if normalized.starts_with(self.prefix) {
            Arc::from(normalized)
        } else {
            Arc::from(format!("{}{normalized}", self.prefix))
        }
    }
}

impl Default for WorkspaceMetricsMeter {
    fn default() -> Self {
        Self::new(METRIC_PREFIX)
    }
}

fn extract_labels(attributes: &MetricAttributes) -> Vec<metrics::Label> {
    let MetricAttributes::Prometheus { labels } = attributes else {
        return Vec::new();
    };
    let values = labels.as_prom_labels();
    labels
        .keys_ordered()
        .filter_map(|key| {
            values
                .get(key)
                .map(|value| metrics::Label::new(key.to_owned(), value.clone()))
        })
        .collect()
}

impl CoreMeter for WorkspaceMetricsMeter {
    fn new_attributes(&self, attribs: NewAttributes) -> MetricAttributes {
        MetricAttributes::Prometheus {
            labels: Arc::new(OrderedPromLabelSet::from(attribs)),
        }
    }

    fn extend_attributes(
        &self,
        existing: MetricAttributes,
        attribs: NewAttributes,
    ) -> MetricAttributes {
        let mut labels = match existing {
            MetricAttributes::Prometheus { labels } => (*labels).clone(),
            _ => OrderedPromLabelSet::new(),
        };
        for kv in attribs.attributes {
            labels.add_kv(kv);
        }
        MetricAttributes::Prometheus {
            labels: Arc::new(labels),
        }
    }

    fn counter(&self, params: MetricParameters) -> Counter {
        Counter::new(Arc::new(CounterMetric {
            name: self.instrument_name(&params.name),
        }))
    }

    fn histogram(&self, params: MetricParameters) -> Histogram {
        Histogram::new(Arc::new(HistogramMetric {
            name: self.instrument_name(&params.name),
        }))
    }

    fn histogram_f64(&self, params: MetricParameters) -> HistogramF64 {
        HistogramF64::new(Arc::new(HistogramMetric {
            name: self.instrument_name(&params.name),
        }))
    }

    fn histogram_duration(&self, params: MetricParameters) -> HistogramDuration {
        HistogramDuration::new(Arc::new(HistogramMetric {
            name: self.instrument_name(&params.name),
        }))
    }

    fn gauge(&self, params: MetricParameters) -> Gauge {
        Gauge::new(Arc::new(GaugeMetric {
            name: self.instrument_name(&params.name),
        }))
    }

    fn gauge_f64(&self, params: MetricParameters) -> GaugeF64 {
        GaugeF64::new(Arc::new(GaugeMetric {
            name: self.instrument_name(&params.name),
        }))
    }

    fn up_down_counter(&self, params: MetricParameters) -> UpDownCounter {
        UpDownCounter::new(Arc::new(GaugeMetric {
            name: self.instrument_name(&params.name),
        }))
    }
}

struct CounterMetric {
    name: Arc<str>,
}

struct BoundCounter(metrics::Counter);

impl MetricAttributable<Box<dyn CounterBase>> for CounterMetric {
    fn with_attributes(
        &self,
        attributes: &MetricAttributes,
    ) -> Result<Box<dyn CounterBase>, Box<dyn Error>> {
        let handle = metrics::counter!(self.name.to_string(), extract_labels(attributes));
        Ok(Box::new(BoundCounter(handle)))
    }
}

impl CounterBase for BoundCounter {
    fn adds(&self, value: u64) {
        self.0.increment(value);
    }
}

struct HistogramMetric {
    name: Arc<str>,
}

struct BoundHistogram(metrics::Histogram);

impl MetricAttributable<Box<dyn HistogramBase>> for HistogramMetric {
    fn with_attributes(
        &self,
        attributes: &MetricAttributes,
    ) -> Result<Box<dyn HistogramBase>, Box<dyn Error>> {
        let handle = metrics::histogram!(self.name.to_string(), extract_labels(attributes));
        Ok(Box::new(BoundHistogram(handle)))
    }
}

impl HistogramBase for BoundHistogram {
    fn records(&self, value: u64) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "metrics::Histogram records observations as f64"
        )]
        self.0.record(value as f64);
    }
}

impl MetricAttributable<Box<dyn HistogramF64Base>> for HistogramMetric {
    fn with_attributes(
        &self,
        attributes: &MetricAttributes,
    ) -> Result<Box<dyn HistogramF64Base>, Box<dyn Error>> {
        let handle = metrics::histogram!(self.name.to_string(), extract_labels(attributes));
        Ok(Box::new(BoundHistogram(handle)))
    }
}

impl HistogramF64Base for BoundHistogram {
    fn records(&self, value: f64) {
        self.0.record(value);
    }
}

impl MetricAttributable<Box<dyn HistogramDurationBase>> for HistogramMetric {
    fn with_attributes(
        &self,
        attributes: &MetricAttributes,
    ) -> Result<Box<dyn HistogramDurationBase>, Box<dyn Error>> {
        let handle = metrics::histogram!(self.name.to_string(), extract_labels(attributes));
        Ok(Box::new(BoundHistogram(handle)))
    }
}

impl HistogramDurationBase for BoundHistogram {
    fn records(&self, value: Duration) {
        self.0.record(value.as_secs_f64());
    }
}

struct GaugeMetric {
    name: Arc<str>,
}

struct BoundGauge(metrics::Gauge);

impl MetricAttributable<Box<dyn GaugeBase>> for GaugeMetric {
    fn with_attributes(
        &self,
        attributes: &MetricAttributes,
    ) -> Result<Box<dyn GaugeBase>, Box<dyn Error>> {
        let handle = metrics::gauge!(self.name.to_string(), extract_labels(attributes));
        Ok(Box::new(BoundGauge(handle)))
    }
}

impl GaugeBase for BoundGauge {
    fn records(&self, value: u64) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "metrics::Gauge stores values as f64"
        )]
        self.0.set(value as f64);
    }
}

impl MetricAttributable<Box<dyn GaugeF64Base>> for GaugeMetric {
    fn with_attributes(
        &self,
        attributes: &MetricAttributes,
    ) -> Result<Box<dyn GaugeF64Base>, Box<dyn Error>> {
        let handle = metrics::gauge!(self.name.to_string(), extract_labels(attributes));
        Ok(Box::new(BoundGauge(handle)))
    }
}

impl GaugeF64Base for BoundGauge {
    fn records(&self, value: f64) {
        self.0.set(value);
    }
}

impl MetricAttributable<Box<dyn UpDownCounterBase>> for GaugeMetric {
    fn with_attributes(
        &self,
        attributes: &MetricAttributes,
    ) -> Result<Box<dyn UpDownCounterBase>, Box<dyn Error>> {
        let handle = metrics::gauge!(self.name.to_string(), extract_labels(attributes));
        Ok(Box::new(BoundGauge(handle)))
    }
}

impl UpDownCounterBase for BoundGauge {
    fn adds(&self, value: i64) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "metrics::Gauge delta operations take f64"
        )]
        let magnitude = value.unsigned_abs() as f64;
        if value >= 0 {
            self.0.increment(magnitude);
        } else {
            self.0.decrement(magnitude);
        }
    }
}
