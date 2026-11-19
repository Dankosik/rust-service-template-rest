use std::mem::DropGuard;
use std::sync::{Arc, LazyLock, Once, OnceLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use foldhash::HashMap;
use metrics::SharedString;
use opentelemetry::Value;
use opentelemetry::trace::{SpanKind, Status as SpanStatus};
use parking_lot::Mutex;
use shared_rules::grpc::{
    DIAGNOSTIC_BYTES, ERROR_TYPE_ATTR, FALLBACK_SPAN_NAME, HANDLING_SECONDS_BUCKETS,
    RPC_METHOD_ATTR, RPC_METHOD_ORIGINAL_ATTR, RPC_METHOD_OTHER, RPC_STATUS_CODE_ATTR,
    RPC_SYSTEM_GRPC, RPC_SYSTEM_NAME_ATTR, RpcName, STATUS_CODE_COUNT,
    diagnostic_rpc_method_original, split_method_path, status_code_names,
};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::otel_trace_span;

pub(crate) const CLIENT_STARTED_METRIC: &str = "grpc_client_started_total";
pub(crate) const CLIENT_HANDLED_METRIC: &str = "grpc_client_handled_total";
pub(crate) const CLIENT_HANDLING_METRIC: &str = "grpc_client_handling_seconds";

const MAX_ADMITTED_METHODS: usize = 256;
const MAX_RPC_NAME_PART_BYTES: usize = 256;
const MAX_RPC_PATH_BYTES: usize = 2 * MAX_RPC_NAME_PART_BYTES + 2;

pub const HISTOGRAMS: &[(&str, &[f64])] = &[(CLIENT_HANDLING_METRIC, HANDLING_SECONDS_BUCKETS)];

static DESCRIBE: Once = Once::new();
static CLIENT_SERIES: LazyLock<Series> = LazyLock::new(Series::new);

#[derive(Clone, Debug)]
pub(crate) struct Destination {
    address: Arc<str>,
    port: Option<u16>,
}

impl Destination {
    pub(crate) fn from_uri(uri: &http::Uri) -> Option<Self> {
        let host = uri
            .host()
            .filter(|host| !host.is_empty() && host.len() <= DIAGNOSTIC_BYTES)?;
        let address = crate::protocol::unbracket_host(host);
        Some(Self {
            address: Arc::from(address),
            port: uri.port_u16(),
        })
    }
}

struct CallInner {
    metrics: Arc<MethodMetrics>,
    span: tracing::Span,
    started: Instant,
}

impl CallInner {
    fn record(&self, code: tonic::Code) {
        let span_code = status_code_names(code as usize).span;
        self.span.set_attribute(RPC_STATUS_CODE_ATTR, span_code);
        if otel_http::grpc::status_is_error(code as u16, false) {
            self.span.set_attribute(ERROR_TYPE_ATTR, span_code);
            self.span.set_status(SpanStatus::error(""));
        }
        self.metrics.record_handled(code, self.started.elapsed());
    }

    fn cancel(self) {
        self.record(tonic::Code::Cancelled);
    }
}

pub(crate) struct Call(DropGuard<CallInner, fn(CallInner)>);

impl Call {
    pub(crate) fn start<B>(
        series: &Series,
        request: &http::Request<B>,
        destination: Option<&Destination>,
    ) -> Self {
        let started = Instant::now();
        let metrics = series.resolve(request);
        let span = make_client_span(request, &metrics, destination);
        metrics.started.increment(1);
        Self(DropGuard::new(
            CallInner {
                metrics,
                span,
                started,
            },
            CallInner::cancel,
        ))
    }

    pub(crate) fn span(&self) -> &tracing::Span {
        &self.0.span
    }

    pub(crate) fn finish(self, code: tonic::Code) {
        DropGuard::dismiss(self.0).record(code);
    }

    pub(crate) fn refuse(self, status: tonic::Status) -> tonic::Status {
        self.finish(status.code());
        status
    }
}

fn make_client_span<B>(
    request: &http::Request<B>,
    metrics: &MethodMetrics,
    destination: Option<&Destination>,
) -> tracing::Span {
    let span = otel_trace_span!(
        "GRPC request",
        otel.name = metrics.span_name(),
        otel.kind = ?SpanKind::Client,
    );
    span.set_attribute(RPC_SYSTEM_NAME_ATTR, RPC_SYSTEM_GRPC);
    if let Some(method) = &metrics.rpc_method {
        span.set_attribute(RPC_METHOD_ATTR, Arc::clone(method));
    } else {
        span.set_attribute(RPC_METHOD_ATTR, RPC_METHOD_OTHER);
        if let Some(original) = diagnostic_rpc_method_original(request.uri().path()) {
            span.set_attribute(RPC_METHOD_ORIGINAL_ATTR, DiagnosticAttribute(original));
        }
    }
    if let Some(destination) = destination {
        span.set_attribute("server.address", Arc::clone(&destination.address));
        if let Some(port) = destination.port {
            span.set_attribute("server.port", i64::from(port));
        }
    }
    let user_agent = otel_http::user_agent(request);
    if !user_agent.is_empty() && user_agent.len() <= DIAGNOSTIC_BYTES {
        span.set_attribute("user_agent.original", DiagnosticAttribute(user_agent));
    }
    span
}

struct DiagnosticAttribute<'a>(&'a str);

impl From<DiagnosticAttribute<'_>> for Value {
    fn from(attribute: DiagnosticAttribute<'_>) -> Self {
        attribute.0.to_owned().into()
    }
}

#[derive(derive_more::Debug)]
#[debug("Series {{ .. }}")]
pub(crate) struct Series {
    admitted: ArcSwap<HashMap<Box<str>, Arc<MethodMetrics>>>,
    admit_lock: Mutex<()>,
    unknown: OnceLock<Arc<MethodMetrics>>,
}

struct MethodMetrics {
    rpc_method: Option<Arc<str>>,
    service: SharedString,
    method: SharedString,
    started: metrics::Counter,
    handling_seconds: metrics::Histogram,
    handled: [OnceLock<metrics::Counter>; STATUS_CODE_COUNT],
}

impl Series {
    pub(crate) fn client() -> &'static Self {
        LazyLock::force(&CLIENT_SERIES)
    }

    fn new() -> Self {
        describe_client_metrics();
        Self {
            admitted: ArcSwap::default(),
            admit_lock: Mutex::new(()),
            unknown: OnceLock::new(),
        }
    }

    fn resolve<B>(&self, request: &http::Request<B>) -> Arc<MethodMetrics> {
        if let Some(declared) = declared_method(request)
            && let Some(admitted) = self.admit_path(request.uri().path(), declared)
        {
            return admitted;
        }
        Arc::clone(
            self.unknown
                .get_or_init(|| Arc::new(MethodMetrics::unknown())),
        )
    }

    fn admit_path(&self, path: &str, declared: RpcName<'_>) -> Option<Arc<MethodMetrics>> {
        if path.len() > MAX_RPC_PATH_BYTES {
            return None;
        }
        let snapshot = self.admitted.load();
        if let Some(existing) = snapshot.get(path) {
            return (existing.service.as_ref() == declared.service
                && existing.method.as_ref() == declared.method)
                .then(|| Arc::clone(existing));
        }
        if snapshot.len() >= MAX_ADMITTED_METHODS {
            return None;
        }
        drop(snapshot);
        let rpc = split_method_path(path).filter(|rpc| *rpc == declared)?;
        let _guard = self.admit_lock.lock();
        let current = self.admitted.load();
        if let Some(existing) = current.get(path) {
            return Some(Arc::clone(existing));
        }
        if current.len() >= MAX_ADMITTED_METHODS {
            return None;
        }
        let metrics = Arc::new(MethodMetrics::for_method(rpc));
        let mut next = (**current).clone();
        next.insert(path.into(), Arc::clone(&metrics));
        self.admitted.store(Arc::new(next));
        Some(metrics)
    }
}

impl MethodMetrics {
    fn for_method(rpc: RpcName<'_>) -> Self {
        let rpc_method: Arc<str> = rpc.to_string().into();
        Self::build(
            Some(rpc_method),
            SharedString::from(Arc::<str>::from(rpc.service)),
            SharedString::from(Arc::<str>::from(rpc.method)),
        )
    }

    fn unknown() -> Self {
        Self::build(
            None,
            SharedString::const_str("unknown"),
            SharedString::const_str("unknown"),
        )
    }

    fn build(rpc_method: Option<Arc<str>>, service: SharedString, method: SharedString) -> Self {
        let started = metrics::counter!(
            CLIENT_STARTED_METRIC,
            "grpc_service" => service.clone(),
            "grpc_method" => method.clone(),
        );
        let handling_seconds = metrics::histogram!(
            CLIENT_HANDLING_METRIC,
            "grpc_service" => service.clone(),
            "grpc_method" => method.clone(),
        );
        Self {
            rpc_method,
            service,
            method,
            started,
            handling_seconds,
            handled: Default::default(),
        }
    }

    fn span_name(&self) -> &str {
        self.rpc_method.as_deref().unwrap_or(FALLBACK_SPAN_NAME)
    }

    fn record_handled(&self, code: tonic::Code, elapsed: Duration) {
        self.handling_seconds.record(elapsed);
        self.handled[code as usize]
            .get_or_init(|| {
                metrics::counter!(
                    CLIENT_HANDLED_METRIC,
                    "grpc_service" => self.service.clone(),
                    "grpc_method" => self.method.clone(),
                    "grpc_code" => status_code_names(code as usize).metric,
                )
            })
            .increment(1);
    }
}

fn declared_method<B>(request: &http::Request<B>) -> Option<RpcName<'_>> {
    let declared = request.extensions().get::<tonic::GrpcMethod<'static>>()?;
    let service = declared.service();
    let method = declared.method();
    (service.len() <= MAX_RPC_NAME_PART_BYTES && method.len() <= MAX_RPC_NAME_PART_BYTES)
        .then_some(RpcName { service, method })
}

fn describe_client_metrics() {
    DESCRIBE.call_once(|| {
        metrics::describe_counter!(
            CLIENT_STARTED_METRIC,
            metrics::Unit::Count,
            "Client gRPC calls started by service and method, including calls that end before dispatch"
        );
        metrics::describe_counter!(
            CLIENT_HANDLED_METRIC,
            metrics::Unit::Count,
            "Completed client gRPC calls by service, method, and status code"
        );
        metrics::describe_histogram!(
            CLIENT_HANDLING_METRIC,
            metrics::Unit::Seconds,
            "Client gRPC time to the call's status by service and method"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_request(
        path: &str,
        declared: Option<(&'static str, &'static str)>,
    ) -> http::Request<()> {
        let mut request = http::Request::builder()
            .method("POST")
            .uri(path)
            .body(())
            .unwrap();
        if let Some((service, method)) = declared {
            request
                .extensions_mut()
                .insert(tonic::GrpcMethod::new(service, method));
        }
        request
    }

    #[test]
    fn a_client_span_carries_the_stable_rpc_attributes() {
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
        let series = Series::new();
        let known = test_request(
            "/example.v1.EchoService/Unary",
            Some(("example.v1.EchoService", "Unary")),
        );
        let destination = Destination::from_uri(&"http://[::1]:50051".parse().unwrap());
        Call::start(&series, &known, destination.as_ref()).finish(tonic::Code::Unauthenticated);
        let mut raw = test_request("/example.v1.Raw/Unary", None);
        raw.headers_mut().insert(
            http::header::USER_AGENT,
            "grpc-client-test".parse().unwrap(),
        );
        Call::start(&series, &raw, None).finish(tonic::Code::Ok);
        let overlong = "a".repeat(DIAGNOSTIC_BYTES + 1);
        let mut raw = test_request(&format!("/{overlong}"), None);
        raw.headers_mut()
            .insert(http::header::USER_AGENT, overlong.parse().unwrap());
        Call::start(&series, &raw, None).finish(tonic::Code::Ok);
        drop(guard);

        let spans = exporter.get_finished_spans().unwrap();
        let [known, raw, overlong] = spans.as_slice() else {
            panic!("three spans: {spans:?}");
        };
        let text = |span: &opentelemetry_sdk::trace::SpanData, key: &str| {
            span.attributes
                .iter()
                .find(|attribute| attribute.key.as_str() == key)
                .map(|attribute| attribute.value.as_str().into_owned())
        };
        assert_eq!(known.name, "example.v1.EchoService/Unary");
        assert_eq!(text(known, "rpc.system.name").as_deref(), Some("grpc"));
        for deprecated in ["rpc.system", "rpc.service", "rpc.grpc.status_code"] {
            assert_eq!(text(known, deprecated), None, "{deprecated}");
        }
        assert_eq!(
            text(known, "rpc.method").as_deref(),
            Some("example.v1.EchoService/Unary")
        );
        assert_eq!(text(known, "rpc.method_original"), None);
        assert_eq!(
            text(known, "rpc.status_code").as_deref(),
            Some("UNAUTHENTICATED")
        );
        assert_eq!(
            text(known, "error.type").as_deref(),
            Some("UNAUTHENTICATED")
        );
        assert_eq!(text(known, "server.address").as_deref(), Some("::1"));
        assert_eq!(text(known, "server.port").as_deref(), Some("50051"));
        assert_eq!(
            text(raw, "rpc.method_original").as_deref(),
            Some("example.v1.Raw/Unary")
        );
        assert_eq!(
            text(raw, "user_agent.original").as_deref(),
            Some("grpc-client-test")
        );
        assert_eq!(text(overlong, "rpc.method_original"), None);
        assert_eq!(text(overlong, "user_agent.original"), None);
    }

    #[test]
    fn an_admitted_path_still_requires_both_declared_labels() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let series = Series::new();
        for declared in [
            Some(("example.v1.EchoService", "Unary")),
            None,
            Some(("example.v1.OtherService", "Unary")),
            Some(("example.v1.EchoService", "Other")),
            Some(("example.v1.EchoService", "Unary")),
        ] {
            let request = test_request("/example.v1.EchoService/Unary", declared);
            Call::start(&series, &request, None).finish(tonic::Code::Ok);
        }
        let rendered = recorder.handle().render();
        assert!(rendered.lines().any(|line| line
            == r#"grpc_client_started_total{grpc_service="example.v1.EchoService",grpc_method="Unary"} 2"#));
        assert!(rendered.lines().any(|line| line
            == r#"grpc_client_started_total{grpc_service="unknown",grpc_method="unknown"} 3"#));
    }

    #[test]
    fn destination_strips_ipv6_brackets_and_caps_overlong_hosts() {
        let ipv6 = Destination::from_uri(&"https://[::1]:50051".parse().unwrap()).unwrap();
        assert_eq!(ipv6.address.as_ref(), "::1");
        assert_eq!(ipv6.port, Some(50051));

        let named = Destination::from_uri(&"https://grpc.example.test".parse().unwrap()).unwrap();
        assert_eq!(named.address.as_ref(), "grpc.example.test");
        assert_eq!(named.port, None);

        let overlong_uri: http::Uri =
            format!("https://{}.test:50051", "a".repeat(DIAGNOSTIC_BYTES))
                .parse()
                .unwrap();
        assert!(Destination::from_uri(&overlong_uri).is_none());
    }

    #[test]
    fn code_labels_match_otel_and_prometheus_conventions() {
        use tonic::Code;
        for (code, span, metric) in [
            (Code::Ok, "OK", "OK"),
            (Code::Cancelled, "CANCELLED", "Canceled"),
            (
                Code::DeadlineExceeded,
                "DEADLINE_EXCEEDED",
                "DeadlineExceeded",
            ),
            (
                Code::FailedPrecondition,
                "FAILED_PRECONDITION",
                "FailedPrecondition",
            ),
            (Code::Unauthenticated, "UNAUTHENTICATED", "Unauthenticated"),
        ] {
            let names = status_code_names(code as usize);
            assert_eq!(names.span, span);
            assert_eq!(names.metric, metric);
        }
        for raw in 0..17 {
            let names = status_code_names(Code::from_i32(raw) as usize);
            assert!(!names.span.is_empty() && !names.metric.is_empty());
        }
    }
}
