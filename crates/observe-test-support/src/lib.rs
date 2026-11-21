use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::{Arc, Mutex, PoisonError},
};

use metrics::{Key, LocalRecorderGuard, SharedString, Unit};
use metrics_util::{
    CompositeKey, MetricKind,
    debugging::{DebugValue, DebuggingRecorder, Snapshotter},
};
use tracing::{Dispatch, Event, Level, Subscriber, field::Field, span};
use tracing_subscriber::{
    Layer, Registry,
    filter::filter_fn,
    layer::{Context, SubscriberExt as _},
    registry::LookupSpan,
};

pub type FieldMap = BTreeMap<&'static str, String>;
pub type Series = HashMap<CompositeKey, (Option<Unit>, Option<SharedString>, DebugValue)>;

#[derive(Debug)]
#[must_use]
pub struct ScopedDispatch {
    _guard: tracing::dispatcher::DefaultGuard,
    _anchor: Dispatch,
}

#[must_use]
pub fn install_default(dispatch: &Dispatch) -> ScopedDispatch {
    let anchor = Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let guard = tracing::dispatcher::set_default(dispatch);
    ScopedDispatch {
        _guard: guard,
        _anchor: anchor,
    }
}

#[must_use]
pub fn dispatch_spans_and_events(spans: &SpanCapture, events: &EventCapture) -> Dispatch {
    Dispatch::new(Registry::default().with(spans.clone()).with(events.clone()))
}

#[must_use]
pub fn install_spans_and_events(spans: &SpanCapture, events: &EventCapture) -> ScopedDispatch {
    install_default(&dispatch_spans_and_events(spans, events))
}

fn unquote(raw: &str) -> &str {
    raw.strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(raw)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedEvent {
    pub level: Level,
    pub target: &'static str,
    pub span: Option<&'static str>,
    pub fields: FieldMap,
}

impl CapturedEvent {
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.field("message")
    }

    #[must_use]
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    #[must_use]
    pub fn unquoted(&self, name: &str) -> Option<&str> {
        self.field(name).map(unquote)
    }
}

#[derive(Clone, Debug, Default)]
pub struct EventCapture {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl EventCapture {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn dispatch(&self) -> Dispatch {
        Dispatch::new(Registry::default().with(self.clone()))
    }

    #[must_use]
    pub fn dispatch_for(&self, targets: &'static [&'static str]) -> Dispatch {
        let filter =
            filter_fn(move |metadata| metadata.is_span() || targets.contains(&metadata.target()));
        Dispatch::new(Registry::default().with(self.clone()).with(filter))
    }

    #[must_use]
    pub fn dispatch_target_prefix(&self, prefix: &'static str) -> Dispatch {
        let filter =
            filter_fn(move |metadata| metadata.is_span() || metadata.target().starts_with(prefix));
        Dispatch::new(Registry::default().with(self.clone()).with(filter))
    }

    #[must_use]
    pub fn install_default(&self) -> ScopedDispatch {
        install_default(&self.dispatch())
    }

    #[must_use]
    pub fn install_for(&self, targets: &'static [&'static str]) -> ScopedDispatch {
        install_default(&self.dispatch_for(targets))
    }

    #[must_use]
    pub fn install_target_prefix(&self, prefix: &'static str) -> ScopedDispatch {
        install_default(&self.dispatch_target_prefix(prefix))
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<CapturedEvent> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[must_use]
    pub fn take(&self) -> Vec<CapturedEvent> {
        std::mem::take(&mut *self.events.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl<S> Layer<S> for EventCapture
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = FieldMap::new();
        event.record(&mut |field: &Field, value: &dyn fmt::Debug| {
            fields.insert(field.name(), format!("{value:?}"));
        });
        let metadata = event.metadata();
        let captured = CapturedEvent {
            level: *metadata.level(),
            target: metadata.target(),
            span: ctx.event_span(event).map(|span| span.name()),
            fields,
        };
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(captured);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedSpan {
    pub name: &'static str,
    pub target: &'static str,
    pub fields: FieldMap,
}

impl CapturedSpan {
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    #[must_use]
    pub fn unquoted(&self, name: &str) -> Option<&str> {
        self.field(name).map(unquote)
    }
}

#[derive(Clone)]
struct SpanEntry(Arc<Mutex<CapturedSpan>>);

#[derive(Clone, Debug, Default)]
pub struct SpanCapture {
    spans: Arc<Mutex<Vec<Arc<Mutex<CapturedSpan>>>>>,
}

impl SpanCapture {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn dispatch(&self) -> Dispatch {
        Dispatch::new(Registry::default().with(self.clone()))
    }

    #[must_use]
    pub fn dispatch_name(&self, name: &'static str) -> Dispatch {
        let filter = filter_fn(move |metadata| metadata.is_span() && metadata.name() == name);
        Dispatch::new(Registry::default().with(self.clone()).with(filter))
    }

    #[must_use]
    pub fn install_default(&self) -> ScopedDispatch {
        install_default(&self.dispatch())
    }

    #[must_use]
    pub fn install_name(&self, name: &'static str) -> ScopedDispatch {
        install_default(&self.dispatch_name(name))
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<CapturedSpan> {
        let entries = self
            .spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        entries
            .into_iter()
            .map(|entry| entry.lock().unwrap_or_else(PoisonError::into_inner).clone())
            .collect()
    }

    #[must_use]
    pub fn take(&self) -> Vec<CapturedSpan> {
        let entries =
            std::mem::take(&mut *self.spans.lock().unwrap_or_else(PoisonError::into_inner));
        entries
            .into_iter()
            .map(|entry| entry.lock().unwrap_or_else(PoisonError::into_inner).clone())
            .collect()
    }
}

impl<S> Layer<S> for SpanCapture
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attributes: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        let mut fields = FieldMap::new();
        attributes.record(&mut |field: &Field, value: &dyn fmt::Debug| {
            fields.insert(field.name(), format!("{value:?}"));
        });
        let metadata = attributes.metadata();
        let entry = Arc::new(Mutex::new(CapturedSpan {
            name: metadata.name(),
            target: metadata.target(),
            fields,
        }));
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanEntry(Arc::clone(&entry)));
        }
        self.spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(entry);
    }

    fn on_record(&self, id: &span::Id, values: &span::Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let Some(entry) = span.extensions().get::<SpanEntry>().cloned() else {
            return;
        };
        let mut fields = FieldMap::new();
        values.record(&mut |field: &Field, value: &dyn fmt::Debug| {
            fields.insert(field.name(), format!("{value:?}"));
        });
        entry
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .fields
            .extend(fields);
    }
}

fn value<'a>(series: &'a Series, kind: MetricKind, key: &Key) -> Option<&'a DebugValue> {
    series
        .get(&CompositeKey::new(kind, key.clone()))
        .map(|(_, _, value)| value)
}

#[must_use]
pub fn counter(series: &Series, key: &Key) -> Option<u64> {
    match value(series, MetricKind::Counter, key)? {
        DebugValue::Counter(count) => Some(*count),
        _ => None,
    }
}

#[must_use]
pub fn gauge(series: &Series, key: &Key) -> Option<f64> {
    match value(series, MetricKind::Gauge, key)? {
        DebugValue::Gauge(sample) => Some(sample.into_inner()),
        _ => None,
    }
}

#[must_use]
pub fn histogram_len(series: &Series, key: &Key) -> Option<usize> {
    match value(series, MetricKind::Histogram, key)? {
        DebugValue::Histogram(samples) => Some(samples.len()),
        _ => None,
    }
}

#[must_use]
pub fn labeled(key: &Key, labels: &[(&str, &str)]) -> bool {
    labels.iter().all(|(name, value)| {
        key.labels()
            .any(|label| label.key() == *name && label.value() == *value)
    })
}

#[derive(Clone, Debug)]
pub struct MetricsCapture {
    recorder: Arc<DebuggingRecorder>,
    snapshotter: Snapshotter,
}

impl Default for MetricsCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsCapture {
    #[must_use]
    pub fn new() -> Self {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        Self {
            recorder: Arc::new(recorder),
            snapshotter,
        }
    }

    #[must_use]
    pub fn recorder(&self) -> &DebuggingRecorder {
        &self.recorder
    }

    #[must_use]
    pub fn install_default(&self) -> LocalRecorderGuard<'_> {
        metrics::set_default_local_recorder(self.recorder())
    }

    #[must_use]
    pub fn snapshot(&self) -> Series {
        self.snapshotter.snapshot().into_hashmap()
    }

    #[must_use]
    pub fn snapshot_vec(
        &self,
    ) -> Vec<(CompositeKey, Option<Unit>, Option<SharedString>, DebugValue)> {
        self.snapshotter.snapshot().into_vec()
    }

    #[must_use]
    pub fn counter(&self, key: &Key) -> Option<u64> {
        counter(&self.snapshot(), key)
    }

    #[must_use]
    pub fn gauge(&self, key: &Key) -> Option<f64> {
        gauge(&self.snapshot(), key)
    }

    #[must_use]
    pub fn histogram_len(&self, key: &Key) -> Option<usize> {
        histogram_len(&self.snapshot(), key)
    }
}

type MetricSeries = (String, Vec<(String, String)>);

#[derive(Clone, Debug, Default)]
pub struct CumulativeMetrics {
    capture: MetricsCapture,
    counters: Arc<Mutex<BTreeMap<MetricSeries, u64>>>,
    histograms: Arc<Mutex<BTreeMap<MetricSeries, usize>>>,
}

impl CumulativeMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn install_default(&self) -> LocalRecorderGuard<'_> {
        self.capture.install_default()
    }

    fn drain(&self) {
        let entries = self.capture.snapshot_vec();
        let mut counters = self.counters.lock().unwrap_or_else(PoisonError::into_inner);
        let mut histograms = self
            .histograms
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for (key, _, _, value) in entries {
            let mut labels: Vec<(String, String)> = key
                .key()
                .labels()
                .map(|label| (label.key().to_owned(), label.value().to_owned()))
                .collect();
            labels.sort();
            let series = (key.key().name().to_owned(), labels);
            match value {
                DebugValue::Counter(delta) => {
                    *counters.entry(series).or_insert(0) += delta;
                }
                DebugValue::Histogram(samples) => {
                    *histograms.entry(series).or_insert(0) += samples.len();
                }
                DebugValue::Gauge(_) => {}
            }
        }
    }

    #[must_use]
    pub fn counter(&self, target: &str, required: &[(&str, &str)]) -> u64 {
        self.drain();
        self.counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|((name, labels), _)| {
                name == target
                    && required
                        .iter()
                        .all(|(k, v)| labels.iter().any(|(lk, lv)| lk == k && lv == v))
            })
            .map(|(_, count)| *count)
            .sum()
    }

    #[must_use]
    pub fn histogram_samples(&self, target: &str, required: &[(&str, &str)]) -> usize {
        self.drain();
        self.histograms
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|((name, labels), _)| {
                name == target
                    && required
                        .iter()
                        .all(|(k, v)| labels.iter().any(|(lk, lv)| lk == k && lv == v))
            })
            .map(|(_, count)| *count)
            .sum()
    }

    #[must_use]
    pub fn counters_by_label(&self, target: &str, label: &str) -> BTreeMap<String, u64> {
        self.drain();
        self.counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|((name, _), count)| name == target && **count > 0)
            .filter_map(|((_, labels), count)| {
                let value = labels
                    .iter()
                    .find_map(|(key, value)| (key == label).then_some(value.clone()))?;
                Some((value, *count))
            })
            .collect()
    }
}
