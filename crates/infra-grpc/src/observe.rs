//! Spans and metrics. Metric names and labels follow the grpc-ecosystem
//! Prometheus convention (`go-grpc-middleware`), so standard dashboards apply.
//!
//! The span is built as the HTTP adapter's observe builds its own: only the
//! operation name and kind are `tracing` fields, because the JSON log layer
//! serializes every span field and repeats it on each record inside the call;
//! the RPC attributes go to the OpenTelemetry span alone, and nothing is
//! `Span::record`ed afterwards, since every record re-serializes the fields.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Once, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

use futures_util::FutureExt as _;
use http::HeaderMap;
use opentelemetry::context::FutureExt as _;
use opentelemetry::trace::SpanKind;
use tonic::Code;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::{otel_trace_span, set_parent_or_fallback};

const SERVER_HANDLED: &str = "grpc_server_handled_total";
/// Server time to response headers, a histogram by service and method.
pub const SERVER_HANDLING_SECONDS: &str = "grpc_server_handling_seconds";
const SERVER_SHED: &str = "grpc_server_shed_requests_total";
const CLIENT_HANDLED: &str = "grpc_client_handled_total";
/// Client time to response headers, a histogram by service and method.
pub const CLIENT_HANDLING_SECONDS: &str = "grpc_client_handling_seconds";
/// Buckets for both handling-time histograms: the Prometheus client default
/// that `go-grpc-middleware` uses, so its dashboards' `_bucket` queries apply.
/// Without them the recorder would render a summary.
pub const HANDLING_SECONDS_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

static DESCRIBE: Once = Once::new();

/// Response marker set once a registered generated service handled the call.
#[derive(Clone, Copy)]
struct Dispatched;

pub(crate) fn mark_dispatched<B>(mut response: http::Response<B>) -> http::Response<B> {
    response.extensions_mut().insert(Dispatched);
    response
}

/// Server span and metrics. The path becomes labels only for a dispatched call
/// that the generated service recognized, so caller-chosen paths cannot create
/// series; every other call is counted under `unknown`. A panicking handler
/// becomes a sanitized `Internal` answer, observed like any other call.
pub(crate) async fn observe(
    axum::extract::State(series): axum::extract::State<std::sync::Arc<Series>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let started = Instant::now();
    // Shares the request's bytes; the path is read after the call.
    let uri = request.uri().clone();
    let span = make_span(&request, &SpanKind::Server);
    let fallback = set_parent_or_fallback(&span, otel_http::extract_context(request.headers()));
    let call = AssertUnwindSafe(next.run(request))
        .catch_unwind()
        .instrument(span.clone());
    let response = match fallback {
        None => call.await,
        // The span is disabled: keep the caller's context current instead,
        // so it still reaches outbound calls.
        Some(context) => call.with_context(context).await,
    }
    .unwrap_or_else(|_panic| tonic::Status::internal("request failed").into_http());
    let code = code_from_headers(response.headers());
    update_span(&span, code, &SpanKind::Server);
    let dispatched = response.extensions().get::<Dispatched>().is_some();
    let path = if dispatched && code != Code::Unimplemented {
        uri.path()
    } else {
        UNKNOWN_PATH
    };
    series.record(path, code, started.elapsed());
    response
}

/// The RPC span with its OpenTelemetry attributes.
pub(crate) fn make_span<B>(request: &http::Request<B>, kind: &SpanKind) -> tracing::Span {
    let (service, method) = service_and_method(request.uri().path());
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
pub(crate) fn update_span(span: &tracing::Span, code: Code, kind: &SpanKind) {
    span.set_attribute("rpc.grpc.status_code", i64::from(code as i32));
    if otel_http::grpc::status_is_error(code as u16, matches!(kind, SpanKind::Server)) {
        span.set_status(opentelemetry::trace::Status::error(""));
    }
}

pub(crate) fn record_shed() {
    describe();
    metrics::counter!(SERVER_SHED).increment(1);
}

pub(crate) fn code_from_headers(headers: &HeaderMap) -> Code {
    headers
        .get("grpc-status")
        .map_or(Code::Ok, |value| Code::from_bytes(value.as_bytes()))
}

/// Path whose labels are `unknown`.
const UNKNOWN_PATH: &str = "";

/// Metric handles by call path, registered on first use so a call does not
/// rebuild label strings and look its series up in the recorder. The server
/// records recognized, dispatched methods or `unknown`; the client records
/// outbound paths, which are fixed method paths when using generated tonic
/// clients. The client's `Service<Request<Body>>` API does not restrict paths.
pub(crate) struct Series {
    handled: &'static str,
    handling_seconds: &'static str,
    paths: RwLock<HashMap<Box<str>, Handles>>,
}

struct Handles {
    handling_seconds: metrics::Histogram,
    /// Indexed by status code.
    handled: [OnceLock<metrics::Counter>; 17],
}

impl Series {
    pub(crate) fn server() -> Self {
        Self::new(SERVER_HANDLED, SERVER_HANDLING_SECONDS)
    }

    pub(crate) fn client() -> Self {
        Self::new(CLIENT_HANDLED, CLIENT_HANDLING_SECONDS)
    }

    fn new(handled: &'static str, handling_seconds: &'static str) -> Self {
        describe();
        Self {
            handled,
            handling_seconds,
            paths: RwLock::default(),
        }
    }

    pub(crate) fn record(&self, path: &str, code: Code, elapsed: Duration) {
        {
            let paths = self.paths.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(handles) = paths.get(path) {
                self.record_in(handles, path, code, elapsed);
                return;
            }
        }
        let (service, method) = labels(path);
        let handles = Handles {
            handling_seconds: metrics::histogram!(
                self.handling_seconds,
                "grpc_service" => service.to_owned(),
                "grpc_method" => method.to_owned(),
            ),
            handled: Default::default(),
        };
        let mut paths = self.paths.write().unwrap_or_else(PoisonError::into_inner);
        let handles = paths.entry(path.into()).or_insert(handles);
        self.record_in(handles, path, code, elapsed);
    }

    fn record_in(&self, handles: &Handles, path: &str, code: Code, elapsed: Duration) {
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
            SERVER_HANDLED,
            metrics::Unit::Count,
            "Completed server gRPC calls by service, method, and status code"
        );
        metrics::describe_histogram!(
            SERVER_HANDLING_SECONDS,
            metrics::Unit::Seconds,
            "Server gRPC time to response headers by service and method"
        );
        metrics::describe_counter!(
            SERVER_SHED,
            metrics::Unit::Count,
            "Business gRPC calls rejected without running a handler because the concurrency limit was reached"
        );
        metrics::describe_counter!(
            CLIENT_HANDLED,
            metrics::Unit::Count,
            "Completed client gRPC calls by service, method, and status code"
        );
        metrics::describe_histogram!(
            CLIENT_HANDLING_SECONDS,
            metrics::Unit::Seconds,
            "Client gRPC time to response headers by service and method"
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
