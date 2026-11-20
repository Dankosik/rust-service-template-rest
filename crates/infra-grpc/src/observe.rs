use std::mem::DropGuard;
use std::sync::{Arc, Once, OnceLock};
use std::time::{Duration, Instant};

use enum_map::EnumMap;
use infra_grpc_core::observe::{Call as CoreCall, CallMetrics, StatusCounters, open_rpc_span};
use opentelemetry::trace::SpanKind;
use shared_rules::grpc::{HANDLING_SECONDS_BUCKETS, RpcName};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::set_parent_or_fallback;

use crate::services::{CallType, Method};

const SERVER_STARTED_METRIC: &str = "grpc_server_started_total";
const SERVER_HANDLED_METRIC: &str = "grpc_server_handled_total";
const SERVER_HANDLING_METRIC: &str = "grpc_server_handling_seconds";
const SERVER_SHED_METRIC: &str = "grpc_server_shed_requests_total";
const SERVER_ADMISSION_WAIT_METRIC: &str = "grpc_server_admission_wait_seconds";
const SERVER_ADMISSION_WAITING_METRIC: &str = "grpc_server_admission_waiting";
const SERVER_FAILURES_METRIC: &str = "grpc_server_failures_total";

pub const HISTOGRAMS: &[(&str, &[f64])] = &[
    (SERVER_HANDLING_METRIC, HANDLING_SECONDS_BUCKETS),
    (SERVER_ADMISSION_WAIT_METRIC, HANDLING_SECONDS_BUCKETS),
];

static DESCRIBE: Once = Once::new();

impl CallType {
    const fn label(self) -> &'static str {
        match self {
            Self::Unary => "unary",
            Self::ClientStream => "client_stream",
            Self::ServerStream => "server_stream",
            Self::BidiStream => "bidi_stream",
        }
    }
}

pub(crate) struct MethodMetrics {
    rpc_method: Option<Arc<str>>,
    base_labels: Box<[metrics::Label]>,
    started: metrics::Counter,
    handling_seconds: metrics::Histogram,
    handled: StatusCounters,
    failed: EnumMap<service_failure::Code, OnceLock<metrics::Counter>>,
}

impl MethodMetrics {
    fn build(path: &str, call_type: Option<CallType>) -> Self {
        let parsed = call_type.and_then(|_| crate::protocol::split_method_path(path));
        let rpc = parsed.unwrap_or(RpcName {
            service: "unknown",
            method: "unknown",
        });
        let labels = vec![
            metrics::Label::new("grpc_type", call_type.map_or("unknown", CallType::label)),
            metrics::Label::new("grpc_service", rpc.service.to_owned()),
            metrics::Label::new("grpc_method", rpc.method.to_owned()),
        ];
        Self {
            rpc_method: parsed.map(|rpc| Arc::from(rpc.to_string())),
            started: metrics::counter!(SERVER_STARTED_METRIC, labels.clone()),
            handling_seconds: metrics::histogram!(SERVER_HANDLING_METRIC, labels.clone()),
            base_labels: labels.into_boxed_slice(),
            handled: StatusCounters::new(),
            failed: EnumMap::default(),
        }
    }

    fn record_failure(&self, failure: service_failure::Code) {
        self.failed[failure]
            .get_or_init(|| {
                let mut labels = self.base_labels.to_vec();
                labels.push(metrics::Label::new("failure_code", failure.as_str()));
                metrics::counter!(SERVER_FAILURES_METRIC, labels)
            })
            .increment(1);
    }
}

impl CallMetrics for MethodMetrics {
    const IS_SERVER: bool = true;

    fn record_handled(&self, code: tonic::Code, elapsed: Duration) {
        self.handling_seconds.record(elapsed);
        self.handled.increment(code, |metric_code| {
            let mut labels = self.base_labels.to_vec();
            labels.push(metrics::Label::new("grpc_code", metric_code));
            metrics::counter!(SERVER_HANDLED_METRIC, labels)
        });
    }
}

pub(crate) struct Call(CoreCall<MethodMetrics>);

impl Call {
    pub(crate) fn start(method: &Method, path: &str) -> Self {
        let started_at = Instant::now();
        let metrics = Arc::clone(
            method
                .metrics
                .get_or_init(|| Arc::new(MethodMetrics::build(path, method.call_type))),
        );
        let span = open_rpc_span(&SpanKind::Server, metrics.rpc_method.as_ref(), path);
        metrics.started.increment(1);
        Self(CoreCall::start(metrics, span, started_at))
    }

    pub(crate) fn span(&self) -> &tracing::Span {
        self.0.span()
    }

    // template:begin authn:grpc-observe-scope-refusal
    pub(crate) fn record_insufficient_scope(&self) {
        self.0
            .span()
            .set_attribute("authz.reason", "insufficient_scope");
    }
    // template:end authn:grpc-observe-scope-refusal

    pub(crate) fn record_failure(&self, failure: service_failure::Code) {
        self.0
            .span()
            .set_attribute("failure.code", failure.as_str());
        self.0.metrics().record_failure(failure);
    }

    pub(crate) fn finish(self, code: tonic::Code) {
        self.0.finish(code);
    }
}

pub(crate) fn attach_parent_or_fallback(
    span: &tracing::Span,
    headers: &http::HeaderMap,
) -> Option<opentelemetry::Context> {
    let parent = if headers.contains_key("traceparent") {
        otel_http::extract_context(headers)
    } else {
        opentelemetry::Context::new()
    };
    set_parent_or_fallback(span, parent)
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
    let waiting = metrics::gauge!(SERVER_ADMISSION_WAITING_METRIC);
    waiting.increment(1.0);
    DropGuard::new(tokio::time::Instant::now(), move |started| {
        waiting.decrement(1.0);
        metrics::histogram!(SERVER_ADMISSION_WAIT_METRIC).record(started.elapsed());
    })
}

pub(crate) fn describe() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_wait_balances_the_gauge_on_the_original_recorder() {
        let entry = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let exit = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let wait = metrics::with_local_recorder(&entry, admission_waiting);
        metrics::with_local_recorder(&exit, || drop(wait));

        assert!(
            entry
                .handle()
                .render()
                .contains("grpc_server_admission_waiting 0")
        );
        assert!(
            exit.handle()
                .render()
                .contains("grpc_server_admission_wait_seconds")
        );
    }

    #[test]
    fn call_types_follow_the_descriptor_streaming_flags() {
        assert_eq!(CallType::new(false, false).label(), "unary");
        assert_eq!(CallType::new(true, false).label(), "client_stream");
        assert_eq!(CallType::new(false, true).label(), "server_stream");
        assert_eq!(CallType::new(true, true).label(), "bidi_stream");
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

        let path = "/example.v1.EchoService/Unary";
        let methods = crate::services::Methods::new([(path.into(), CallType::Unary)]);
        let known = Call::start(methods.resolve(path), path);
        known.finish(tonic::Code::Unauthenticated);

        let path = "/example.v1.EchoService/Invented";
        let unknown = Call::start(methods.resolve(path), path);
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
