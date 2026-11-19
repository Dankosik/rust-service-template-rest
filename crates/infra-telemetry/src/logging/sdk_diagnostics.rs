use enum_map::{Enum, EnumMap, enum_len};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tracing::field::{Field, Visit};
use tracing::{Event, Metadata};

macro_rules! sdk_events {
    ($($variant:ident => ($name:literal, $label:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Enum)]
        enum SdkEvent { $($variant),+ }

        impl SdkEvent {
            fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }

            const fn label(self) -> &'static str {
                match self { $(Self::$variant => $label),+ }
            }
        }
    };
}

sdk_events! {
    SpanDroppingStarted => ("BatchSpanProcessor.SpanDroppingStarted", "queue_dropping_started"),
    SpansDropped => ("BatchSpanProcessor.SpansDropped", "queue_spans_dropped"),
    PartialSuccess => ("HttpTraceClient.PartialSuccess", "partial_success"),
    ResponseParseError => ("HttpTraceClient.ResponseParseError", "response_parse_error"),
    StatusError => ("HttpClient.StatusError", "http_status_error"),
    NetworkError => ("HttpClient.NetworkError", "network_error"),
    ResponseBodyTooLarge => ("HttpClient.ResponseBodyTooLarge", "response_body_too_large"),
    ExportError => ("BatchSpanProcessor.ExportError", "export_error"),
}

const DIAGNOSTICS_METRIC: &str = "telemetry_sdk_diagnostics_total";
const QUEUE_DROPPED_METRIC: &str = "telemetry_sdk_queue_dropped_spans";
const REJECTED_METRIC: &str = "telemetry_sdk_reported_rejected_spans_total";

static COUNTS: EnumMap<SdkEvent, AtomicU64> =
    EnumMap::from_array([const { AtomicU64::new(0) }; enum_len::<SdkEvent>()]);
static DROPPED: Observed = Observed::new();
static REJECTED: Observed = Observed::new();

struct Observed {
    value: AtomicU64,
    seen: AtomicBool,
}

impl Observed {
    const fn new() -> Self {
        Self {
            value: AtomicU64::new(0),
            seen: AtomicBool::new(false),
        }
    }

    fn set(&self, value: u64) {
        self.value.store(value, Ordering::Relaxed);
        self.seen.store(true, Ordering::Release);
    }

    fn add(&self, amount: u64) {
        saturating_add(&self.value, amount);
        self.seen.store(true, Ordering::Release);
    }

    fn get(&self) -> Option<u64> {
        self.seen
            .load(Ordering::Acquire)
            .then(|| self.value.load(Ordering::Relaxed))
    }
}

pub(super) fn is_denied_target(target: &str) -> bool {
    [
        "opentelemetry",
        "opentelemetry_sdk",
        "opentelemetry-sdk",
        "opentelemetry_otlp",
        "opentelemetry-otlp",
        "opentelemetry_http",
        "opentelemetry-http",
        "tracing_opentelemetry",
        "tracing-opentelemetry",
        "async_nats",
        "reqwest",
        "hyper",
        "hyper_util",
        "hyper-util",
        "h2",
        "rustls",
        "rustls_platform_verifier",
        "rustls-platform-verifier",
    ]
    .iter()
    .any(|family| super::is_within_target(target, family))
}

pub(super) fn is_known_diagnostic(metadata: &Metadata<'_>) -> bool {
    metadata.is_event() && SdkEvent::from_name(metadata.name()).is_some()
}

fn saturating_add(counter: &AtomicU64, amount: u64) {
    let _ = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

struct NumericField {
    name: &'static str,
    value: Option<u64>,
}

impl Visit for NumericField {
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == self.name {
            self.value = Some(value);
        }
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        if let Ok(value) = u64::try_from(value) {
            self.record_u64(field, value);
        }
    }
}

fn numeric_field(event: &Event<'_>, name: &'static str) -> Option<u64> {
    let mut visitor = NumericField { name, value: None };
    event.record(&mut visitor);
    visitor.value
}

pub(super) fn observe(event: &Event<'_>, metadata: &Metadata<'_>) {
    let Some(sdk_event) = SdkEvent::from_name(metadata.name()) else {
        return;
    };
    saturating_add(&COUNTS[sdk_event], 1);
    match sdk_event {
        SdkEvent::SpansDropped => {
            if let Some(count) = numeric_field(event, "dropped_span_count") {
                DROPPED.set(count);
            }
        }
        SdkEvent::PartialSuccess => {
            if let Some(count) = numeric_field(event, "rejected_spans") {
                REJECTED.add(count);
            }
        }
        SdkEvent::SpanDroppingStarted
        | SdkEvent::ResponseParseError
        | SdkEvent::StatusError
        | SdkEvent::NetworkError
        | SdkEvent::ResponseBodyTooLarge
        | SdkEvent::ExportError => {}
    }
}

pub(super) fn describe_metrics() {
    metrics::describe_counter!(
        DIAGNOSTICS_METRIC,
        "Observed SDK diagnostic occurrences; finite categories only, not delivery evidence."
    );
    metrics::describe_gauge!(
        QUEUE_DROPPED_METRIC,
        "Latest cumulative dropped-span count explicitly reported by the SDK; absent until observed."
    );
    metrics::describe_counter!(
        REJECTED_METRIC,
        "Rejected spans explicitly reported by SDK partial-success diagnostics; absent until a numeric report."
    );
}

pub(super) fn publish() {
    for (event, count) in &COUNTS {
        metrics::counter!(DIAGNOSTICS_METRIC, "event" => event.label())
            .absolute(count.load(Ordering::Relaxed));
    }
    if let Some(count) = DROPPED.get() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "Prometheus gauges use f64; the observation retains its exact u64 in the atomic"
        )]
        let count = count as f64;
        metrics::gauge!(QUEUE_DROPPED_METRIC).set(count);
    }
    if let Some(count) = REJECTED.get() {
        metrics::counter!(REJECTED_METRIC).absolute(count);
    }
}
