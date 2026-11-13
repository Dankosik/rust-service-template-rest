//! Spans and metrics. Metric names and labels follow the grpc-ecosystem
//! Prometheus convention (`go-grpc-middleware`), so standard dashboards apply.
//!
//! The span is built as the HTTP adapter's observe builds its own: only the
//! operation name and kind are `tracing` fields, because the JSON log layer
//! serializes every span field and repeats it on each record inside the call;
//! the RPC attributes go to the OpenTelemetry span alone, and nothing is
//! `Span::record`ed afterwards, since every record re-serializes the fields.
//!
//! Native clients report the outcome after tonic validates the response.
//! Server calls and direct HTTP service users report at the transport boundary.

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, LazyLock, Once, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

use foldhash::{HashMap, HashSet};
use futures_util::FutureExt as _;
use opentelemetry::context::FutureExt as _;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::{otel_trace_span, set_parent_or_fallback};

use crate::call::{Observation, ResponseCustody, Side};

const SERVER_STARTED: &str = "grpc_server_started_total";
const SERVER_HANDLED: &str = "grpc_server_handled_total";
/// Server time to the call's status, a histogram by service and method.
pub(crate) const SERVER_HANDLING_SECONDS: &str = "grpc_server_handling_seconds";
const SERVER_SHED: &str = "grpc_server_shed_requests_total";
/// Time a call waited for an admission permit, a histogram without labels. It
/// is recorded only for a call that had to wait, whether the wait ended in a
/// permit, a refusal or the caller leaving.
pub(crate) const SERVER_ADMISSION_WAIT_SECONDS: &str = "grpc_server_admission_wait_seconds";
/// Calls waiting for an admission permit right now, a gauge without labels.
const SERVER_ADMISSION_WAITING: &str = "grpc_server_admission_waiting";
const SERVER_FAILURES: &str = "grpc_server_failures_total";
const CLIENT_STARTED: &str = "grpc_client_started_total";
const CLIENT_DIAGNOSTIC_BYTES: usize = 1024;
const CLIENT_HANDLED: &str = "grpc_client_handled_total";
/// Client time to the call's status, a histogram by service and method.
pub(crate) const CLIENT_HANDLING_SECONDS: &str = "grpc_client_handling_seconds";
/// Prometheus client default buckets shared by call duration and admission wait.
/// Explicit boundaries keep `go-grpc-middleware` dashboard queries against
/// `_bucket` compatible when the recorder's defaults differ.
pub const HANDLING_SECONDS_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Every histogram this crate records. A composition root hands it to the
/// Prometheus recorder; a metric left out renders the default buckets.
pub const HISTOGRAMS: &[(&str, &[f64])] = &[
    (SERVER_HANDLING_SECONDS, HANDLING_SECONDS_BUCKETS),
    (CLIENT_HANDLING_SECONDS, HANDLING_SECONDS_BUCKETS),
    (SERVER_ADMISSION_WAIT_SECONDS, HANDLING_SECONDS_BUCKETS),
];

static DESCRIBE: Once = Once::new();

/// Server span and metrics. A panicking handler, or a response stream that
/// panics, becomes a sanitized `Internal` answer, observed like any other
/// call.
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
        // The span is disabled: keep the caller's context current instead,
        // so it still reaches outbound calls.
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

/// One observed call: counted as started when created and as handled, with
/// its status code, when finished. A call dropped before that was abandoned
/// by its caller, which stopped waiting, reset the stream or stopped reading
/// it, and is recorded as `Canceled`, as grpc-go records it.
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
        destination: Option<&http::Uri>,
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

    /// The catalog failure behind the status: several share one status code,
    /// and during an incident which one is the whole question. Bounded by
    /// the catalog.
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

/// The RPC span with its OpenTelemetry attributes, named after the label
/// path's method: `unknown/unknown` for a path that is no known method.
fn make_span<B>(
    request: &http::Request<B>,
    handles: &Handles,
    side: Side,
    destination: Option<&http::Uri>,
) -> tracing::Span {
    let span = otel_trace_span!(
        "GRPC request",
        otel.name = &*handles.span_name,
        otel.kind = ?side.span_kind(),
    );
    span.set_attribute("rpc.system", "grpc");
    span.set_attribute("rpc.service", Arc::clone(&handles.service));
    span.set_attribute("rpc.method", Arc::clone(&handles.method));
    // Tonic applies the channel's authority after the adapter starts the span.
    if let Some(destination) = destination {
        if let Some(host) = destination.host()
            && !host.is_empty()
            && host.len() <= CLIENT_DIAGNOSTIC_BYTES
        {
            let address = host.trim_start_matches('[').trim_end_matches(']');
            span.set_attribute("server.address", address.to_owned());
            if let Some(port) = destination.port_u16() {
                span.set_attribute("server.port", i64::from(port));
            }
        }
        let user_agent = otel_http::user_agent(request);
        if !user_agent.is_empty() && user_agent.len() <= CLIENT_DIAGNOSTIC_BYTES {
            span.set_attribute("user_agent.original", user_agent.to_owned());
        }
    }
    span
}

/// The status code attribute, and an error status for the codes the RPC
/// semantic conventions treat as errors on that side of the call.
fn update_span(span: &tracing::Span, code: tonic::Code, side: Side) {
    span.set_attribute("rpc.grpc.status_code", i64::from(code as i32));
    if otel_http::grpc::status_is_error(code as u16, matches!(side, Side::Server)) {
        span.set_status(opentelemetry::trace::Status::error(""));
    }
}

pub(crate) fn record_shed() {
    describe();
    metrics::counter!(SERVER_SHED).increment(1);
}

/// One call waiting for an admission permit, counted in the waiting gauge and
/// recorded in the wait histogram when it is dropped, whichever way the wait
/// ends, cancellation included.
pub(crate) struct AdmissionWaiting {
    started: tokio::time::Instant,
}

impl AdmissionWaiting {
    pub(crate) fn start() -> Self {
        describe();
        metrics::gauge!(SERVER_ADMISSION_WAITING).increment(1.0);
        Self {
            started: tokio::time::Instant::now(),
        }
    }
}

impl Drop for AdmissionWaiting {
    fn drop(&mut self) {
        metrics::gauge!(SERVER_ADMISSION_WAITING).decrement(1.0);
        metrics::histogram!(SERVER_ADMISSION_WAIT_SECONDS)
            .record(self.started.elapsed().as_secs_f64());
    }
}

/// Publishes the shed counter at zero, so a scrape tells "never shed" from
/// "not exported".
pub(crate) fn register_shed() {
    describe();
    metrics::counter!(SERVER_SHED).increment(0);
}

/// Path whose labels are `unknown`.
const UNKNOWN_PATH: &str = "";
const CLIENT_METHOD_LIMIT: usize = 256;
const CLIENT_COMPONENT_LIMIT: usize = 256;
static CLIENT_SERIES: LazyLock<Arc<Series>> = LazyLock::new(|| {
    Arc::new(Series::new(Kind::Client {
        admitted: RwLock::default(),
    }))
});

/// Metric handles by call path, registered on first use so a call does not
/// rebuild label strings and look its series up in the recorder. The server
/// labels the described methods of its registered services and counts every
/// other path under `unknown`, so a caller-chosen path cannot create a
/// series. All clients share a finite, never-evicted set of matching native
/// generated-method candidates. This limits observation, never routing.
///
/// A call resolves its handles once, when it starts, and records through
/// them without another lookup.
#[derive(derive_more::Debug)]
#[debug("Series {{ .. }}")]
pub(crate) struct Series {
    started_metric: &'static str,
    handled_metric: &'static str,
    handling_seconds_metric: &'static str,
    kind: Kind,
    /// Handles of every path that is not a label.
    unknown: OnceLock<Arc<Handles>>,
}

/// Which side a [`Series`] observes, and so which paths become labels.
enum Kind {
    /// Only the described methods of the registered services. The set is
    /// fixed at startup, so finding a call's handles takes no lock.
    Server {
        described: HashMap<Box<str>, OnceLock<Arc<Handles>>>,
    },
    /// Generated-method candidates, up to [`CLIENT_METHOD_LIMIT`].
    Client {
        admitted: RwLock<HashMap<Box<str>, Arc<Handles>>>,
    },
}

/// The series of one label path.
struct Handles {
    /// `service/method`, the span name.
    span_name: Box<str>,
    service: Arc<str>,
    method: Arc<str>,
    handled_metric: &'static str,
    started: metrics::Counter,
    handling_seconds: metrics::Histogram,
    /// Indexed by status code.
    handled: [OnceLock<metrics::Counter>; 17],
    /// Indexed by catalog code.
    failed: [OnceLock<metrics::Counter>; service_failure::Code::ALL.len()],
}

impl Series {
    pub(crate) fn server(methods: HashSet<Box<str>>) -> Arc<Self> {
        Arc::new(Self::new(Kind::Server {
            described: methods
                .into_iter()
                .map(|path| (path, OnceLock::new()))
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

    /// The handles of the request's label path, `unknown` when its path is
    /// not a label.
    fn handles<B>(&self, request: &http::Request<B>) -> Arc<Handles> {
        let labelled = match &self.kind {
            Kind::Server { described } => described
                .get_key_value(request.uri().path())
                .map(|(path, handles)| Arc::clone(handles.get_or_init(|| self.register(path)))),
            Kind::Client { admitted } => {
                client_path(request).and_then(|path| self.admit_client_path(admitted, path))
            }
        };
        labelled
            .unwrap_or_else(|| Arc::clone(self.unknown.get_or_init(|| self.register(UNKNOWN_PATH))))
    }

    /// The handles of `path` while the client label set has room for it,
    /// `None` once [`CLIENT_METHOD_LIMIT`] other paths hold it.
    fn admit_client_path(
        &self,
        admitted: &RwLock<HashMap<Box<str>, Arc<Handles>>>,
        path: &str,
    ) -> Option<Arc<Handles>> {
        if let Some(handles) = admitted
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
        {
            return Some(Arc::clone(handles));
        }
        let mut paths = admitted.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(handles) = paths.get(path) {
            return Some(Arc::clone(handles));
        }
        if paths.len() >= CLIENT_METHOD_LIMIT {
            return None;
        }
        // Admission and handle registration share the write lock. Racing
        // first calls cannot allocate recorder series beyond the bound.
        let handles = self.register(path);
        paths.insert(path.into(), Arc::clone(&handles));
        Some(handles)
    }

    fn register(&self, path: &str) -> Arc<Handles> {
        let (service, method) = service_and_method(path);
        Arc::new(Handles {
            span_name: format!("{service}/{method}").into(),
            service: service.into(),
            method: method.into(),
            handled_metric: self.handled_metric,
            started: metrics::counter!(self.started_metric, call_labels(service, method)),
            handling_seconds: metrics::histogram!(
                self.handling_seconds_metric,
                call_labels(service, method)
            ),
            handled: Default::default(),
            failed: std::array::from_fn(|_| OnceLock::new()),
        })
    }
}

impl Handles {
    fn handled(&self, code: tonic::Code, elapsed: Duration) {
        self.handling_seconds.record(elapsed.as_secs_f64());
        self.handled[code as usize]
            .get_or_init(|| {
                let mut labels = call_labels(&self.service, &self.method);
                labels.push(metrics::Label::new("grpc_code", code_name(code)));
                metrics::counter!(self.handled_metric, labels)
            })
            .increment(1);
    }

    fn failed(&self, failure: service_failure::Code) {
        self.failed[failure as usize]
            .get_or_init(|| {
                let mut labels = call_labels(&self.service, &self.method);
                labels.push(metrics::Label::new("failure_code", failure.as_str()));
                metrics::counter!(SERVER_FAILURES, labels)
            })
            .increment(1);
    }
}

/// The request path when it matches the call's generated method metadata
/// within the label size bounds; validated before any label is allocated. The
/// public extension is forgeable, so a match still passes the global
/// admission cap.
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

/// Splits `/package.Service/Method`; [`UNKNOWN_PATH`] and any other path
/// outside that grammar are `unknown/unknown`.
fn service_and_method(path: &str) -> (&str, &str) {
    crate::protocol::split_method_path(path).unwrap_or(("unknown", "unknown"))
}

fn call_labels(service: &str, method: &str) -> Vec<metrics::Label> {
    vec![
        metrics::Label::new("grpc_service", service.to_owned()),
        metrics::Label::new("grpc_method", method.to_owned()),
    ]
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

/// The grpc-go code names used by the grpc-ecosystem metrics.
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
    fn paths_split_into_service_and_method() {
        assert_eq!(
            service_and_method("/example.v1.EchoService/Unary"),
            ("example.v1.EchoService", "Unary")
        );
        assert_eq!(service_and_method("no-slash"), ("unknown", "unknown"));
        assert_eq!(service_and_method(UNKNOWN_PATH), ("unknown", "unknown"));
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

    /// One recorder owns the process-wide client handles for this unit-test
    /// binary. Replacing/resetting that registry for tests would evade its bound.
    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "one process-wide metric registry must cover admission and terminal paths without a test-only reset"
    )]
    async fn client_observation_is_global_bounded_and_terminal_exactly_once() {
        // The registry pins each series to the recorder current at its first
        // use, so another test in this binary that starts a client call first
        // (`client::tests`) leaves those series outside this test's recorder.
        // nextest runs each test in its own process; `cargo test` shares one,
        // so there the test runs again alone in a child process.
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
        let server = infra_http::Server::bind(
            "127.0.0.1:0".parse().unwrap(),
            app,
            infra_http::ServerOptions {
                header_read_timeout: WAIT,
                max_header_bytes: 16 * 1024,
                max_connections: None,
                max_connection_age: None,
            },
        )
        .await
        .unwrap();
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
            // The cancellation case drops its still-open response here.
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
        // A call refused before dispatch counts with the status its caller
        // receives, not as abandoned.
        let refusing =
            crate::Client::try_new(&destination, crate::ClientSecurity::Plaintext, WAIT).unwrap();
        let status = refusing
            .prepare_call(client_request(
                "/example.v1.EchoService/Unary",
                Some(("example.v1.EchoService", "Unary")),
            ))
            .refuse(tonic::Status::unavailable("credentials unavailable"));
        assert_eq!(status.code(), tonic::Code::Unavailable);
        // An HTTP answer without `grpc-status`, as a proxy sends, counts with
        // the code a gRPC client infers from its HTTP status.
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

        // Raw paths, mismatched public extensions and extra URI segments must
        // never reserve a label, even before the admission set is saturated.
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

        // Reconstructing the public adapter on every attempt cannot reset the
        // registry. Public GrpcMethod metadata is not an unforgeability proof.
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
        // An admitted identity is retained after saturation and reconstruction.
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
        let Kind::Client { admitted } = &series.kind else {
            panic!("the client series observes the client side");
        };
        assert_eq!(admitted.read().unwrap().len(), CLIENT_METHOD_LIMIT);
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

        // Adapter entry, not the first poll, starts the opening budget.
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
