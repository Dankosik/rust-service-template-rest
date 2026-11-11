//! SDK diagnostics admitted as finite numeric observations before every output.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tracing::field::{Field, Visit};
use tracing::{Event, Metadata};

/// SDK diagnostic events counted under [`DIAGNOSTICS_METRIC`].
#[derive(Clone, Copy)]
enum SdkEvent {
    SpanDroppingStarted,
    SpansDropped,
    PartialSuccess,
    ResponseParseError,
    StatusError,
    NetworkError,
    ResponseBodyTooLarge,
    ExportError,
}

impl SdkEvent {
    /// Every event, in discriminant order: an event indexes [`COUNTS`].
    const ALL: [Self; 8] = [
        Self::SpanDroppingStarted,
        Self::SpansDropped,
        Self::PartialSuccess,
        Self::ResponseParseError,
        Self::StatusError,
        Self::NetworkError,
        Self::ResponseBodyTooLarge,
        Self::ExportError,
    ];

    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "BatchSpanProcessor.SpanDroppingStarted" => Self::SpanDroppingStarted,
            "BatchSpanProcessor.SpansDropped" => Self::SpansDropped,
            "HttpTraceClient.PartialSuccess" => Self::PartialSuccess,
            "HttpTraceClient.ResponseParseError" => Self::ResponseParseError,
            "HttpClient.StatusError" => Self::StatusError,
            "HttpClient.NetworkError" => Self::NetworkError,
            "HttpClient.ResponseBodyTooLarge" => Self::ResponseBodyTooLarge,
            "BatchSpanProcessor.ExportError" => Self::ExportError,
            _ => return None,
        })
    }

    const fn label(self) -> &'static str {
        match self {
            Self::SpanDroppingStarted => "queue_dropping_started",
            Self::SpansDropped => "queue_spans_dropped",
            Self::PartialSuccess => "partial_success",
            Self::ResponseParseError => "response_parse_error",
            Self::StatusError => "http_status_error",
            Self::NetworkError => "network_error",
            Self::ResponseBodyTooLarge => "response_body_too_large",
            Self::ExportError => "export_error",
        }
    }
}

const _: () = {
    let mut index = 0;
    while index < SdkEvent::ALL.len() {
        assert!(SdkEvent::ALL[index] as usize == index);
        index += 1;
    }
};

const DIAGNOSTICS_METRIC: &str = "telemetry_sdk_diagnostics_total";
const QUEUE_DROPPED_METRIC: &str = "telemetry_sdk_queue_dropped_spans";
const REJECTED_METRIC: &str = "telemetry_sdk_reported_rejected_spans_total";

static COUNTS: [AtomicU64; SdkEvent::ALL.len()] =
    [const { AtomicU64::new(0) }; SdkEvent::ALL.len()];
static DROPPED: Observed = Observed::new();
static REJECTED: Observed = Observed::new();

/// A number the SDK reported, absent until its first report.
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
        add(&self.value, amount);
        self.seen.store(true, Ordering::Release);
    }

    fn get(&self) -> Option<u64> {
        self.seen
            .load(Ordering::Acquire)
            .then(|| self.value.load(Ordering::Relaxed))
    }
}

/// Exporter and transport crates, denied process-wide: their events carry no
/// tracing context that would tell an export call from the service's own
/// use of the same crate.
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
    .any(|family| super::within(target, family))
}

/// Whether a denied target's callsite is an SDK diagnostic this module counts.
pub(super) fn is_known_diagnostic(metadata: &Metadata<'_>) -> bool {
    metadata.is_event() && SdkEvent::from_name(metadata.name()).is_some()
}

fn add(counter: &AtomicU64, amount: u64) {
    let _ = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

// Unknown fields never format Debug/Display or retain strings. In particular,
// receiver text and URL/body diagnostics cannot enter a later output layer.
#[derive(Default)]
struct ReportedCounts {
    dropped: Option<u64>,
    rejected: Option<u64>,
}

impl Visit for ReportedCounts {
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
    let Some(sdk_event) = SdkEvent::from_name(metadata.name()) else {
        return;
    };
    add(&COUNTS[sdk_event as usize], 1);
    let mut counts = ReportedCounts::default();
    event.record(&mut counts);
    match sdk_event {
        SdkEvent::SpansDropped => {
            if let Some(count) = counts.dropped {
                DROPPED.set(count);
            }
        }
        SdkEvent::PartialSuccess => {
            if let Some(count) = counts.rejected {
                REJECTED.add(count);
            }
        }
        _ => {}
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
    for event in SdkEvent::ALL {
        metrics::counter!(DIAGNOSTICS_METRIC, "event" => event.label())
            .absolute(COUNTS[event as usize].load(Ordering::Relaxed));
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
