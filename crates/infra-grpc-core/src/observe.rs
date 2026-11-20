use std::fmt;
use std::mem::DropGuard;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use opentelemetry::trace::{SpanKind, Status as SpanStatus};
use shared_rules::grpc::{
    ERROR_TYPE_ATTR, FALLBACK_SPAN_NAME, RPC_METHOD_ATTR, RPC_METHOD_ORIGINAL_ATTR,
    RPC_METHOD_OTHER, RPC_STATUS_CODE_ATTR, RPC_SYSTEM_GRPC, RPC_SYSTEM_NAME_ATTR,
    STATUS_CODE_COUNT, diagnostic_rpc_method_original, status_code_names,
};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_opentelemetry_instrumentation_sdk::http as otel_http;
use tracing_opentelemetry_instrumentation_sdk::otel_trace_span;

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
        let metric_code = status_code_names(code as usize).metric;
        self.0[code as usize]
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
        let span_code = status_code_names(code as usize).span;
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
