//! Metric and trace capture for authentication tests.

use std::sync::Arc;

#[derive(Clone, Default)]
pub(crate) struct Diagnostics {
    pub(crate) counters: Arc<std::sync::Mutex<Vec<(metrics::Key, u64)>>>,
    pub(crate) histograms: Arc<std::sync::Mutex<Vec<metrics::Key>>>,
    pub(crate) acquisitions: Arc<std::sync::Mutex<Vec<f64>>>,
    pub(crate) events: Arc<std::sync::Mutex<Vec<String>>>,
    /// Each span's fields, the ones recorded later appended to its entry.
    pub(crate) spans: Arc<std::sync::Mutex<Vec<String>>>,
}
/// Renders fields as `name=value;` with values in their debug form.
#[derive(Default)]
struct Fields(String);
impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        write!(self.0, "{}={value:?};", field.name()).unwrap();
    }
}
struct RecordedHistogram {
    key: metrics::Key,
    diagnostics: Diagnostics,
}
impl metrics::HistogramFn for RecordedHistogram {
    fn record(&self, _: f64) {
        self.diagnostics
            .histograms
            .lock()
            .unwrap()
            .push(self.key.clone());
    }
}
struct RecordedCounter {
    key: metrics::Key,
    diagnostics: Diagnostics,
}
impl metrics::CounterFn for RecordedCounter {
    fn increment(&self, value: u64) {
        self.diagnostics
            .counters
            .lock()
            .unwrap()
            .push((self.key.clone(), value));
    }
    fn absolute(&self, value: u64) {
        self.increment(value);
    }
}
impl metrics::GaugeFn for Diagnostics {
    fn increment(&self, _: f64) {
        panic!("acquisition time must be assigned from the wall clock");
    }

    fn decrement(&self, _: f64) {
        panic!("acquisition time must be assigned from the wall clock");
    }

    fn set(&self, value: f64) {
        self.acquisitions.lock().unwrap().push(value);
    }
}
impl metrics::Recorder for Diagnostics {
    fn describe_counter(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn describe_gauge(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn describe_histogram(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn register_counter(&self, key: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Counter {
        metrics::Counter::from_arc(Arc::new(RecordedCounter {
            key: key.clone(),
            diagnostics: self.clone(),
        }))
    }
    fn register_gauge(&self, key: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
        assert_eq!(
            key.name(),
            "authn_jwks_last_successful_acquisition_timestamp_seconds"
        );
        assert_eq!(key.labels().count(), 0);
        metrics::Gauge::from_arc(Arc::new(self.clone()))
    }
    fn register_histogram(
        &self,
        key: &metrics::Key,
        _: &metrics::Metadata<'_>,
    ) -> metrics::Histogram {
        metrics::Histogram::from_arc(Arc::new(RecordedHistogram {
            key: key.clone(),
            diagnostics: self.clone(),
        }))
    }
}
impl tracing::Subscriber for Diagnostics {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut fields = Fields(format!("span={};", attributes.metadata().name()));
        attributes.record(&mut fields);
        let mut spans = self.spans.lock().unwrap();
        spans.push(fields.0);
        tracing::span::Id::from_u64(spans.len() as u64)
    }
    fn record(&self, id: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        let mut fields = Fields::default();
        values.record(&mut fields);
        let index = usize::try_from(id.into_u64()).unwrap() - 1;
        self.spans.lock().unwrap()[index].push_str(&fields.0);
    }
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.events.lock().unwrap().push(fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}
