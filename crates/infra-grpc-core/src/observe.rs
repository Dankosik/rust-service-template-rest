use std::fmt;
use std::mem::DropGuard;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use opentelemetry::trace::{SpanKind, Status as SpanStatus};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::otel_trace_span;

pub const DIAGNOSTIC_BYTES: usize = 1024;
pub const HANDLING_SECONDS_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

const FALLBACK_SPAN_NAME: &str = "grpc";
const RPC_SYSTEM_NAME_ATTR: &str = "rpc.system.name";
const RPC_SYSTEM_GRPC: &str = "grpc";
const RPC_METHOD_ATTR: &str = "rpc.method";
const RPC_METHOD_OTHER: &str = "_OTHER";
const RPC_METHOD_ORIGINAL_ATTR: &str = "rpc.method_original";
const RPC_STATUS_CODE_ATTR: &str = "rpc.status_code";
const ERROR_TYPE_ATTR: &str = "error.type";

const STATUS_CODE_COUNT: usize = 17;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusNames {
    pub span: &'static str,
    pub metric: &'static str,
}

#[must_use]
pub const fn status_code_names(code: tonic::Code) -> StatusNames {
    status_entry(code).1
}

const fn entry(slot: usize, span: &'static str, metric: &'static str) -> (usize, StatusNames) {
    (slot, StatusNames { span, metric })
}

const fn status_entry(code: tonic::Code) -> (usize, StatusNames) {
    match code {
        tonic::Code::Ok => entry(0, "OK", "OK"),
        tonic::Code::Cancelled => entry(1, "CANCELLED", "Canceled"),
        tonic::Code::Unknown => entry(2, "UNKNOWN", "Unknown"),
        tonic::Code::InvalidArgument => entry(3, "INVALID_ARGUMENT", "InvalidArgument"),
        tonic::Code::DeadlineExceeded => entry(4, "DEADLINE_EXCEEDED", "DeadlineExceeded"),
        tonic::Code::NotFound => entry(5, "NOT_FOUND", "NotFound"),
        tonic::Code::AlreadyExists => entry(6, "ALREADY_EXISTS", "AlreadyExists"),
        tonic::Code::PermissionDenied => entry(7, "PERMISSION_DENIED", "PermissionDenied"),
        tonic::Code::ResourceExhausted => entry(8, "RESOURCE_EXHAUSTED", "ResourceExhausted"),
        tonic::Code::FailedPrecondition => entry(9, "FAILED_PRECONDITION", "FailedPrecondition"),
        tonic::Code::Aborted => entry(10, "ABORTED", "Aborted"),
        tonic::Code::OutOfRange => entry(11, "OUT_OF_RANGE", "OutOfRange"),
        tonic::Code::Unimplemented => entry(12, "UNIMPLEMENTED", "Unimplemented"),
        tonic::Code::Internal => entry(13, "INTERNAL", "Internal"),
        tonic::Code::Unavailable => entry(14, "UNAVAILABLE", "Unavailable"),
        tonic::Code::DataLoss => entry(15, "DATA_LOSS", "DataLoss"),
        tonic::Code::Unauthenticated => entry(16, "UNAUTHENTICATED", "Unauthenticated"),
    }
}

fn diagnostic_rpc_method_original(path: &str) -> Option<&str> {
    let trimmed = path.trim_start_matches('/');
    (!trimmed.is_empty() && trimmed.len() <= DIAGNOSTIC_BYTES).then_some(trimmed)
}

pub trait CallMetrics: Send + Sync + 'static {
    const IS_SERVER: bool;

    fn record_handled(&self, code: tonic::Code, elapsed: Duration);
}

pub struct StatusCounters([OnceLock<metrics::Counter>; STATUS_CODE_COUNT]);

impl StatusCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self([const { OnceLock::new() }; STATUS_CODE_COUNT])
    }

    pub fn increment(
        &self,
        code: tonic::Code,
        init: impl FnOnce(&'static str) -> metrics::Counter,
    ) {
        let (slot, names) = status_entry(code);
        self.0[slot].get_or_init(|| init(names.metric)).increment(1);
    }
}

impl Default for StatusCounters {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for StatusCounters {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StatusCounters")
            .finish_non_exhaustive()
    }
}

struct CallInner<M: CallMetrics> {
    metrics: Arc<M>,
    span: tracing::Span,
    started_at: Instant,
}

impl<M: CallMetrics> CallInner<M> {
    fn emit(&self, code: tonic::Code) {
        let span_code = status_code_names(code).span;
        self.span.set_attribute(RPC_STATUS_CODE_ATTR, span_code);
        if otel_http::grpc::status_is_error(code as u16, M::IS_SERVER) {
            self.span.set_attribute(ERROR_TYPE_ATTR, span_code);
            self.span.set_status(SpanStatus::error(""));
        }
        self.metrics.record_handled(code, self.started_at.elapsed());
    }

    fn cancel(self) {
        self.emit(tonic::Code::Cancelled);
    }
}

#[must_use = "finish the gRPC call or drop it intentionally"]
pub struct Call<M: CallMetrics>(DropGuard<CallInner<M>, fn(CallInner<M>)>);

impl<M: CallMetrics> Call<M> {
    pub fn start(metrics: Arc<M>, span: tracing::Span, started_at: Instant) -> Self {
        Self(DropGuard::new(
            CallInner {
                metrics,
                span,
                started_at,
            },
            CallInner::cancel,
        ))
    }

    #[must_use]
    pub fn span(&self) -> &tracing::Span {
        &self.0.span
    }

    #[must_use]
    pub fn metrics(&self) -> &M {
        &self.0.metrics
    }

    pub fn finish(self, code: tonic::Code) {
        DropGuard::dismiss(self.0).emit(code);
    }

    #[must_use]
    pub fn refuse(self, status: tonic::Status) -> tonic::Status {
        self.finish(status.code());
        status
    }
}

impl<M: CallMetrics> fmt::Debug for Call<M> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Call").finish_non_exhaustive()
    }
}

#[must_use]
pub fn open_rpc_span(
    kind: &SpanKind,
    rpc_method: Option<&Arc<str>>,
    raw_path: &str,
) -> tracing::Span {
    let span_name = rpc_method.map_or(FALLBACK_SPAN_NAME, AsRef::as_ref);
    let span = otel_trace_span!(
        "GRPC request",
        otel.name = span_name,
        otel.kind = ?kind,
    );
    if span.is_disabled() {
        return span;
    }
    span.set_attribute(RPC_SYSTEM_NAME_ATTR, RPC_SYSTEM_GRPC);
    if let Some(method) = rpc_method {
        span.set_attribute(RPC_METHOD_ATTR, Arc::clone(method));
    } else {
        span.set_attribute(RPC_METHOD_ATTR, RPC_METHOD_OTHER);
        if let Some(raw) = diagnostic_rpc_method_original(raw_path) {
            span.set_attribute(RPC_METHOD_ORIGINAL_ATTR, raw.to_owned());
        }
    }
    span
}
