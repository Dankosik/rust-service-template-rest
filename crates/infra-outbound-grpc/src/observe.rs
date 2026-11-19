use std::sync::{Arc, LazyLock, Once, OnceLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use foldhash::HashMap;
use metrics::SharedString;
use opentelemetry::trace::{SpanKind, Status as SpanStatus};
use parking_lot::Mutex;
use shared_rules::grpc::{RpcName, STATUS_CODE_COUNT, split_method_path, status_code_names};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::otel_trace_span;

pub(crate) const CLIENT_STARTED_METRIC: &str = "grpc_client_started_total";
pub(crate) const CLIENT_HANDLED_METRIC: &str = "grpc_client_handled_total";
pub(crate) const CLIENT_HANDLING_METRIC: &str = "grpc_client_handling_seconds";

const DIAGNOSTIC_BYTES: usize = 1024;
const CLIENT_METHOD_LIMIT: usize = 256;
const CLIENT_COMPONENT_LIMIT: usize = 256;
const HANDLING_SECONDS_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

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
        let address = host
            .strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(host);
        Some(Self {
            address: Arc::from(address),
            port: uri.port_u16(),
        })
    }
}

pub(crate) struct Call {
    metrics: Arc<MethodMetrics>,
    span: tracing::Span,
    started: Instant,
    finished: bool,
}

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
        Self {
            metrics,
            span,
            started,
            finished: false,
        }
    }

    pub(crate) fn span(&self) -> &tracing::Span {
        &self.span
    }

    pub(crate) fn finish(mut self, code: tonic::Code) {
        self.record(code);
        self.finished = true;
    }

    pub(crate) fn refuse(self, status: tonic::Status) -> tonic::Status {
        self.finish(status.code());
        status
    }

    fn record(&self, code: tonic::Code) {
        let span_code = status_code_names(code as usize).span;
        self.span.set_attribute("rpc.status_code", span_code);
        if otel_http::grpc::status_is_error(code as u16, false) {
            self.span.set_attribute("error.type", span_code);
            self.span.set_status(SpanStatus::error(""));
        }
        self.metrics.handled(code, self.started.elapsed());
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if !self.finished {
            self.record(tonic::Code::Cancelled);
        }
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
    span.set_attribute("rpc.system.name", "grpc");
    if let Some(method) = &metrics.rpc_method {
        span.set_attribute("rpc.method", Arc::clone(method));
    } else {
        span.set_attribute("rpc.method", "_OTHER");
        let original = request.uri().path().trim_start_matches('/');
        if !original.is_empty() && original.len() <= DIAGNOSTIC_BYTES {
            span.set_attribute("rpc.method_original", original.to_owned());
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
        span.set_attribute("user_agent.original", user_agent.to_owned());
    }
    span
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
        if let Some((path, rpc)) = verified_client_path(request)
            && let Some(admitted) = self.admit_path(path, rpc)
        {
            return admitted;
        }
        Arc::clone(
            self.unknown
                .get_or_init(|| Arc::new(MethodMetrics::unknown())),
        )
    }

    fn admit_path(&self, path: &str, rpc: RpcName<'_>) -> Option<Arc<MethodMetrics>> {
        let snapshot = self.admitted.load();
        if let Some(existing) = snapshot.get(path) {
            return Some(Arc::clone(existing));
        }
        drop(snapshot);
        let _guard = self.admit_lock.lock();
        let current = self.admitted.load();
        if let Some(existing) = current.get(path) {
            return Some(Arc::clone(existing));
        }
        if current.len() >= CLIENT_METHOD_LIMIT {
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
        self.rpc_method.as_deref().unwrap_or("grpc")
    }

    fn handled(&self, code: tonic::Code, elapsed: Duration) {
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

fn verified_client_path<B>(request: &http::Request<B>) -> Option<(&str, RpcName<'_>)> {
    let declared = request.extensions().get::<tonic::GrpcMethod<'static>>()?;
    if declared.service().len() > CLIENT_COMPONENT_LIMIT
        || declared.method().len() > CLIENT_COMPONENT_LIMIT
    {
        return None;
    }
    let path = request.uri().path();
    let rpc = split_method_path(path)?;
    (rpc.service == declared.service() && rpc.method == declared.method()).then_some((path, rpc))
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
        drop(guard);

        let spans = exporter.get_finished_spans().unwrap();
        let [span] = spans.as_slice() else {
            panic!("one span: {spans:?}");
        };
        let text = |key: &str| {
            span.attributes
                .iter()
                .find(|attribute| attribute.key.as_str() == key)
                .map(|attribute| attribute.value.as_str().into_owned())
        };
        assert_eq!(span.name, "example.v1.EchoService/Unary");
        assert_eq!(text("rpc.system.name").as_deref(), Some("grpc"));
        for deprecated in ["rpc.system", "rpc.service", "rpc.grpc.status_code"] {
            assert_eq!(text(deprecated), None, "{deprecated}");
        }
        assert_eq!(
            text("rpc.method").as_deref(),
            Some("example.v1.EchoService/Unary")
        );
        assert_eq!(text("rpc.method_original"), None);
        assert_eq!(text("rpc.status_code").as_deref(), Some("UNAUTHENTICATED"));
        assert_eq!(text("error.type").as_deref(), Some("UNAUTHENTICATED"));
        assert_eq!(text("server.address").as_deref(), Some("::1"));
        assert_eq!(text("server.port").as_deref(), Some("50051"));
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
