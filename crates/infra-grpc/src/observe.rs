//! Spans and metrics. Metric names and labels follow the grpc-ecosystem
//! Prometheus convention (`go-grpc-middleware`), so standard dashboards apply.
//!
//! The span is built as the HTTP adapter's observe builds its own: only the
//! operation name and kind are `tracing` fields, because the JSON log layer
//! serializes every span field and repeats it on each record inside the call;
//! the RPC attributes go to the OpenTelemetry span alone, and nothing is
//! `Span::record`ed afterwards, since every record re-serializes the fields.
//!
//! A call is observed until its status is known. An answer that carries
//! `grpc-status` in its headers ends there; any other answer is followed
//! through its body to the trailers, so a stream that fails after its first
//! message is counted with that failure and its handling time is the whole
//! call, as grpc-go's interceptors count it.

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Once, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

use futures_util::FutureExt as _;
use opentelemetry::context::FutureExt as _;
use opentelemetry::trace::SpanKind;
use tonic::Code;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::{otel_trace_span, set_parent_or_fallback};

const SERVER_STARTED: &str = "grpc_server_started_total";
const SERVER_HANDLED: &str = "grpc_server_handled_total";
/// Server time to the call's status, a histogram by service and method.
pub const SERVER_HANDLING_SECONDS: &str = "grpc_server_handling_seconds";
const SERVER_SHED: &str = "grpc_server_shed_requests_total";
const SERVER_FAILURES: &str = "grpc_server_failures_total";
const CLIENT_STARTED: &str = "grpc_client_started_total";
const CLIENT_DIAGNOSTIC_BYTES: usize = 1024;
const CLIENT_HANDLED: &str = "grpc_client_handled_total";
/// Client time to the call's status, a histogram by service and method.
pub const CLIENT_HANDLING_SECONDS: &str = "grpc_client_handling_seconds";
/// Buckets for both handling-time histograms: the Prometheus client default
/// that `go-grpc-middleware` uses, so its dashboards' `_bucket` queries apply.
/// Without them the recorder would render a summary.
pub const HANDLING_SECONDS_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
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
    let call = Call::start(&series, &request, SpanKind::Server);
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
    .unwrap_or_else(|_panic| internal().into_http());
    let lifetime = crate::call::Lifetime {
        deadline: response.extensions_mut().remove::<crate::call::Deadline>(),
        permit: response
            .extensions_mut()
            .remove::<crate::call::Permit>()
            .and_then(crate::call::Permit::take),
        upload: None,
    };
    crate::call::attach(
        response,
        call,
        fallback,
        lifetime,
        crate::call::Side::Server,
        |_error| Code::Unknown,
    )
    .map(axum::body::Body::new)
}

/// The sanitized answer to a panic.
fn internal() -> tonic::Status {
    crate::Failure::new(service_failure::Code::InternalServerError).into()
}

/// One observed call: counted as started when created and as handled, with
/// its status code, when finished. A call dropped before that was abandoned
/// by its caller, which stopped waiting, reset the stream or stopped reading
/// it, and is recorded as `Cancelled`, as grpc-go records it.
pub(crate) struct Call {
    series: Arc<Series>,
    /// Shares the request's bytes. `None` when the path is not a label.
    uri: Option<http::Uri>,
    span: tracing::Span,
    kind: SpanKind,
    started: Instant,
    finished: bool,
}

impl Call {
    pub(crate) fn start<B>(
        series: &Arc<Series>,
        request: &http::Request<B>,
        kind: SpanKind,
    ) -> Self {
        let started = Instant::now();
        let path = series.label(request);
        let span = make_span(request, path, &kind);
        series.started(path);
        Self {
            series: Arc::clone(series),
            uri: (path != UNKNOWN_PATH).then(|| request.uri().clone()),
            span,
            kind,
            started,
            finished: false,
        }
    }

    pub(crate) fn span(&self) -> &tracing::Span {
        &self.span
    }

    /// The catalog failure behind the status: several share one status code,
    /// and during an incident which one is the whole question. Bounded by
    /// the catalog.
    pub(crate) fn failed(&self, failure: service_failure::Code) {
        self.span.set_attribute("failure.code", failure.as_str());
        self.series.failed(self.path(), failure);
    }

    pub(crate) fn finish(mut self, code: Code) {
        self.record(code);
        self.finished = true;
    }

    fn record(&self, code: Code) {
        update_span(&self.span, code, &self.kind);
        self.series
            .handled(self.path(), code, self.started.elapsed());
    }

    fn path(&self) -> &str {
        self.uri.as_ref().map_or(UNKNOWN_PATH, http::Uri::path)
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if !self.finished {
            self.record(Code::Cancelled);
        }
    }
}

/// The RPC span with its OpenTelemetry attributes. `path` is the label path,
/// so a path that is no known method names the span `unknown/unknown`.
fn make_span<B>(request: &http::Request<B>, path: &str, kind: &SpanKind) -> tracing::Span {
    let (service, method) = labels(path);
    let span = otel_trace_span!(
        "GRPC request",
        otel.name = format!("{service}/{method}"),
        otel.kind = ?kind,
    );
    span.set_attribute("rpc.system", "grpc");
    span.set_attribute("rpc.service", service.to_owned());
    span.set_attribute("rpc.method", method.to_owned());
    // Client destination identity is configured by the caller of our adapter;
    // inbound authority and User-Agent are untrusted request data.
    if matches!(kind, SpanKind::Client) {
        let (server_address, server_port) = otel_http::http_host_port(request);
        if !server_address.is_empty() && server_address.len() <= CLIENT_DIAGNOSTIC_BYTES {
            span.set_attribute("server.address", server_address.to_owned());
        }
        if let Some(port) = server_port {
            span.set_attribute("server.port", port);
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
fn update_span(span: &tracing::Span, code: Code, kind: &SpanKind) {
    span.set_attribute("rpc.grpc.status_code", i64::from(code as i32));
    if otel_http::grpc::status_is_error(code as u16, matches!(kind, SpanKind::Server)) {
        span.set_status(opentelemetry::trace::Status::error(""));
    }
}

pub(crate) fn record_shed() {
    describe();
    metrics::counter!(SERVER_SHED).increment(1);
}

/// Path whose labels are `unknown`.
const UNKNOWN_PATH: &str = "";
const CLIENT_METHOD_LIMIT: usize = 256;
const CLIENT_COMPONENT_LIMIT: usize = 256;
static CLIENT_SERIES: OnceLock<Arc<Series>> = OnceLock::new();

/// Metric handles by call path, registered on first use so a call does not
/// rebuild label strings and look its series up in the recorder. The server
/// labels the described methods of its registered services and counts every
/// other path under `unknown`, so a caller-chosen path cannot create a
/// series. All clients share a finite, never-evicted set of matching native
/// generated-method candidates. This limits observation, never routing.
pub(crate) struct Series {
    started: &'static str,
    handled: &'static str,
    handling_seconds: &'static str,
    /// Server descriptor paths; `None` selects bounded client admission.
    methods: Option<HashSet<Box<str>>>,
    paths: RwLock<HashMap<Box<str>, Handles>>,
}

struct Handles {
    started: metrics::Counter,
    handling_seconds: metrics::Histogram,
    /// Indexed by status code.
    handled: [OnceLock<metrics::Counter>; 17],
    /// Indexed by catalog code.
    failed: [OnceLock<metrics::Counter>; service_failure::Code::ALL.len()],
}

impl Series {
    pub(crate) fn server(methods: HashSet<Box<str>>) -> Self {
        Self::new(
            SERVER_STARTED,
            SERVER_HANDLED,
            SERVER_HANDLING_SECONDS,
            Some(methods),
        )
    }

    pub(crate) fn client() -> Arc<Self> {
        Arc::clone(CLIENT_SERIES.get_or_init(|| {
            Arc::new(Self::new(
                CLIENT_STARTED,
                CLIENT_HANDLED,
                CLIENT_HANDLING_SECONDS,
                None,
            ))
        }))
    }

    fn new(
        started: &'static str,
        handled: &'static str,
        handling_seconds: &'static str,
        methods: Option<HashSet<Box<str>>>,
    ) -> Self {
        describe();
        Self {
            started,
            handled,
            handling_seconds,
            methods,
            paths: RwLock::default(),
        }
    }

    /// `path` when it becomes labels, [`UNKNOWN_PATH`] otherwise.
    fn label<'a, B>(&self, request: &'a http::Request<B>) -> &'a str {
        let path = request.uri().path();
        if let Some(methods) = &self.methods {
            return if methods.contains(path) {
                path
            } else {
                UNKNOWN_PATH
            };
        }
        let Some(method) = request.extensions().get::<tonic::GrpcMethod<'static>>() else {
            return UNKNOWN_PATH;
        };
        // Validate before allocating labels. The public extension is forgeable,
        // so matching generated metadata still passes the global admission cap.
        if method.service().len() > CLIENT_COMPONENT_LIMIT
            || method.method().len() > CLIENT_COMPONENT_LIMIT
        {
            return UNKNOWN_PATH;
        }
        let Some((service, rpc)) = path.strip_prefix('/').and_then(|path| path.split_once('/'))
        else {
            return UNKNOWN_PATH;
        };
        if service.is_empty()
            || rpc.is_empty()
            || rpc.contains('/')
            || service != method.service()
            || rpc != method.method()
        {
            return UNKNOWN_PATH;
        }
        {
            let paths = self.paths.read().unwrap_or_else(PoisonError::into_inner);
            if paths.contains_key(path) {
                return path;
            }
        }
        let mut paths = self.paths.write().unwrap_or_else(PoisonError::into_inner);
        if !paths.contains_key(path) {
            let admitted = paths.len() - usize::from(paths.contains_key(UNKNOWN_PATH));
            if admitted >= CLIENT_METHOD_LIMIT {
                return UNKNOWN_PATH;
            }
            // Admission and handle registration share the write lock. Racing
            // first calls cannot allocate recorder series beyond the bound.
            paths.insert(path.into(), self.handles(path));
        }
        path
    }

    fn started(&self, path: &str) {
        self.with(path, |handles| handles.started.increment(1));
    }

    fn handled(&self, path: &str, code: Code, elapsed: Duration) {
        self.with(path, |handles| {
            handles.handling_seconds.record(elapsed.as_secs_f64());
            handles.handled[code as usize]
                .get_or_init(|| {
                    let (service, method) = labels(path);
                    metrics::counter!(
                        self.handled,
                        "grpc_service" => service.to_owned(),
                        "grpc_method" => method.to_owned(),
                        "grpc_code" => code_name(code),
                    )
                })
                .increment(1);
        });
    }

    fn failed(&self, path: &str, failure: service_failure::Code) {
        self.with(path, |handles| {
            handles.failed[failure as usize]
                .get_or_init(|| {
                    let (service, method) = labels(path);
                    metrics::counter!(
                        SERVER_FAILURES,
                        "grpc_service" => service.to_owned(),
                        "grpc_method" => method.to_owned(),
                        "failure_code" => failure.as_str(),
                    )
                })
                .increment(1);
        });
    }

    fn with(&self, path: &str, record: impl FnOnce(&Handles)) {
        {
            let paths = self.paths.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(handles) = paths.get(path) {
                return record(handles);
            }
        }
        let handles = self.handles(path);
        let mut paths = self.paths.write().unwrap_or_else(PoisonError::into_inner);
        record(paths.entry(path.into()).or_insert(handles));
    }

    fn handles(&self, path: &str) -> Handles {
        let (service, method) = labels(path);
        Handles {
            started: metrics::counter!(
                self.started,
                "grpc_service" => service.to_owned(),
                "grpc_method" => method.to_owned(),
            ),
            handling_seconds: metrics::histogram!(
                self.handling_seconds,
                "grpc_service" => service.to_owned(),
                "grpc_method" => method.to_owned(),
            ),
            handled: Default::default(),
            failed: std::array::from_fn(|_| OnceLock::new()),
        }
    }
}

impl std::fmt::Debug for Series {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Series").finish_non_exhaustive()
    }
}

fn labels(path: &str) -> (&str, &str) {
    if path == UNKNOWN_PATH {
        ("unknown", "unknown")
    } else {
        service_and_method(path)
    }
}

/// Splits `/package.Service/Method`.
fn service_and_method(path: &str) -> (&str, &str) {
    path.strip_prefix('/')
        .and_then(|path| path.split_once('/'))
        .unwrap_or(("unknown", "unknown"))
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
            "Client gRPC calls sent by service and method"
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
            "Business gRPC calls rejected without running a handler because the concurrency limit was reached"
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

/// The grpc-go `codes.Code` names used by the grpc-ecosystem metrics.
const fn code_name(code: Code) -> &'static str {
    match code {
        Code::Ok => "OK",
        Code::Cancelled => "Canceled",
        Code::Unknown => "Unknown",
        Code::InvalidArgument => "InvalidArgument",
        Code::DeadlineExceeded => "DeadlineExceeded",
        Code::NotFound => "NotFound",
        Code::AlreadyExists => "AlreadyExists",
        Code::PermissionDenied => "PermissionDenied",
        Code::ResourceExhausted => "ResourceExhausted",
        Code::FailedPrecondition => "FailedPrecondition",
        Code::Aborted => "Aborted",
        Code::OutOfRange => "OutOfRange",
        Code::Unimplemented => "Unimplemented",
        Code::Internal => "Internal",
        Code::Unavailable => "Unavailable",
        Code::DataLoss => "DataLoss",
        Code::Unauthenticated => "Unauthenticated",
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "observation fixtures fail with their setup context"
)]
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
    }

    #[test]
    fn code_names_match_grpc_go() {
        assert_eq!(code_name(Code::Cancelled), "Canceled");
        assert_eq!(code_name(Code::FailedPrecondition), "FailedPrecondition");
        assert_eq!(code_name(Code::Unauthenticated), "Unauthenticated");
    }

    /// One recorder owns the process-wide client handles for this unit-test
    /// binary. Replacing/resetting that registry for tests would evade its bound.
    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one process-wide metric registry must cover admission and terminal paths without a test-only reset"
    )]
    async fn client_observation_is_global_bounded_and_terminal_exactly_once() {
        #[allow(
            clippy::disallowed_types,
            reason = "native gRPC wire fixture uses the native path extractor; its handler retains runtime checks"
        )]
        type NativePath = axum::extract::Path<String>;
        const WAIT: Duration = Duration::from_secs(5);
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let handler_1 = |path: NativePath| async move {
            let method = path.0;
            if method == "Header" {
                return tonic::Status::new(Code::Ok, "").into_http();
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
                .header("content-type", "application/grpc")
                .body(body)
                .unwrap()
        };
        let handler_2 = || async {
            tonic::Status::invalid_argument("native generated method")
                .into_http::<tonic::body::Body>()
        };
        #[allow(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = axum::Router::new()
            .route("/test.Observation/{method}", axum::routing::post(handler_1))
            .route(
                "/example.v1.EchoService/Unary",
                axum::routing::post(handler_2),
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
            ("Header", Code::Ok),
            ("Trailer", Code::Aborted),
            ("Cancelled", Code::Cancelled),
            ("Deadline", Code::DeadlineExceeded),
        ] {
            let mut client = crate::Client::new(
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
                let status = tokio::time::timeout(WAIT, trailer_status(&mut body))
                    .await
                    .unwrap();
                assert_eq!(status, expected);
                assert!(body.frame().await.is_none());
            }
            // The cancellation case drops its still-open response here.
        }
        let adapter =
            crate::Client::new(&destination, crate::ClientSecurity::Plaintext, WAIT).unwrap();
        let mut generated =
            grpc_contracts::example::v1::echo_service_client::EchoServiceClient::new(adapter);
        let status = tokio::time::timeout(
            WAIT,
            generated.unary(tonic::Request::new(
                grpc_contracts::example::v1::UnaryRequest {
                    message: "native identity".to_owned(),
                },
            )),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(status.code(), Code::InvalidArgument);
        drop(generated);

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
                crate::Client::new(&destination, crate::ClientSecurity::Plaintext, WAIT).unwrap();
            drop(client.call(client_request(&path, method)));
        }
        let boundary: &'static str = Box::leak("b".repeat(256).into_boxed_str());
        let mut client =
            crate::Client::new(&destination, crate::ClientSecurity::Plaintext, WAIT).unwrap();
        drop(client.call(client_request(
            &format!("/{boundary}/{boundary}"),
            Some((boundary, boundary)),
        )));

        // Reconstructing the public adapter on every attempt cannot reset the
        // registry. Public GrpcMethod metadata is not an unforgeability proof.
        for index in 0..=CLIENT_METHOD_LIMIT {
            let method: &'static str = Box::leak(format!("Method{index}").into_boxed_str());
            let mut client =
                crate::Client::new(&destination, crate::ClientSecurity::Plaintext, WAIT).unwrap();
            drop(client.call(client_request(
                &format!("/bounded.Service/{method}"),
                Some(("bounded.Service", method)),
            )));
        }
        // An admitted identity is retained after saturation and reconstruction.
        let mut client =
            crate::Client::new(&destination, crate::ClientSecurity::Plaintext, WAIT).unwrap();
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
        assert_eq!(
            Series::client().paths.read().unwrap().len(),
            CLIENT_METHOD_LIMIT + 1
        );
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
        assert!(rendered.contains(r#"grpc_client_started_total{grpc_service="example.v1.EchoService",grpc_method="Unary"} 1"#), "{rendered}");
        assert!(rendered.contains(r#"grpc_client_handled_total{grpc_service="example.v1.EchoService",grpc_method="Unary",grpc_code="InvalidArgument"} 1"#), "{rendered}");
        assert!(rendered.contains(r#"grpc_client_handled_total{grpc_service="test.Observation",grpc_method="Header",grpc_code="Canceled"} 1"#), "{rendered}");
        assert!(
            rendered.contains(
                r#"grpc_client_started_total{grpc_service="unknown",grpc_method="unknown"} 12"#
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
        let mut client = crate::Client::new(
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
        assert_eq!(result.unwrap_err().code(), Code::DeadlineExceeded);
    }

    async fn trailer_status(body: &mut tonic::body::Body) -> Code {
        while let Some(frame) = body.frame().await {
            let frame = frame.unwrap();
            if let Some(trailers) = frame.trailers_ref() {
                return tonic::Status::from_header_map(trailers).unwrap().code();
            }
        }
        Code::Unknown
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
