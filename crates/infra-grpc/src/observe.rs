use std::mem::DropGuard;
use std::sync::{Arc, Once, OnceLock};
use std::time::{Duration, Instant};

use foldhash::HashMap;
use opentelemetry::trace::{SpanKind, Status as SpanStatus};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::{otel_trace_span, set_parent_or_fallback};

const SERVER_STARTED_METRIC: &str = "grpc_server_started_total";
const SERVER_HANDLED_METRIC: &str = "grpc_server_handled_total";
const SERVER_HANDLING_METRIC: &str = "grpc_server_handling_seconds";
const SERVER_SHED_METRIC: &str = "grpc_server_shed_requests_total";
const SERVER_ADMISSION_WAIT_METRIC: &str = "grpc_server_admission_wait_seconds";
const SERVER_ADMISSION_WAITING_METRIC: &str = "grpc_server_admission_waiting";
const SERVER_FAILURES_METRIC: &str = "grpc_server_failures_total";

const DIAGNOSTIC_BYTES: usize = 1024;
const UNKNOWN_PATH: &str = "";
const HANDLING_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

pub const HISTOGRAMS: &[(&str, &[f64])] = &[
    (SERVER_HANDLING_METRIC, HANDLING_BUCKETS),
    (SERVER_ADMISSION_WAIT_METRIC, HANDLING_BUCKETS),
];

static DESCRIBE: Once = Once::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CallType {
    Unary,
    ClientStream,
    ServerStream,
    BidiStream,
}

impl CallType {
    pub(crate) const fn new(client_streaming: bool, server_streaming: bool) -> Self {
        match (client_streaming, server_streaming) {
            (false, false) => Self::Unary,
            (true, false) => Self::ClientStream,
            (false, true) => Self::ServerStream,
            (true, true) => Self::BidiStream,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Unary => "unary",
            Self::ClientStream => "client_stream",
            Self::ServerStream => "server_stream",
            Self::BidiStream => "bidi_stream",
        }
    }
}

pub(crate) struct Methods {
    registered: HashMap<Box<str>, (CallType, OnceLock<Arc<MethodMetrics>>)>,
    unknown: OnceLock<Arc<MethodMetrics>>,
}

struct MethodMetrics {
    call_type: Option<CallType>,
    rpc_method: Option<Arc<str>>,
    base_labels: Box<[metrics::Label]>,
    started: metrics::Counter,
    handling_seconds: metrics::Histogram,
    handled: [OnceLock<metrics::Counter>; 17],
    failed: [OnceLock<metrics::Counter>; service_failure::Code::ALL.len()],
}

impl Methods {
    pub(crate) fn new(methods: HashMap<Box<str>, CallType>) -> Self {
        describe();
        Self {
            registered: methods
                .into_iter()
                .map(|(path, call_type)| (path, (call_type, OnceLock::new())))
                .collect(),
            unknown: OnceLock::new(),
        }
    }

    fn resolve(&self, path: &str) -> Arc<MethodMetrics> {
        if let Some((registered_path, (call_type, slot))) = self.registered.get_key_value(path) {
            return Arc::clone(slot.get_or_init(|| {
                Arc::new(MethodMetrics::build(registered_path, Some(*call_type)))
            }));
        }
        Arc::clone(
            self.unknown
                .get_or_init(|| Arc::new(MethodMetrics::build(UNKNOWN_PATH, None))),
        )
    }
}

impl MethodMetrics {
    fn build(path: &str, call_type: Option<CallType>) -> Self {
        let parsed = crate::protocol::split_method_path(path);
        let (service, method) = parsed.unwrap_or(("unknown", "unknown"));
        let labels = vec![
            metrics::Label::new("grpc_type", call_type.map_or("unknown", CallType::label)),
            metrics::Label::new("grpc_service", service.to_owned()),
            metrics::Label::new("grpc_method", method.to_owned()),
        ];
        Self {
            call_type,
            rpc_method: parsed.map(|(svc, rpc)| Arc::from(format!("{svc}/{rpc}"))),
            started: metrics::counter!(SERVER_STARTED_METRIC, labels.clone()),
            handling_seconds: metrics::histogram!(SERVER_HANDLING_METRIC, labels.clone()),
            base_labels: labels.into_boxed_slice(),
            handled: std::array::from_fn(|_| OnceLock::new()),
            failed: std::array::from_fn(|_| OnceLock::new()),
        }
    }

    fn span_name(&self) -> &str {
        self.rpc_method.as_deref().unwrap_or("grpc")
    }

    fn record_handled(&self, code: tonic::Code, elapsed: Duration) {
        let (_, metric_code) = code_labels(code);
        self.handling_seconds.record(elapsed.as_secs_f64());
        self.handled[code as usize]
            .get_or_init(|| {
                let mut labels = self.base_labels.to_vec();
                labels.push(metrics::Label::new("grpc_code", metric_code));
                metrics::counter!(SERVER_HANDLED_METRIC, labels)
            })
            .increment(1);
    }

    fn record_failure(&self, failure: service_failure::Code) {
        self.failed[failure as usize]
            .get_or_init(|| {
                let mut labels = self.base_labels.to_vec();
                labels.push(metrics::Label::new("failure_code", failure.as_str()));
                metrics::counter!(SERVER_FAILURES_METRIC, labels)
            })
            .increment(1);
    }
}

pub(crate) struct Call {
    metrics: Arc<MethodMetrics>,
    span: tracing::Span,
    started_at: Instant,
    completed: bool,
}

impl Call {
    pub(crate) fn start<B>(methods: &Methods, request: &http::Request<B>) -> Self {
        let started_at = Instant::now();
        let path = request.uri().path();
        let metrics = methods.resolve(path);
        let span = open_server_span(path, &metrics);
        metrics.started.increment(1);
        Self {
            metrics,
            span,
            started_at,
            completed: false,
        }
    }

    pub(crate) fn call_type(&self) -> Option<CallType> {
        self.metrics.call_type
    }

    pub(crate) const fn span(&self) -> &tracing::Span {
        &self.span
    }

    pub(crate) fn record_failure(&self, failure: service_failure::Code) {
        self.span.set_attribute("failure.code", failure.as_str());
        self.metrics.record_failure(failure);
    }

    pub(crate) fn finish(mut self, code: tonic::Code) {
        self.emit(code);
        self.completed = true;
    }

    fn emit(&self, code: tonic::Code) {
        let (span_code, _) = code_labels(code);
        self.span.set_attribute("rpc.status_code", span_code);
        if otel_http::grpc::status_is_error(code as u16, true) {
            self.span.set_attribute("error.type", span_code);
            self.span.set_status(SpanStatus::error(""));
        }
        self.metrics.record_handled(code, self.started_at.elapsed());
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if !self.completed {
            self.emit(tonic::Code::Cancelled);
        }
    }
}

pub(crate) fn propagated_parent(
    span: &tracing::Span,
    headers: &http::HeaderMap,
) -> Option<opentelemetry::Context> {
    set_parent_or_fallback(span, otel_http::extract_context(headers))
}

fn open_server_span(path: &str, metrics: &MethodMetrics) -> tracing::Span {
    let span = otel_trace_span!(
        "GRPC request",
        otel.name = metrics.span_name(),
        otel.kind = ?SpanKind::Server,
    );
    span.set_attribute("rpc.system.name", "grpc");
    if let Some(method) = &metrics.rpc_method {
        span.set_attribute("rpc.method", Arc::clone(method));
    } else {
        span.set_attribute("rpc.method", "_OTHER");
        let raw = path.trim_start_matches('/');
        if !raw.is_empty() && raw.len() <= DIAGNOSTIC_BYTES {
            span.set_attribute("rpc.method_original", raw.to_owned());
        }
    }
    span
}

pub(crate) fn record_shed() {
    describe();
    metrics::counter!(SERVER_SHED_METRIC).increment(1);
}

pub(crate) fn register_shed() {
    describe();
    metrics::counter!(SERVER_SHED_METRIC).increment(0);
}

#[must_use]
pub(crate) fn admission_waiting() -> impl Drop {
    describe();
    metrics::gauge!(SERVER_ADMISSION_WAITING_METRIC).increment(1.0);
    DropGuard::new(tokio::time::Instant::now(), |started| {
        metrics::gauge!(SERVER_ADMISSION_WAITING_METRIC).decrement(1.0);
        metrics::histogram!(SERVER_ADMISSION_WAIT_METRIC).record(started.elapsed().as_secs_f64());
    })
}

fn describe() {
    DESCRIBE.call_once(|| {
        metrics::describe_counter!(
            SERVER_STARTED_METRIC,
            metrics::Unit::Count,
            "Server gRPC calls received by service and method"
        );
        metrics::describe_counter!(
            SERVER_HANDLED_METRIC,
            metrics::Unit::Count,
            "Completed server gRPC calls by service, method, and status code"
        );
        metrics::describe_histogram!(
            SERVER_HANDLING_METRIC,
            metrics::Unit::Seconds,
            "Server gRPC time to the call's status by service and method"
        );
        metrics::describe_counter!(
            SERVER_SHED_METRIC,
            metrics::Unit::Count,
            "Business gRPC calls rejected without running a handler because no concurrency permit freed within grpc.admission_wait or the call's remaining budget"
        );
        metrics::describe_gauge!(
            SERVER_ADMISSION_WAITING_METRIC,
            metrics::Unit::Count,
            "Business gRPC calls waiting for a concurrency permit now"
        );
        metrics::describe_histogram!(
            SERVER_ADMISSION_WAIT_METRIC,
            metrics::Unit::Seconds,
            "Time business gRPC calls waited for a concurrency permit, recorded only for a call that had to wait"
        );
        metrics::describe_counter!(
            SERVER_FAILURES_METRIC,
            metrics::Unit::Count,
            "Server gRPC calls answered with a failure from the shared catalog by service, method, and catalog code"
        );
    });
}

const fn code_labels(code: tonic::Code) -> (&'static str, &'static str) {
    shared_rules::grpc::status_code_names(code as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_types_follow_the_descriptor_streaming_flags() {
        assert_eq!(CallType::new(false, false).label(), "unary");
        assert_eq!(CallType::new(true, false).label(), "client_stream");
        assert_eq!(CallType::new(false, true).label(), "server_stream");
        assert_eq!(CallType::new(true, true).label(), "bidi_stream");
    }

    #[test]
    fn code_labels_match_otel_and_grpc_go_names() {
        assert_eq!(code_labels(tonic::Code::Ok), ("OK", "OK"));
        assert_eq!(
            code_labels(tonic::Code::Cancelled),
            ("CANCELLED", "Canceled")
        );
        assert_eq!(
            code_labels(tonic::Code::DeadlineExceeded),
            ("DEADLINE_EXCEEDED", "DeadlineExceeded")
        );
        assert_eq!(
            code_labels(tonic::Code::FailedPrecondition),
            ("FAILED_PRECONDITION", "FailedPrecondition")
        );
        assert_eq!(
            code_labels(tonic::Code::Unauthenticated),
            ("UNAUTHENTICATED", "Unauthenticated")
        );
    }

    #[test]
    fn server_spans_carry_stable_rpc_attributes() {
        use opentelemetry::trace::TracerProvider as _;
        use tracing_subscriber::layer::SubscriberExt as _;

        let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        let _anchor = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let guard = tracing::subscriber::set_default(subscriber);

        let methods = Methods::new(
            std::iter::once(("/example.v1.EchoService/Unary".into(), CallType::Unary)).collect(),
        );
        let build_req = |path: &str| http::Request::builder().uri(path).body(()).unwrap();

        let known = Call::start(&methods, &build_req("/example.v1.EchoService/Unary"));
        assert_eq!(known.call_type(), Some(CallType::Unary));
        known.finish(tonic::Code::Unauthenticated);

        let unknown = Call::start(&methods, &build_req("/example.v1.EchoService/Invented"));
        assert_eq!(unknown.call_type(), None);
        unknown.finish(tonic::Code::Unimplemented);
        drop(guard);

        let spans = exporter.get_finished_spans().unwrap();
        let attr = |span: &opentelemetry_sdk::trace::SpanData, key: &str| {
            span.attributes
                .iter()
                .find(|entry| entry.key.as_str() == key)
                .map(|entry| entry.value.as_str().into_owned())
        };
        let [known_span, unknown_span] = spans.as_slice() else {
            panic!("expected two server spans, got {spans:?}");
        };
        for span in &spans {
            assert_eq!(span.span_kind, SpanKind::Server);
            assert_eq!(attr(span, "rpc.system.name").as_deref(), Some("grpc"));
            for deprecated in ["rpc.system", "rpc.service", "rpc.grpc.status_code"] {
                assert_eq!(attr(span, deprecated), None, "{deprecated}");
            }
        }
        assert_eq!(known_span.name, "example.v1.EchoService/Unary");
        assert_eq!(
            attr(known_span, "rpc.method").as_deref(),
            Some("example.v1.EchoService/Unary")
        );
        assert_eq!(
            attr(known_span, "rpc.status_code").as_deref(),
            Some("UNAUTHENTICATED")
        );
        assert_eq!(attr(known_span, "error.type"), None);

        assert_eq!(unknown_span.name, "grpc");
        assert_eq!(attr(unknown_span, "rpc.method").as_deref(), Some("_OTHER"));
        assert_eq!(
            attr(unknown_span, "rpc.method_original").as_deref(),
            Some("example.v1.EchoService/Invented")
        );
        assert_eq!(
            attr(unknown_span, "error.type").as_deref(),
            Some("UNIMPLEMENTED")
        );
    }
}
