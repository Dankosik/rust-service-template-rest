use std::{
    fmt::Write as _,
    sync::{Arc, Mutex},
};

use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
use tracing_subscriber::{
    Layer, Registry,
    layer::{Context, SubscriberExt as _},
    registry::LookupSpan,
};

#[derive(Default)]
struct BufferedMetrics {
    counters: Vec<(metrics::Key, u64)>,
    histograms: Vec<metrics::Key>,
    acquisitions: Vec<f64>,
}

#[derive(Clone)]
struct SpanEntry(Arc<Mutex<String>>);

#[derive(Default)]
struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        write!(self.0, "{}={value:?};", field.name()).unwrap();
    }
}

#[derive(Clone)]
pub(crate) struct Diagnostics {
    recorder: Arc<DebuggingRecorder>,
    snapshotter: Snapshotter,
    metrics: Arc<Mutex<BufferedMetrics>>,
    events: Arc<Mutex<Vec<String>>>,
    spans: Arc<Mutex<Vec<Arc<Mutex<String>>>>>,
}

impl Default for Diagnostics {
    fn default() -> Self {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        Self {
            recorder: Arc::new(recorder),
            snapshotter,
            metrics: Arc::default(),
            events: Arc::default(),
            spans: Arc::default(),
        }
    }
}

impl Diagnostics {
    pub(crate) fn recorder(&self) -> &DebuggingRecorder {
        &self.recorder
    }

    pub(crate) fn dispatch(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(Registry::default().with(self.clone()))
    }

    fn flush_metrics(&self) -> std::sync::MutexGuard<'_, BufferedMetrics> {
        let snapshot = self.snapshotter.snapshot().into_vec();
        let mut buffered = self.metrics.lock().unwrap();
        for (key, _, _, value) in snapshot {
            match value {
                DebugValue::Counter(count) if count > 0 => {
                    buffered.counters.push((key.key().clone(), count));
                }
                DebugValue::Counter(_) => {}
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
        self.events.lock().unwrap().clone()
    }

    pub(crate) fn take_spans(&self) -> Vec<String> {
        let entries = std::mem::take(&mut *self.spans.lock().unwrap());
        entries
            .into_iter()
            .map(|span| span.lock().unwrap().clone())
            .collect()
    }
}

impl<S> Layer<S> for Diagnostics
where
    S: tracing::Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let mut fields = Fields(format!("span={};", attributes.metadata().name()));
        attributes.record(&mut fields);
        let entry = Arc::new(Mutex::new(fields.0));
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanEntry(Arc::clone(&entry)));
        }
        self.spans.lock().unwrap().push(entry);
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let extensions = span.extensions();
        let Some(entry) = extensions.get::<SpanEntry>() else {
            return;
        };
        let mut fields = Fields::default();
        values.record(&mut fields);
        entry.0.lock().unwrap().push_str(&fields.0);
    }

    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.events.lock().unwrap().push(fields.0);
    }
}
