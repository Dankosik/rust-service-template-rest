use std::sync::Once;

use http::HeaderMap;
use opentelemetry::global;
use opentelemetry::propagation::{Extractor, Injector};
use tokio::time::Instant;
use tonic::Code;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use crate::Method;

static DESCRIBE: Once = Once::new();

/// One known-method transport span and metric guard. It retains neither peer
/// metadata values nor message content, and is dropped only at terminal body
/// completion, cancellation, timeout, or error.
pub(crate) struct Observation {
    method: Method,
    started: Instant,
    direction: &'static str,
    span: Span,
    finished: bool,
}

impl Observation {
    pub(crate) fn server(method: Method, headers: &HeaderMap) -> Self {
        let observation = Self::new(method, "server");
        global::get_text_map_propagator(|propagator| {
            // No OTel layer is a supported disabled-telemetry configuration.
            let _ = observation
                .span
                .set_parent(propagator.extract(&HeaderExtractor(headers)));
        });
        observation
    }

    pub(crate) fn client(method: Method) -> Self {
        Self::new(method, "client")
    }

    pub(crate) fn span(&self) -> Span {
        self.span.clone()
    }

    fn new(method: Method, direction: &'static str) -> Self {
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
        let (service, method_name) = split_method(method.path());
        let span = tracing::info_span!(
            "grpc_call",
            otel.kind = direction,
            rpc.system = "grpc",
            rpc.service = service,
            rpc.method = method_name,
            rpc.grpc.status_code = tracing::field::Empty,
            grpc.outcome = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        Self {
            method,
            started: Instant::now(),
            direction,
            span,
            finished: false,
        }
    }

    pub(crate) fn inject(&self, headers: &mut HeaderMap) {
        global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&self.span.context(), &mut HeaderInjector(headers));
        });
    }

    pub(crate) fn finish(mut self, status: Code) {
        self.record(status);
        self.finished = true;
    }

    fn record(&self, status: Code) {
        let outcome = outcome(status);
        self.span.record(
            "rpc.grpc.status_code",
            tracing::field::display(status as i32),
        );
        self.span.record("grpc.outcome", outcome);
        if status != Code::Ok {
            self.span.record("otel.status_code", "ERROR");
        }
        metrics::counter!(
            "grpc_calls_total",
            "method" => self.method.path(),
            "direction" => self.direction,
            "outcome" => outcome,
        )
        .increment(1);
        metrics::histogram!(
            "grpc_call_duration_seconds",
            "method" => self.method.path(),
            "direction" => self.direction,
        )
        .record(self.started.elapsed().as_secs_f64());
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        if !self.finished {
            self.record(Code::Cancelled);
        }
    }
}

pub(crate) fn status_code(headers: &HeaderMap) -> Option<Code> {
    headers
        .get("grpc-status")
        .map(|value| Code::from_bytes(value.as_bytes()))
}

struct HeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        ["traceparent", "tracestate"].to_vec()
    }
}

struct HeaderInjector<'a>(&'a mut HeaderMap);

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if matches!(key, "traceparent" | "tracestate")
            && let (Ok(name), Ok(value)) = (
                http::header::HeaderName::from_bytes(key.as_bytes()),
                http::HeaderValue::from_str(&value),
            )
        {
            self.0.insert(name, value);
        }
    }
}

fn split_method(path: &str) -> (&str, &str) {
    let trimmed = path.trim_start_matches('/');
    trimmed.split_once('/').unwrap_or(("unknown", "unknown"))
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
