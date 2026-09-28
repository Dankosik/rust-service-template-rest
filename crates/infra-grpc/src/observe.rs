//! Spans and metrics. Metric names and labels follow the grpc-ecosystem
//! Prometheus convention (`go-grpc-middleware`), so standard dashboards apply.

use std::sync::Once;
use std::time::{Duration, Instant};

use http::HeaderMap;
use tonic::Code;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

const SERVER_HANDLED: &str = "grpc_server_handled_total";
const SERVER_HANDLING_SECONDS: &str = "grpc_server_handling_seconds";
const SERVER_SHED: &str = "grpc_server_shed_requests_total";
const CLIENT_HANDLED: &str = "grpc_client_handled_total";
const CLIENT_HANDLING_SECONDS: &str = "grpc_client_handling_seconds";

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
/// series; every other call is counted under `unknown`.
pub(crate) async fn observe(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let span = tracing_opentelemetry_instrumentation_sdk::http::grpc_server::make_span_from_request(
        &request,
    );
    let parent =
        tracing_opentelemetry_instrumentation_sdk::http::extract_context(request.headers());
    let _ = span.set_parent(parent);
    let started = Instant::now();
    let path = request.uri().path().to_owned();
    let response = next.run(request).instrument(span.clone()).await;
    tracing_opentelemetry_instrumentation_sdk::http::grpc::update_span_from_response(
        &span, &response, true,
    );
    let code = code_from_headers(response.headers());
    let dispatched = response.extensions().get::<Dispatched>().is_some();
    let (service, method) = if dispatched && code != Code::Unimplemented {
        service_and_method(&path)
    } else {
        ("unknown", "unknown")
    };
    record(
        SERVER_HANDLED,
        SERVER_HANDLING_SECONDS,
        service,
        method,
        code,
        started.elapsed(),
    );
    response
}

pub(crate) fn record_client(path: &str, code: Code, elapsed: Duration) {
    let (service, method) = service_and_method(path);
    record(
        CLIENT_HANDLED,
        CLIENT_HANDLING_SECONDS,
        service,
        method,
        code,
        elapsed,
    );
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

fn record(
    handled: &'static str,
    handling_seconds: &'static str,
    service: &str,
    method: &str,
    code: Code,
    elapsed: Duration,
) {
    describe();
    metrics::counter!(
        handled,
        "grpc_service" => label(service),
        "grpc_method" => label(method),
        "grpc_code" => code_name(code),
    )
    .increment(1);
    metrics::histogram!(
        handling_seconds,
        "grpc_service" => label(service),
        "grpc_method" => label(method),
    )
    .record(elapsed.as_secs_f64());
}

fn label(value: &str) -> metrics::SharedString {
    if value == "unknown" {
        "unknown".into()
    } else {
        value.to_owned().into()
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
