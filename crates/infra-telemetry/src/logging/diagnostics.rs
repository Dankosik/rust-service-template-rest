//! SDK diagnostics admitted as finite numeric observations before every output.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tracing::field::{Field, Visit};
use tracing::{Event, Metadata};

const EVENTS: [&str; 8] = [
    "queue_dropping_started",
    "queue_spans_dropped",
    "partial_success",
    "response_parse_error",
    "http_status_error",
    "network_error",
    "response_body_too_large",
    "export_error",
];
static COUNTS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
static DROPPED: AtomicU64 = AtomicU64::new(0);
static DROPPED_SEEN: AtomicBool = AtomicBool::new(false);
static REJECTED: AtomicU64 = AtomicU64::new(0);
static REJECTED_SEEN: AtomicBool = AtomicBool::new(false);

pub(super) fn denied(target: &str) -> bool {
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
    .any(|family| {
        target
            .strip_prefix(family)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
    })
}

pub(super) fn observed(metadata: &Metadata<'_>) -> bool {
    metadata.is_event() && denied(metadata.target()) && event_index(metadata.name()).is_some()
}

fn event_index(name: &str) -> Option<usize> {
    Some(match name {
        "BatchSpanProcessor.SpanDroppingStarted" => 0,
        "BatchSpanProcessor.SpansDropped" => 1,
        "HttpTraceClient.PartialSuccess" => 2,
        "HttpTraceClient.ResponseParseError" => 3,
        "HttpClient.StatusError" => 4,
        "HttpClient.NetworkError" => 5,
        "HttpClient.ResponseBodyTooLarge" => 6,
        "BatchSpanProcessor.ExportError" => 7,
        _ => return None,
    })
}

fn add(counter: &AtomicU64, amount: u64) {
    let _ = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

// Unknown fields never format Debug/Display or retain strings. In particular,
// receiver text and URL/body diagnostics cannot enter a later output layer.
#[derive(Default)]
struct Numeric {
    dropped: Option<u64>,
    rejected: Option<u64>,
}

impl Visit for Numeric {
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "dropped_span_count" => self.dropped = Some(value),
            "rejected_spans" => self.rejected = Some(value),
            _ => {}
        }
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        if let Ok(value) = u64::try_from(value) {
            self.record_u64(field, value);
        }
    }
}

pub(super) fn observe(event: &Event<'_>, metadata: &Metadata<'_>) {
    let Some(index) = event_index(metadata.name()) else {
        return;
    };
    add(&COUNTS[index], 1);
    let mut numbers = Numeric::default();
    event.record(&mut numbers);
    if index == 1
        && let Some(count) = numbers.dropped
    {
        DROPPED.store(count, Ordering::Relaxed);
        DROPPED_SEEN.store(true, Ordering::Release);
    }
    if index == 2
        && let Some(count) = numbers.rejected
    {
        add(&REJECTED, count);
        REJECTED_SEEN.store(true, Ordering::Release);
    }
}

pub(crate) fn publish() {
    metrics::describe_counter!(
        "telemetry_sdk_diagnostics_total",
        "Observed SDK diagnostic occurrences; finite categories only, not delivery evidence."
    );
    metrics::describe_gauge!(
        "telemetry_sdk_queue_dropped_spans",
        "Latest cumulative dropped-span count explicitly reported by the SDK; absent until observed."
    );
    metrics::describe_counter!(
        "telemetry_sdk_reported_rejected_spans_total",
        "Rejected spans explicitly reported by SDK partial-success diagnostics; absent until a numeric report."
    );
    for (index, event) in EVENTS.iter().enumerate() {
        metrics::counter!("telemetry_sdk_diagnostics_total", "event" => *event)
            .absolute(COUNTS[index].load(Ordering::Relaxed));
    }
    if DROPPED_SEEN.load(Ordering::Acquire) {
        #[allow(
            clippy::cast_precision_loss,
            reason = "Prometheus gauges use f64; the observation retains its exact u64 in the atomic"
        )]
        let count = DROPPED.load(Ordering::Relaxed) as f64;
        metrics::gauge!("telemetry_sdk_queue_dropped_spans").set(count);
    }
    if REJECTED_SEEN.load(Ordering::Acquire) {
        metrics::counter!("telemetry_sdk_reported_rejected_spans_total")
            .absolute(REJECTED.load(Ordering::Relaxed));
    }
}
