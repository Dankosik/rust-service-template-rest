use std::sync::Once;
use std::time::{Duration, Instant};

use http::HeaderMap;
use tonic::Code;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

static DESCRIBE: Once = Once::new();

#[derive(Clone, Copy)]
struct Dispatched;

pub(crate) async fn mark_dispatched(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    response.extensions_mut().insert(Dispatched);
    response
}

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
    let label =
        if response.extensions().get::<Dispatched>().is_some() && code != Code::Unimplemented {
            path.as_str()
        } else {
            "unknown"
        };
    record(label, "server", code, started.elapsed());
    response
}

pub(crate) fn code_from_headers(headers: &HeaderMap) -> Code {
    headers
        .get("grpc-status")
        .map_or(Code::Ok, |value| Code::from_bytes(value.as_bytes()))
}

pub(crate) fn record(label: &str, direction: &'static str, code: Code, elapsed: Duration) {
    DESCRIBE.call_once(|| {
        metrics::describe_counter!(
            "grpc_calls_total",
            "Closed gRPC calls by generated method, direction, and outcome"
        );
        metrics::describe_histogram!(
            "grpc_call_duration_seconds",
            "Closed gRPC call duration by generated method and direction"
        );
    });
    let outcome = outcome(code);
    metrics::counter!(
        "grpc_calls_total",
        "method" => label.to_owned(),
        "direction" => direction,
        "outcome" => outcome,
    )
    .increment(1);
    metrics::histogram!(
        "grpc_call_duration_seconds",
        "method" => label.to_owned(),
        "direction" => direction,
    )
    .record(elapsed.as_secs_f64());
}

const fn outcome(status: Code) -> &'static str {
    match status {
        Code::Ok => "ok",
        Code::Cancelled => "cancelled",
        Code::DeadlineExceeded => "deadline_exceeded",
        Code::InvalidArgument => "invalid_argument",
        Code::Unauthenticated => "unauthenticated",
        Code::PermissionDenied => "permission_denied",
        Code::NotFound => "not_found",
        Code::AlreadyExists => "already_exists",
        Code::Aborted => "aborted",
        Code::Unimplemented => "unimplemented",
        Code::ResourceExhausted => "resource_exhausted",
        Code::Unavailable => "unavailable",
        Code::Internal
        | Code::Unknown
        | Code::DataLoss
        | Code::FailedPrecondition
        | Code::OutOfRange => "internal",
    }
}
