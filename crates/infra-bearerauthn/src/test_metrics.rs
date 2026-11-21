use std::{
    fmt::Write as _,
    sync::{Arc, Mutex},
};

use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use observe_test_support::{
    EventCapture, FieldMap, MetricsCapture, SpanCapture, dispatch_spans_and_events,
};

pub(crate) use observe_test_support::labeled;

#[derive(Default)]
struct BufferedMetrics {
    registered_counters: Vec<metrics::Key>,
    counters: Vec<(metrics::Key, u64)>,
    histograms: Vec<metrics::Key>,
    acquisitions: Vec<f64>,
}

fn render_fields(prefix: &str, fields: &FieldMap) -> String {
    let mut rendered = prefix.to_owned();
    for (name, value) in fields {
        write!(rendered, "{name}={value};").unwrap();
    }
    rendered
}

#[derive(Clone, Default)]
pub(crate) struct Diagnostics {
    metrics: MetricsCapture,
    buffered: Arc<Mutex<BufferedMetrics>>,
    events: EventCapture,
    spans: SpanCapture,
}

impl Diagnostics {
    pub(crate) fn recorder(&self) -> &DebuggingRecorder {
        self.metrics.recorder()
    }

    pub(crate) fn dispatch(&self) -> tracing::Dispatch {
        dispatch_spans_and_events(&self.spans, &self.events)
    }

    fn flush_metrics(&self) -> std::sync::MutexGuard<'_, BufferedMetrics> {
        let snapshot = self.metrics.snapshot_vec();
        let mut buffered = self.buffered.lock().unwrap();
        for (key, _, _, value) in snapshot {
            match value {
                DebugValue::Counter(count) => {
                    if !buffered
                        .registered_counters
                        .iter()
                        .any(|known| known == key.key())
                    {
                        buffered.registered_counters.push(key.key().clone());
                    }
                    if count > 0 {
                        if let Some((_, total)) = buffered
                            .counters
                            .iter_mut()
                            .find(|(known, _)| known == key.key())
                        {
                            *total += count;
                        } else {
                            buffered.counters.push((key.key().clone(), count));
                        }
                    }
                }
                DebugValue::Gauge(sample) => {
                    assert_eq!(
                        key.key().name(),
                        "authn_jwks_last_successful_acquisition_timestamp_seconds"
                    );
                    assert_eq!(key.key().labels().count(), 0);
                    let seconds = sample.into_inner();
                    if seconds.to_bits() != 0 {
                        buffered.acquisitions.push(seconds);
                    }
                }
                DebugValue::Histogram(samples) => {
                    buffered
                        .histograms
                        .extend(std::iter::repeat_n(key.key().clone(), samples.len()));
                }
            }
        }
        buffered
    }

    #[allow(
        dead_code,
        reason = "the introspection-only profile does not assert eager counter registration"
    )]
    pub(crate) fn registered_counters(&self) -> Vec<metrics::Key> {
        self.flush_metrics().registered_counters.clone()
    }

    pub(crate) fn take_counters(&self) -> Vec<(metrics::Key, u64)> {
        std::mem::take(&mut self.flush_metrics().counters)
    }

    pub(crate) fn take_histograms(&self) -> Vec<metrics::Key> {
        std::mem::take(&mut self.flush_metrics().histograms)
    }

    #[allow(
        dead_code,
        reason = "the introspection-only profile has no JWKS acquisition gauge"
    )]
    pub(crate) fn acquisitions(&self) -> Vec<f64> {
        self.flush_metrics().acquisitions.clone()
    }

    #[allow(
        dead_code,
        reason = "the introspection-only profile asserts span telemetry rather than log events"
    )]
    pub(crate) fn events(&self) -> Vec<String> {
        self.events
            .snapshot()
            .into_iter()
            .map(|event| render_fields("", &event.fields))
            .collect()
    }

    pub(crate) fn take_spans(&self) -> Vec<String> {
        self.spans
            .take()
            .into_iter()
            .map(|span| render_fields(&format!("span={};", span.name), &span.fields))
            .collect()
    }
}
