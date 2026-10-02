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
use std::pin::Pin;
use std::sync::{Arc, Once, OnceLock, PoisonError, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::FutureExt as _;
use http::HeaderMap;
use http_body::{Body, Frame, SizeHint};
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
    let response = match fallback {
        None => answer.await,
        // The span is disabled: keep the caller's context current instead,
        // so it still reaches outbound calls.
        Some(context) => answer.with_context(context).await,
    }
    .unwrap_or_else(|_panic| internal().into_http());
    // tonic keeps the status of a Trailers-Only answer in the extensions.
    if let Some(failure) = response
        .extensions()
        .get::<tonic::Status>()
        .and_then(crate::status::catalog_code)
    {
        call.failed(failure);
    }
    call.until_status(response, |body, call| {
        let unknown = |_error: &axum::Error| Code::Unknown;
        axum::body::Body::new(Observed::new(body, call, Recover::Panics, unknown))
    })
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
        let path = series.label(request.uri().path());
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
    fn failed(&self, failure: service_failure::Code) {
        self.span.set_attribute("failure.code", failure.as_str());
        self.series.failed(self.path(), failure);
    }

    pub(crate) fn finish(mut self, code: Code) {
        self.record(code);
        self.finished = true;
    }

    /// Finishes the call when its status is known: now, when the response
    /// headers carry it (a Trailers-Only answer), and otherwise when the
    /// body yields its trailers. Only that second answer is handed to
    /// `follow`, which wraps its body in an [`Observed`].
    pub(crate) fn until_status<B: Body>(
        self,
        response: http::Response<B>,
        follow: impl FnOnce(B, Self) -> B,
    ) -> http::Response<B> {
        match status(response.headers()) {
            Some(code) => self.finish(code),
            // No status and no body left to carry one.
            None if response.body().is_end_stream() => self.finish(Code::Unknown),
            None => return response.map(|body| follow(body, self)),
        }
        response
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
    let (server_address, server_port) = otel_http::http_host_port(request);
    span.set_attribute("rpc.system", "grpc");
    span.set_attribute("rpc.service", service.to_owned());
    span.set_attribute("rpc.method", method.to_owned());
    if !server_address.is_empty() {
        span.set_attribute("server.address", server_address.to_owned());
    }
    if let Some(port) = server_port {
        span.set_attribute("server.port", port);
    }
    let user_agent = otel_http::user_agent(request);
    if !user_agent.is_empty() {
        span.set_attribute("user_agent.original", user_agent.to_owned());
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

/// Whether a response body that panics is answered or left to unwind.
#[derive(Clone, Copy)]
pub(crate) enum Recover {
    /// The server: the caller gets `Internal` trailers.
    Panics,
    /// The client: tonic's own channel body does not run handler code.
    Nothing,
}

/// A response body that finishes its [`Call`] with the status in the
/// trailers. A body dropped before them leaves the call to its drop guard.
pub(crate) struct Observed<B: Body> {
    /// `None` once the body ended or panicked; it is not polled again.
    inner: Option<B>,
    /// `None` once the call is finished.
    call: Option<Call>,
    recover: Recover,
    error_code: fn(&B::Error) -> Code,
}

impl<B: Body> Observed<B> {
    /// `error_code` is the status of a body that fails instead of ending.
    pub(crate) fn new(
        inner: B,
        call: Call,
        recover: Recover,
        error_code: fn(&B::Error) -> Code,
    ) -> Self {
        Self {
            inner: Some(inner),
            call: Some(call),
            recover,
            error_code,
        }
    }

    fn finish(&mut self, code: Code) {
        if let Some(call) = self.call.take() {
            call.finish(code);
        }
    }
}

impl<B> Body for Observed<B>
where
    B: Body<Data = Bytes> + Unpin,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, B::Error>>> {
        let this = self.get_mut();
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        let polled = match this.recover {
            Recover::Nothing => Pin::new(inner).poll_frame(context),
            Recover::Panics => {
                let poll = AssertUnwindSafe(|| Pin::new(inner).poll_frame(context));
                match std::panic::catch_unwind(poll) {
                    Ok(polled) => polled,
                    Err(_panic) => {
                        this.inner = None;
                        if let Some(call) = &this.call {
                            call.failed(service_failure::Code::InternalServerError);
                        }
                        this.finish(Code::Internal);
                        let mut trailers = HeaderMap::new();
                        // A fixed catalog status always encodes.
                        let _ = internal().add_header(&mut trailers);
                        return Poll::Ready(Some(Ok(Frame::trailers(trailers))));
                    }
                }
            }
        };
        match &polled {
            Poll::Pending => {}
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(trailers) = frame.trailers_ref() {
                    if matches!(this.recover, Recover::Panics)
                        && let Some(failure) = tonic::Status::from_header_map(trailers)
                            .as_ref()
                            .and_then(crate::status::catalog_code)
                        && let Some(call) = &this.call
                    {
                        call.failed(failure);
                    }
                    this.finish(status(trailers).unwrap_or(Code::Unknown));
                }
            }
            Poll::Ready(Some(Err(error))) => this.finish((this.error_code)(error)),
            // A gRPC answer always carries a status; one that ended without
            // it is what tonic's client reports as `Unknown`.
            Poll::Ready(None) => {
                this.inner = None;
                this.finish(Code::Unknown);
            }
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.as_ref().is_none_or(Body::is_end_stream)
    }

    fn size_hint(&self) -> SizeHint {
        self.inner
            .as_ref()
            .map_or_else(|| SizeHint::with_exact(0), Body::size_hint)
    }
}

/// The `grpc-status` of response headers or trailers.
fn status(headers: &HeaderMap) -> Option<Code> {
    headers
        .get("grpc-status")
        .map(|value| Code::from_bytes(value.as_bytes()))
}

/// Metric handles by call path, registered on first use so a call does not
/// rebuild label strings and look its series up in the recorder. The server
/// labels the described methods of its registered services and counts every
/// other path under `unknown`, so a caller-chosen path cannot create a
/// series. The client labels outbound paths, which are fixed method paths
/// when using generated tonic clients; its `Service<Request<Body>>` API does
/// not restrict paths.
pub(crate) struct Series {
    started: &'static str,
    handled: &'static str,
    handling_seconds: &'static str,
    /// The paths that become labels; `None` labels every path.
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

    pub(crate) fn client() -> Self {
        Self::new(
            CLIENT_STARTED,
            CLIENT_HANDLED,
            CLIENT_HANDLING_SECONDS,
            None,
        )
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
    fn label<'a>(&self, path: &'a str) -> &'a str {
        match &self.methods {
            Some(methods) if !methods.contains(path) => UNKNOWN_PATH,
            _ => path,
        }
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
        let (service, method) = labels(path);
        let handles = Handles {
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
        };
        let mut paths = self.paths.write().unwrap_or_else(PoisonError::into_inner);
        record(paths.entry(path.into()).or_insert(handles));
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
mod tests {
    use super::*;

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
}
