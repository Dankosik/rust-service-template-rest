use std::mem::DropGuard;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, LazyLock, Once, OnceLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use foldhash::HashMap;
use futures_util::FutureExt as _;
use opentelemetry::context::FutureExt as _;
use parking_lot::Mutex;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::{otel_trace_span, set_parent_or_fallback};

use crate::call::{Observation, ResponseCustody, Side};

const SERVER_STARTED: &str = "grpc_server_started_total";
const SERVER_HANDLED: &str = "grpc_server_handled_total";
pub(crate) const SERVER_HANDLING_SECONDS: &str = "grpc_server_handling_seconds";
const SERVER_SHED: &str = "grpc_server_shed_requests_total";
pub(crate) const SERVER_ADMISSION_WAIT_SECONDS: &str = "grpc_server_admission_wait_seconds";
const SERVER_ADMISSION_WAITING: &str = "grpc_server_admission_waiting";
const SERVER_FAILURES: &str = "grpc_server_failures_total";
const CLIENT_STARTED: &str = "grpc_client_started_total";
const CLIENT_HANDLED: &str = "grpc_client_handled_total";
pub(crate) const CLIENT_HANDLING_SECONDS: &str = "grpc_client_handling_seconds";
const DIAGNOSTIC_BYTES: usize = 1024;
const HANDLING_SECONDS_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

pub const HISTOGRAMS: &[(&str, &[f64])] = &[
    (SERVER_HANDLING_SECONDS, HANDLING_SECONDS_BUCKETS),
    (CLIENT_HANDLING_SECONDS, HANDLING_SECONDS_BUCKETS),
    (SERVER_ADMISSION_WAIT_SECONDS, HANDLING_SECONDS_BUCKETS),
];

static DESCRIBE: Once = Once::new();

pub(crate) async fn observe(
    axum::extract::State(series): axum::extract::State<Arc<Series>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let call = Call::start(&series, &request, None);
    let fallback =
        set_parent_or_fallback(call.span(), otel_http::extract_context(request.headers()));
    let answer = AssertUnwindSafe(next.run(request))
        .catch_unwind()
        .instrument(call.span().clone());
    let mut response = match &fallback {
        None => answer.await,
        Some(context) => answer.with_context(context.clone()).await,
    }
    .unwrap_or_else(|_panic| crate::status::internal_status().into_http());
    let custody = ResponseCustody::take_from_server_response(&mut response);
    crate::call::attach(
        response,
        Observation::Transport(call),
        fallback,
        custody,
        |_error| tonic::Code::Unknown,
    )
    .map(axum::body::Body::new)
}

pub(crate) struct Call {
    handles: Arc<Handles>,
    side: Side,
    span: tracing::Span,
    started: Instant,
    finished: bool,
}

impl Call {
    pub(crate) fn start<B>(
        series: &Arc<Series>,
        request: &http::Request<B>,
        destination: Option<&Destination>,
    ) -> Self {
        let started = Instant::now();
        let handles = series.handles(request);
        let span = make_span(request, &handles, series.side(), destination);
        handles.started.increment(1);
        Self {
            handles,
            side: series.side(),
            span,
            started,
            finished: false,
        }
    }

    pub(crate) fn span(&self) -> &tracing::Span {
        &self.span
    }

    pub(crate) fn side(&self) -> Side {
        self.side
    }

    pub(crate) fn failed(&self, failure: service_failure::Code) {
        self.span.set_attribute("failure.code", failure.as_str());
        self.handles.failed(failure);
    }

    pub(crate) fn finish(mut self, code: tonic::Code) {
        self.record(code);
        self.finished = true;
    }

    fn record(&self, code: tonic::Code) {
        update_span(&self.span, code, self.side);
        self.handles.handled(code, self.started.elapsed());
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if !self.finished {
            self.record(tonic::Code::Cancelled);
        }
    }
}

fn make_span<B>(
    request: &http::Request<B>,
    handles: &Handles,
    side: Side,
    destination: Option<&Destination>,
) -> tracing::Span {
    let span = otel_trace_span!(
        "GRPC request",
        otel.name = handles.span_name(),
        otel.kind = ?side.span_kind(),
    );
    span.set_attribute("rpc.system.name", "grpc");
    if let Some(method) = &handles.rpc_method {
        span.set_attribute("rpc.method", Arc::clone(method));
    } else {
        span.set_attribute("rpc.method", "_OTHER");
        let original = request.uri().path().trim_start_matches('/');
        if !original.is_empty() && original.len() <= DIAGNOSTIC_BYTES {
            span.set_attribute("rpc.method_original", original.to_owned());
        }
    }
    if let Some(destination) = destination {
        if let Some(address) = &destination.address {
            span.set_attribute("server.address", Arc::clone(address));
            if let Some(port) = destination.port {
                span.set_attribute("server.port", i64::from(port));
            }
        }
        let user_agent = otel_http::user_agent(request);
        if !user_agent.is_empty() && user_agent.len() <= DIAGNOSTIC_BYTES {
            span.set_attribute("user_agent.original", user_agent.to_owned());
        }
    }
    span
}

fn update_span(span: &tracing::Span, code: tonic::Code, side: Side) {
    let name = status_code_name(code);
    span.set_attribute("rpc.status_code", name);
    if otel_http::grpc::status_is_error(code as u16, matches!(side, Side::Server)) {
        span.set_attribute("error.type", name);
        span.set_status(opentelemetry::trace::Status::error(""));
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Destination {
    address: Option<Arc<str>>,
    port: Option<u16>,
}

impl Destination {
    pub(crate) fn new(uri: &http::Uri) -> Self {
        let address: Option<Arc<str>> = uri
            .host()
            .filter(|host| !host.is_empty() && host.len() <= DIAGNOSTIC_BYTES)
            .map(|host| {
                host.strip_prefix('[')
                    .and_then(|host| host.strip_suffix(']'))
                    .unwrap_or(host)
                    .into()
            });
        let port = address.as_ref().and_then(|_| uri.port_u16());
        Self { address, port }
    }
}

pub(crate) fn record_shed() {
    describe();
    metrics::counter!(SERVER_SHED).increment(1);
}

#[must_use]
pub(crate) fn admission_waiting() -> impl Drop {
    describe();
    metrics::gauge!(SERVER_ADMISSION_WAITING).increment(1.0);
    DropGuard::new(tokio::time::Instant::now(), |started| {
        metrics::gauge!(SERVER_ADMISSION_WAITING).decrement(1.0);
        metrics::histogram!(SERVER_ADMISSION_WAIT_SECONDS).record(started.elapsed().as_secs_f64());
    })
}

pub(crate) fn register_shed() {
    describe();
    metrics::counter!(SERVER_SHED).increment(0);
}

const UNKNOWN_PATH: &str = "";
const CLIENT_METHOD_LIMIT: usize = 256;
const CLIENT_COMPONENT_LIMIT: usize = 256;
static CLIENT_SERIES: LazyLock<Arc<Series>> = LazyLock::new(|| {
    Arc::new(Series::new(Kind::Client {
        admitted: ArcSwap::default(),
        admit_lock: Mutex::new(()),
    }))
});

#[derive(derive_more::Debug)]
#[debug("Series {{ .. }}")]
pub(crate) struct Series {
    started_metric: &'static str,
    handled_metric: &'static str,
    handling_seconds_metric: &'static str,
    kind: Kind,
    unknown: OnceLock<Arc<Handles>>,
}

enum Kind {
    Server {
        described: HashMap<Box<str>, (CallType, OnceLock<Arc<Handles>>)>,
    },
    Client {
        admitted: ArcSwap<HashMap<Box<str>, Arc<Handles>>>,
        admit_lock: Mutex<()>,
    },
}

struct Handles {
    rpc_method: Option<Arc<str>>,
    labels: Box<[metrics::Label]>,
    handled_metric: &'static str,
    started: metrics::Counter,
    handling_seconds: metrics::Histogram,
    handled: [OnceLock<metrics::Counter>; 17],
    failed: [OnceLock<metrics::Counter>; service_failure::Code::ALL.len()],
}

impl Series {
    pub(crate) fn server(methods: HashMap<Box<str>, CallType>) -> Arc<Self> {
        Arc::new(Self::new(Kind::Server {
            described: methods
                .into_iter()
                .map(|(path, call_type)| (path, (call_type, OnceLock::new())))
                .collect(),
        }))
    }

    pub(crate) fn client() -> Arc<Self> {
        Arc::clone(&CLIENT_SERIES)
    }

    fn new(kind: Kind) -> Self {
        describe();
        let (started_metric, handled_metric, handling_seconds_metric) = match &kind {
            Kind::Server { .. } => (SERVER_STARTED, SERVER_HANDLED, SERVER_HANDLING_SECONDS),
            Kind::Client { .. } => (CLIENT_STARTED, CLIENT_HANDLED, CLIENT_HANDLING_SECONDS),
        };
        Self {
            started_metric,
            handled_metric,
            handling_seconds_metric,
            kind,
            unknown: OnceLock::new(),
        }
    }

    pub(crate) const fn side(&self) -> Side {
        match self.kind {
            Kind::Server { .. } => Side::Server,
            Kind::Client { .. } => Side::Client,
        }
    }

    fn handles<B>(&self, request: &http::Request<B>) -> Arc<Handles> {
        let labelled = match &self.kind {
            Kind::Server { described } => {
                let path = request.uri().path();
                described
                    .get_key_value(path)
                    .map(|(path, (call_type, handles))| {
                        Arc::clone(handles.get_or_init(|| self.register(path, Some(*call_type))))
                    })
            }
            Kind::Client {
                admitted,
                admit_lock,
            } => client_path(request)
                .and_then(|path| self.admit_client_path(admitted, admit_lock, path)),
        };
        labelled.unwrap_or_else(|| {
            let unknown = self
                .unknown
                .get_or_init(|| self.register(UNKNOWN_PATH, None));
            Arc::clone(unknown)
        })
    }

    fn admit_client_path(
        &self,
        admitted: &ArcSwap<HashMap<Box<str>, Arc<Handles>>>,
        admit_lock: &Mutex<()>,
        path: &str,
    ) -> Option<Arc<Handles>> {
        if let Some(handles) = admitted.load().get(path) {
            return Some(Arc::clone(handles));
        }
        let _guard = admit_lock.lock();
        let current = admitted.load();
        if let Some(handles) = current.get(path) {
            return Some(Arc::clone(handles));
        }
        if current.len() >= CLIENT_METHOD_LIMIT {
            return None;
        }
        let handles = self.register(path, None);
        let mut next = (**current).clone();
        next.insert(path.into(), Arc::clone(&handles));
        admitted.store(Arc::new(next));
        Some(handles)
    }

    fn register(&self, path: &str, call_type: Option<CallType>) -> Arc<Handles> {
        let recognized = crate::protocol::split_method_path(path);
        let (service, method) = recognized.unwrap_or(("unknown", "unknown"));
        let mut labels = Vec::with_capacity(4);
        if let Kind::Server { .. } = self.kind {
            let call_type = call_type.map_or("unknown", CallType::label);
            labels.push(metrics::Label::new("grpc_type", call_type));
        }
        labels.push(metrics::Label::new("grpc_service", service.to_owned()));
        labels.push(metrics::Label::new("grpc_method", method.to_owned()));
        Arc::new(Handles {
            rpc_method: recognized.map(|(service, method)| format!("{service}/{method}").into()),
            started: metrics::counter!(self.started_metric, labels.clone()),
            handling_seconds: metrics::histogram!(self.handling_seconds_metric, labels.clone()),
            labels: labels.into_boxed_slice(),
            handled_metric: self.handled_metric,
            handled: Default::default(),
            failed: std::array::from_fn(|_| OnceLock::new()),
        })
    }
}

impl Handles {
    fn span_name(&self) -> &str {
        self.rpc_method.as_deref().unwrap_or("grpc")
    }

    fn handled(&self, code: tonic::Code, elapsed: Duration) {
        self.handling_seconds.record(elapsed.as_secs_f64());
        self.handled[code as usize]
            .get_or_init(|| {
                let mut labels = self.labels.to_vec();
                labels.push(metrics::Label::new("grpc_code", code_name(code)));
                metrics::counter!(self.handled_metric, labels)
            })
            .increment(1);
    }

    fn failed(&self, failure: service_failure::Code) {
        self.failed[failure as usize]
            .get_or_init(|| {
                let mut labels = self.labels.to_vec();
                labels.push(metrics::Label::new("failure_code", failure.as_str()));
                metrics::counter!(SERVER_FAILURES, labels)
            })
            .increment(1);
    }
}

fn client_path<B>(request: &http::Request<B>) -> Option<&str> {
    let method = request.extensions().get::<tonic::GrpcMethod<'static>>()?;
    if method.service().len() > CLIENT_COMPONENT_LIMIT
        || method.method().len() > CLIENT_COMPONENT_LIMIT
    {
        return None;
    }
    let path = request.uri().path();
    let (service, rpc) = crate::protocol::split_method_path(path)?;
    let matches = service == method.service() && rpc == method.method();
    matches.then_some(path)
}

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

fn describe() {
    DESCRIBE.call_once(|| {
        metrics::describe_counter!(
            SERVER_STARTED,
            metrics::Unit::Count,
            "Server gRPC calls received by service and method"
        );
        metrics::describe_counter!(
            CLIENT_STARTED,
            metrics::Unit::Count,
            "Client gRPC calls started by service and method, including calls that end before dispatch"
        );
        metrics::describe_counter!(
            SERVER_HANDLED,
            metrics::Unit::Count,
            "Completed server gRPC calls by service, method, and status code"
        );
        metrics::describe_histogram!(
            SERVER_HANDLING_SECONDS,
            metrics::Unit::Seconds,
            "Server gRPC time to the call's status by service and method"
        );
        metrics::describe_counter!(
            SERVER_SHED,
            metrics::Unit::Count,
            "Business gRPC calls rejected without running a handler because no concurrency permit freed within grpc.admission_wait or the call's remaining budget"
        );
        metrics::describe_gauge!(
            SERVER_ADMISSION_WAITING,
            metrics::Unit::Count,
            "Business gRPC calls waiting for a concurrency permit now"
        );
        metrics::describe_histogram!(
            SERVER_ADMISSION_WAIT_SECONDS,
            metrics::Unit::Seconds,
            "Time business gRPC calls waited for a concurrency permit, recorded only for a call that had to wait"
        );
        metrics::describe_counter!(
            SERVER_FAILURES,
            metrics::Unit::Count,
            "Server gRPC calls answered with a failure from the shared catalog by service, method, and catalog code"
        );
        metrics::describe_counter!(
            CLIENT_HANDLED,
            metrics::Unit::Count,
            "Completed client gRPC calls by service, method, and status code"
        );
        metrics::describe_histogram!(
            CLIENT_HANDLING_SECONDS,
            metrics::Unit::Seconds,
            "Client gRPC time to the call's status by service and method"
        );
    });
}

const fn status_code_name(code: tonic::Code) -> &'static str {
    match code {
        tonic::Code::Ok => "OK",
        tonic::Code::Cancelled => "CANCELLED",
        tonic::Code::Unknown => "UNKNOWN",
        tonic::Code::InvalidArgument => "INVALID_ARGUMENT",
        tonic::Code::DeadlineExceeded => "DEADLINE_EXCEEDED",
        tonic::Code::NotFound => "NOT_FOUND",
        tonic::Code::AlreadyExists => "ALREADY_EXISTS",
        tonic::Code::PermissionDenied => "PERMISSION_DENIED",
        tonic::Code::ResourceExhausted => "RESOURCE_EXHAUSTED",
        tonic::Code::FailedPrecondition => "FAILED_PRECONDITION",
        tonic::Code::Aborted => "ABORTED",
        tonic::Code::OutOfRange => "OUT_OF_RANGE",
        tonic::Code::Unimplemented => "UNIMPLEMENTED",
        tonic::Code::Internal => "INTERNAL",
        tonic::Code::Unavailable => "UNAVAILABLE",
        tonic::Code::DataLoss => "DATA_LOSS",
        tonic::Code::Unauthenticated => "UNAUTHENTICATED",
    }
}

const fn code_name(code: tonic::Code) -> &'static str {
    match code {
        tonic::Code::Ok => "OK",
        tonic::Code::Cancelled => "Canceled",
        tonic::Code::Unknown => "Unknown",
        tonic::Code::InvalidArgument => "InvalidArgument",
        tonic::Code::DeadlineExceeded => "DeadlineExceeded",
        tonic::Code::NotFound => "NotFound",
        tonic::Code::AlreadyExists => "AlreadyExists",
        tonic::Code::PermissionDenied => "PermissionDenied",
        tonic::Code::ResourceExhausted => "ResourceExhausted",
        tonic::Code::FailedPrecondition => "FailedPrecondition",
        tonic::Code::Aborted => "Aborted",
        tonic::Code::OutOfRange => "OutOfRange",
        tonic::Code::Unimplemented => "Unimplemented",
        tonic::Code::Internal => "Internal",
        tonic::Code::Unavailable => "Unavailable",
        tonic::Code::DataLoss => "DataLoss",
        tonic::Code::Unauthenticated => "Unauthenticated",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt as _;
    use tower::Service as _;

    #[test]
    fn call_types_follow_the_descriptor_streaming_flags() {
        assert_eq!(CallType::new(false, false).label(), "unary");
        assert_eq!(CallType::new(true, false).label(), "client_stream");
        assert_eq!(CallType::new(false, true).label(), "server_stream");
        assert_eq!(CallType::new(true, true).label(), "bidi_stream");
    }

    #[test]
    fn status_code_names_are_the_canonical_names() {
        assert_eq!(status_code_name(tonic::Code::Ok), "OK");
        assert_eq!(status_code_name(tonic::Code::Cancelled), "CANCELLED");
        assert_eq!(
            status_code_name(tonic::Code::DeadlineExceeded),
            "DEADLINE_EXCEEDED"
        );
        assert_eq!(
            status_code_name(tonic::Code::Unauthenticated),
            "UNAUTHENTICATED"
        );
    }

    #[test]
    fn spans_carry_the_stable_rpc_attributes_on_each_side() {
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
        let server = Series::server(
            std::iter::once(("/example.v1.EchoService/Unary".into(), CallType::Unary)).collect(),
        );
        let client = Arc::new(Series::new(Kind::Client {
            admitted: ArcSwap::default(),
            admit_lock: Mutex::new(()),
        }));
        let request = |path: &str| http::Request::builder().uri(path).body(()).unwrap();
        Call::start(&server, &request("/example.v1.EchoService/Unary"), None)
            .finish(tonic::Code::Unauthenticated);
        Call::start(&server, &request("/example.v1.EchoService/Invented"), None)
            .finish(tonic::Code::Unimplemented);
        let mut known = request("/example.v1.EchoService/Unary");
        known
            .extensions_mut()
            .insert(tonic::GrpcMethod::new("example.v1.EchoService", "Unary"));
        let destination = Destination::new(&"http://[::1]:50051".parse().unwrap());
        Call::start(&client, &known, Some(&destination)).finish(tonic::Code::Unauthenticated);
        drop(guard);

        let spans = exporter.get_finished_spans().unwrap();
        let text = |span: &opentelemetry_sdk::trace::SpanData, key: &str| {
            span.attributes
                .iter()
                .find(|attribute| attribute.key.as_str() == key)
                .map(|attribute| attribute.value.as_str().into_owned())
        };
        let [known_server, unknown_server, known_client] = spans.as_slice() else {
            panic!("three spans: {spans:?}");
        };
        for span in &spans {
            assert_eq!(text(span, "rpc.system.name").as_deref(), Some("grpc"));
            for deprecated in ["rpc.system", "rpc.service", "rpc.grpc.status_code"] {
                assert_eq!(text(span, deprecated), None, "{deprecated}");
            }
        }
        assert_eq!(known_server.name, "example.v1.EchoService/Unary");
        assert_eq!(
            text(known_server, "rpc.method").as_deref(),
            Some("example.v1.EchoService/Unary")
        );
        assert_eq!(
            text(known_server, "rpc.status_code").as_deref(),
            Some("UNAUTHENTICATED")
        );
        assert_eq!(text(known_server, "error.type"), None);
        assert_eq!(unknown_server.name, "grpc");
        assert_eq!(
            text(unknown_server, "rpc.method").as_deref(),
            Some("_OTHER")
        );
        assert_eq!(
            text(unknown_server, "rpc.method_original").as_deref(),
            Some("example.v1.EchoService/Invented")
        );
        assert_eq!(
            text(unknown_server, "error.type").as_deref(),
            Some("UNIMPLEMENTED")
        );
        assert_eq!(
            text(known_client, "error.type").as_deref(),
            Some("UNAUTHENTICATED")
        );
        assert_eq!(text(known_client, "server.address").as_deref(), Some("::1"));
        assert_eq!(text(known_client, "server.port").as_deref(), Some("50051"));
        assert_eq!(text(known_client, "rpc.method_original"), None);
    }

    #[test]
    fn a_destination_keeps_the_configured_host_without_brackets() {
        let parse = |uri: &str| Destination::new(&uri.parse().unwrap());
        let ipv6 = parse("http://[::1]:50051");
        assert_eq!(ipv6.address.as_deref(), Some("::1"));
        assert_eq!(ipv6.port, Some(50051));
        let name = parse("https://grpc.example.test");
        assert_eq!(name.address.as_deref(), Some("grpc.example.test"));
        assert_eq!(name.port, None);
    }

    #[test]
    fn code_names_match_grpc_go() {
        assert_eq!(code_name(tonic::Code::Cancelled), "Canceled");
        assert_eq!(
            code_name(tonic::Code::FailedPrecondition),
            "FailedPrecondition"
        );
        assert_eq!(code_name(tonic::Code::Unauthenticated), "Unauthenticated");
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "one process-wide metric registry must cover admission and terminal paths without a test-only reset"
    )]
    async fn client_observation_is_global_bounded_and_terminal_exactly_once() {
        const OWN_PROCESS: &str = "INFRA_GRPC_CLIENT_OBSERVATION_OWN_PROCESS";
        const WAIT: Duration = Duration::from_secs(5);
        #[expect(
            clippy::disallowed_types,
            reason = "native gRPC wire fixture uses the native path extractor; its handler retains runtime checks"
        )]
        type NativePath = axum::extract::Path<String>;
        if std::env::var("NEXTEST_EXECUTION_MODE").as_deref() != Ok("process-per-test")
            && std::env::var_os(OWN_PROCESS).is_none()
        {
            let name = format!(
                "{}::client_observation_is_global_bounded_and_terminal_exactly_once",
                module_path!().split_once("::").unwrap().1
            );
            #[expect(
                clippy::disallowed_methods,
                clippy::disallowed_types,
                reason = "cargo test fallback isolates the global metric registry in a fresh child process"
            )]
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--test-threads=1"])
                .env(OWN_PROCESS, "1")
                .output()
                .unwrap();
            assert!(
                child.status.success(),
                "{}",
                String::from_utf8_lossy(&child.stdout)
            );
            assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
            return;
        }
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let handler_1 = |path: NativePath| async move {
            let method = path.0;
            if method == "Header" {
                return tonic::Status::new(tonic::Code::Ok, "").into_http();
            }
            if method == "Proxy" {
                return http::Response::builder()
                    .status(http::StatusCode::UNAUTHORIZED)
                    .body(tonic::body::Body::empty())
                    .unwrap();
            }
            let body = if method == "Trailer" {
                let mut trailers = http::HeaderMap::new();
                tonic::Status::aborted("peer trailers")
                    .add_header(&mut trailers)
                    .unwrap();
                tonic::body::Body::new(http_body_util::StreamBody::new(futures_util::stream::iter(
                    [
                        Ok::<_, tonic::Status>(http_body::Frame::data(bytes::Bytes::from_static(
                            b"data",
                        ))),
                        Ok(http_body::Frame::trailers(trailers)),
                    ],
                )))
            } else {
                tonic::body::Body::new(http_body_util::StreamBody::new(
                    futures_util::stream::pending::<
                        Result<http_body::Frame<bytes::Bytes>, tonic::Status>,
                    >(),
                ))
            };
            http::Response::builder()
                .status(if method == "Trailer" {
                    http::StatusCode::SERVICE_UNAVAILABLE
                } else {
                    http::StatusCode::OK
                })
                .header("content-type", "application/grpc")
                .body(body)
                .unwrap()
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = axum::Router::new()
            .route("/test.Observation/{method}", axum::routing::post(handler_1))
            .route(
                "/example.v1.EchoService/Unary",
                axum::routing::post(client_status_fixture),
            )
            .route(
                "/example.v1.EchoService/ServerStream",
                axum::routing::post(client_status_fixture),
            );
        let server = infra_http::BoundListener::bind("127.0.0.1:0".parse().unwrap())
            .unwrap()
            .serve_h2(
                app,
                infra_http::ServerOptions {
                    header_read_timeout: WAIT,
                    max_header_bytes: 16 * 1024,
                    max_connections: None,
                    max_connection_age: None,
                },
                None,
            );
        let destination = format!("http://{}", server.local_addr());
        for (method, expected) in [
            ("Header", tonic::Code::Ok),
            ("Trailer", tonic::Code::Aborted),
            ("Cancelled", tonic::Code::Cancelled),
            ("Deadline", tonic::Code::DeadlineExceeded),
        ] {
            let mut client = crate::Client::try_new(
                &destination,
                crate::ClientSecurity::Plaintext,
                Duration::from_millis(200),
            )
            .unwrap();
            let request = client_request(
                &format!("/test.Observation/{method}"),
                Some(("test.Observation", method)),
            );
            let response = tokio::time::timeout(WAIT, client.call(request))
                .await
                .unwrap()
                .unwrap();
            if method == "Header" {
                assert_eq!(
                    tonic::Status::from_header_map(response.headers())
                        .unwrap()
                        .code(),
                    expected
                );
            } else if method != "Cancelled" {
                let mut body = response.into_body();
                if method == "Trailer" {
                    let data = tokio::time::timeout(WAIT, body.frame())
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap()
                        .into_data()
                        .unwrap();
                    assert_eq!(data, bytes::Bytes::from_static(b"data"));
                }
                let status = tokio::time::timeout(WAIT, trailer_status(&mut body))
                    .await
                    .unwrap();
                assert_eq!(status, expected);
                assert!(body.frame().await.is_none());
            }
        }
        let adapter =
            crate::Client::try_new(&destination, crate::ClientSecurity::Plaintext, WAIT).unwrap();
        let mut generated =
            grpc_contracts::example::v1::echo_service_client::EchoServiceClient::new(adapter);
        for (reply, expected) in [
            ("invalid-argument", tonic::Code::InvalidArgument),
            ("empty-head", tonic::Code::Internal),
            ("empty-trailers", tonic::Code::Internal),
            ("decode-error", tonic::Code::Internal),
            ("malformed-status-head", tonic::Code::Unknown),
            ("malformed-status-trailers", tonic::Code::Unknown),
            ("unsupported-encoding", tonic::Code::Unimplemented),
        ] {
            let mut request = tonic::Request::new(grpc_contracts::example::v1::UnaryRequest {
                message: "native identity".to_owned(),
            });
            request
                .metadata_mut()
                .insert("x-test-reply", reply.parse().unwrap());
            let status = tokio::time::timeout(WAIT, generated.unary(request))
                .await
                .unwrap()
                .unwrap_err();
            assert_eq!(status.code(), expected, "{reply}");
        }
        let mut request = tonic::Request::new(grpc_contracts::example::v1::ServerStreamRequest {
            message: "native streaming identity".to_owned(),
        });
        request
            .metadata_mut()
            .insert("x-test-reply", "decode-error".parse().unwrap());
        let mut stream = tokio::time::timeout(WAIT, generated.server_stream(request))
            .await
            .unwrap()
            .unwrap()
            .into_inner();
        let status = tokio::time::timeout(WAIT, stream.message())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::Internal);
        drop(stream);
        drop(generated);
        let parent = tokio_util::sync::CancellationToken::new();
        let client =
            crate::Client::try_new(&destination, crate::ClientSecurity::Plaintext, WAIT).unwrap();
        let mut generated =
            grpc_contracts::example::v1::echo_service_client::EchoServiceClient::new(client);
        let mut request = tonic::Request::new(grpc_contracts::example::v1::ServerStreamRequest {
            message: "unpolled response".to_owned(),
        });
        request
            .metadata_mut()
            .insert("x-test-reply", "held-stream".parse().unwrap());
        request
            .extensions_mut()
            .insert(operation_context::OperationContext::new(
                None,
                parent.clone(),
            ));
        let mut held = tokio::time::timeout(WAIT, generated.server_stream(request))
            .await
            .unwrap()
            .unwrap()
            .into_inner();
        parent.cancel();
        let canceled = r#"grpc_client_handled_total{grpc_service="example.v1.EchoService",grpc_method="ServerStream",grpc_code="Canceled"} 1"#;
        tokio::time::timeout(WAIT, async {
            while !handle.render().contains(canceled) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("transport cancellation must emit a metric without polling the decoded stream");
        assert_eq!(
            tokio::time::timeout(WAIT, held.message())
                .await
                .unwrap()
                .unwrap_err()
                .code(),
            tonic::Code::Cancelled
        );
        drop(held);
        let mut request = tonic::Request::new(grpc_contracts::example::v1::ServerStreamRequest {
            message: "empty streaming identity".to_owned(),
        });
        request
            .metadata_mut()
            .insert("x-test-reply", "empty-head".parse().unwrap());
        let empty = tokio::time::timeout(WAIT, generated.server_stream(request))
            .await
            .unwrap()
            .unwrap()
            .into_inner();
        let empty_ok = r#"grpc_client_handled_total{grpc_service="example.v1.EchoService",grpc_method="ServerStream",grpc_code="OK"} 1"#;
        assert!(handle.render().contains(empty_ok));
        drop(empty);
        drop(generated);
        let refusing =
            crate::Client::try_new(&destination, crate::ClientSecurity::Plaintext, WAIT).unwrap();
        let status = refusing
            .prepare_call(client_request(
                "/example.v1.EchoService/Unary",
                Some(("example.v1.EchoService", "Unary")),
            ))
            .refuse(tonic::Status::unavailable("credentials unavailable"));
        assert_eq!(status.code(), tonic::Code::Unavailable);
        let mut proxied =
            crate::Client::try_new(&destination, crate::ClientSecurity::Plaintext, WAIT).unwrap();
        let response = tokio::time::timeout(
            WAIT,
            proxied.call(client_request(
                "/test.Observation/Proxy",
                Some(("test.Observation", "Proxy")),
            )),
        )
        .await
        .unwrap()
        .unwrap();
        drop(response);

        let oversized: &'static str = Box::leak("x".repeat(257).into_boxed_str());
        for (path, method) in [
            ("/raw.Service/Chosen".to_owned(), None),
            (
                "/mismatch.Service/Chosen".to_owned(),
                Some(("other.Service", "Chosen")),
            ),
            (
                "/extra.Service/Chosen/Tail".to_owned(),
                Some(("extra.Service", "Chosen/Tail")),
            ),
            (format!("/{oversized}/Chosen"), Some((oversized, "Chosen"))),
            (
                format!("/overlong.Service/{oversized}"),
                Some(("overlong.Service", oversized)),
            ),
        ] {
            let mut client =
                crate::Client::try_new(&destination, crate::ClientSecurity::Plaintext, WAIT)
                    .unwrap();
            drop(client.call(client_request(&path, method)));
        }
        let boundary: &'static str = Box::leak("b".repeat(256).into_boxed_str());
        let mut client =
            crate::Client::try_new(&destination, crate::ClientSecurity::Plaintext, WAIT).unwrap();
        drop(client.call(client_request(
            &format!("/{boundary}/{boundary}"),
            Some((boundary, boundary)),
        )));

        for index in 0..=CLIENT_METHOD_LIMIT {
            let method: &'static str = Box::leak(format!("Method{index}").into_boxed_str());
            let mut client =
                crate::Client::try_new(&destination, crate::ClientSecurity::Plaintext, WAIT)
                    .unwrap();
            drop(client.call(client_request(
                &format!("/bounded.Service/{method}"),
                Some(("bounded.Service", method)),
            )));
        }
        let mut client =
            crate::Client::try_new(&destination, crate::ClientSecurity::Plaintext, WAIT).unwrap();
        drop(client.call(client_request(
            "/test.Observation/Header",
            Some(("test.Observation", "Header")),
        )));
        let rendered = handle.render();
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with("grpc_client_started_total{"))
                .count(),
            CLIENT_METHOD_LIMIT + 1,
            "{rendered}"
        );
        let series = Series::client();
        let Kind::Client { admitted, .. } = &series.kind else {
            panic!("the client series observes the client side");
        };
        assert_eq!(admitted.load().len(), CLIENT_METHOD_LIMIT);
        for (method, status) in [
            ("Header", "OK"),
            ("Trailer", "Aborted"),
            ("Cancelled", "Canceled"),
            ("Deadline", "DeadlineExceeded"),
        ] {
            let labels = format!(r#"grpc_service="test.Observation",grpc_method="{method}""#);
            assert!(
                rendered.contains(&format!(
                    r#"grpc_client_handled_total{{{labels},grpc_code="{status}"}} 1"#
                )),
                "{rendered}"
            );
            assert!(
                rendered.contains(&format!(
                    "grpc_client_started_total{{{labels}}} {}",
                    if method == "Header" { 2 } else { 1 }
                )),
                "{rendered}"
            );
            assert_eq!(
                rendered
                    .lines()
                    .filter(
                        |line| line.starts_with(&format!("grpc_client_handled_total{{{labels},"))
                    )
                    .count(),
                if method == "Header" { 2 } else { 1 },
                "{rendered}"
            );
        }
        let unary = r#"grpc_service="example.v1.EchoService",grpc_method="Unary""#;
        assert!(
            rendered.contains(&format!("grpc_client_started_total{{{unary}}} 8")),
            "{rendered}"
        );
        let handled_prefix = format!("grpc_client_handled_total{{{unary},");
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with(&handled_prefix))
                .count(),
            5,
            "{rendered}"
        );
        for (code, count) in [
            ("InvalidArgument", 1),
            ("Internal", 3),
            ("Unknown", 2),
            ("Unimplemented", 1),
            ("Unavailable", 1),
        ] {
            assert!(
                rendered.contains(&format!(r#"{handled_prefix}grpc_code="{code}"}} {count}"#)),
                "{rendered}"
            );
        }
        assert!(rendered.contains(r#"grpc_client_handled_total{grpc_service="example.v1.EchoService",grpc_method="ServerStream",grpc_code="Internal"} 1"#), "{rendered}");
        assert!(rendered.contains(canceled), "{rendered}");
        assert!(rendered.contains(empty_ok), "{rendered}");
        assert!(rendered.contains(r#"grpc_client_handled_total{grpc_service="test.Observation",grpc_method="Header",grpc_code="Canceled"} 1"#), "{rendered}");
        assert!(rendered.contains(r#"grpc_client_handled_total{grpc_service="test.Observation",grpc_method="Proxy",grpc_code="Unauthenticated"} 1"#), "{rendered}");
        assert!(
            rendered.contains(
                r#"grpc_client_started_total{grpc_service="unknown",grpc_method="unknown"} 14"#
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!(
                r#"grpc_service="{boundary}",grpc_method="{boundary}""#
            )),
            "{rendered}"
        );
        for forbidden in [
            "raw.Service",
            "mismatch.Service",
            "other.Service",
            "extra.Service",
            "overlong.Service",
            oversized,
            "Method256",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "unexpected client identity {forbidden}: {rendered}"
            );
        }
        drop(client);
        assert_eq!(
            tokio::time::timeout(WAIT, server.drain(Duration::from_secs(1)))
                .await
                .unwrap()
                .unwrap(),
            infra_http::Drained::Complete
        );

        tokio::time::pause();
        let mut client = crate::Client::try_new(
            "http://127.0.0.1:1",
            crate::ClientSecurity::Plaintext,
            Duration::from_secs(1),
        )
        .unwrap();
        let pending = client.call(client_request("/unpolled.Service/Unary", None));
        tokio::time::advance(Duration::from_secs(1)).await;
        let result = tokio::time::timeout(Duration::ZERO, pending)
            .await
            .expect("expired call answers without channel progress");
        assert_eq!(result.unwrap_err().code(), tonic::Code::DeadlineExceeded);
    }

    #[allow(
        clippy::unused_async,
        reason = "axum's wire fixture handler must return a future"
    )]
    async fn client_status_fixture(headers: http::HeaderMap) -> http::Response<tonic::body::Body> {
        use http_body::Frame;
        use tonic::body::Body;

        let reply = headers["x-test-reply"].to_str().unwrap();
        let trailers = |code, message| {
            let mut trailers = http::HeaderMap::new();
            trailers.insert("grpc-status", http::HeaderValue::from_static(code));
            if let Some(message) = message {
                trailers.insert("grpc-message", http::HeaderValue::from_static(message));
            }
            Frame::trailers(trailers)
        };
        let frames = |frame| {
            Body::new(http_body_util::StreamBody::new(futures_util::stream::iter(
                [Ok::<_, tonic::Status>(frame)],
            )))
        };
        let mut response = http::Response::builder().header("content-type", "application/grpc");
        let body = match reply {
            "invalid-argument" => {
                return tonic::Status::invalid_argument("native generated method").into_http();
            }
            "empty-head" => {
                response = response.header("grpc-status", "0");
                Body::empty()
            }
            "empty-trailers" => frames(trailers("0", None)),
            "decode-error" => frames(Frame::data(bytes::Bytes::from_static(&[2, 0, 0, 0, 0]))),
            "held-stream" => Body::new(http_body_util::StreamBody::new(
                futures_util::stream::pending::<Result<Frame<bytes::Bytes>, tonic::Status>>(),
            )),
            "malformed-status-head" => {
                response = response
                    .header("grpc-status", "14")
                    .header("grpc-message", "%FF");
                Body::empty()
            }
            "malformed-status-trailers" => frames(trailers("14", Some("%FF"))),
            "unsupported-encoding" => {
                response = response
                    .header("grpc-status", "14")
                    .header("grpc-encoding", "gzip");
                Body::empty()
            }
            _ => panic!("unknown test reply {reply}"),
        };
        response.body(body).unwrap()
    }

    async fn trailer_status(body: &mut tonic::body::Body) -> tonic::Code {
        while let Some(frame) = body.frame().await {
            let frame = frame.unwrap();
            if let Some(trailers) = frame.trailers_ref() {
                return tonic::Status::from_header_map(trailers).unwrap().code();
            }
        }
        tonic::Code::Unknown
    }

    fn client_request(
        path: &str,
        method: Option<(&'static str, &'static str)>,
    ) -> http::Request<tonic::body::Body> {
        let mut request = http::Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/grpc")
            .body(tonic::body::Body::empty())
            .unwrap();
        if let Some((service, method)) = method {
            request
                .extensions_mut()
                .insert(tonic::GrpcMethod::new(service, method));
        }
        request
    }
}
