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
    match code {
        tonic::Code::Ok => StatusNames {
            span: "OK",
            metric: "OK",
        },
        tonic::Code::Cancelled => StatusNames {
            span: "CANCELLED",
            metric: "Canceled",
        },
        tonic::Code::Unknown => StatusNames {
            span: "UNKNOWN",
            metric: "Unknown",
        },
        tonic::Code::InvalidArgument => StatusNames {
            span: "INVALID_ARGUMENT",
            metric: "InvalidArgument",
        },
        tonic::Code::DeadlineExceeded => StatusNames {
            span: "DEADLINE_EXCEEDED",
            metric: "DeadlineExceeded",
        },
        tonic::Code::NotFound => StatusNames {
            span: "NOT_FOUND",
            metric: "NotFound",
        },
        tonic::Code::AlreadyExists => StatusNames {
            span: "ALREADY_EXISTS",
            metric: "AlreadyExists",
        },
        tonic::Code::PermissionDenied => StatusNames {
            span: "PERMISSION_DENIED",
            metric: "PermissionDenied",
        },
        tonic::Code::ResourceExhausted => StatusNames {
            span: "RESOURCE_EXHAUSTED",
            metric: "ResourceExhausted",
        },
        tonic::Code::FailedPrecondition => StatusNames {
            span: "FAILED_PRECONDITION",
            metric: "FailedPrecondition",
        },
        tonic::Code::Aborted => StatusNames {
            span: "ABORTED",
            metric: "Aborted",
        },
        tonic::Code::OutOfRange => StatusNames {
            span: "OUT_OF_RANGE",
            metric: "OutOfRange",
        },
        tonic::Code::Unimplemented => StatusNames {
            span: "UNIMPLEMENTED",
            metric: "Unimplemented",
        },
        tonic::Code::Internal => StatusNames {
            span: "INTERNAL",
            metric: "Internal",
        },
        tonic::Code::Unavailable => StatusNames {
            span: "UNAVAILABLE",
            metric: "Unavailable",
        },
        tonic::Code::DataLoss => StatusNames {
            span: "DATA_LOSS",
            metric: "DataLoss",
        },
        tonic::Code::Unauthenticated => StatusNames {
            span: "UNAUTHENTICATED",
            metric: "Unauthenticated",
        },
    }
}

const fn status_slot(code: tonic::Code) -> usize {
    match code {
        tonic::Code::Ok => 0,
        tonic::Code::Cancelled => 1,
        tonic::Code::Unknown => 2,
        tonic::Code::InvalidArgument => 3,
        tonic::Code::DeadlineExceeded => 4,
        tonic::Code::NotFound => 5,
        tonic::Code::AlreadyExists => 6,
        tonic::Code::PermissionDenied => 7,
        tonic::Code::ResourceExhausted => 8,
        tonic::Code::FailedPrecondition => 9,
        tonic::Code::Aborted => 10,
        tonic::Code::OutOfRange => 11,
        tonic::Code::Unimplemented => 12,
        tonic::Code::Internal => 13,
        tonic::Code::Unavailable => 14,
        tonic::Code::DataLoss => 15,
        tonic::Code::Unauthenticated => 16,
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
        let metric_code = status_code_names(code).metric;
        self.0[status_slot(code)]
            .get_or_init(|| init(metric_code))
            .increment(1);
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

pub struct Call<M: CallMetrics>(DropGuard<CallInner<M>, fn(CallInner<M>)>);

impl<M: CallMetrics> Call<M> {
    #[must_use]
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
