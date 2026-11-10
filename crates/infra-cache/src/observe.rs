use std::sync::OnceLock;
use std::time::Instant;

use metrics::{Histogram, Unit};
use tracing::Span;

use crate::{ServerEndpoint, Unavailable};

pub const OPERATION_DURATION_METRIC: &str = "cache_operation_duration_seconds";

/// Buckets in seconds for [`OPERATION_DURATION_METRIC`]. A command that runs
/// into a `command_timeout` above the top bucket lands only in `+Inf`.
pub const OPERATION_DURATION_BUCKETS: &[f64] = &[
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 1.0,
];

/// Every histogram this crate records. A composition root hands it to the
/// Prometheus recorder; a metric left out renders the default buckets.
pub const HISTOGRAMS: &[(&str, &[f64])] =
    &[(OPERATION_DURATION_METRIC, OPERATION_DURATION_BUCKETS)];

/// The closed set of namespace operations.
#[derive(Clone, Copy)]
pub(crate) enum Operation {
    Get,
    Set,
    Delete,
}

impl Operation {
    fn label(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Set => "set",
            Self::Delete => "delete",
        }
    }

    pub(crate) fn command(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Set => "SET",
            Self::Delete => "DEL",
        }
    }

    /// Whether a reply that arrives after the caller's context stopped is
    /// discarded as a timeout. The caller's budget is gone, so a read reply
    /// is stale; a confirmed mutation is final and keeps its result.
    pub(crate) fn is_read(self) -> bool {
        match self {
            Self::Get => true,
            Self::Set | Self::Delete => false,
        }
    }
}

/// Why a command did not produce a usable success: the span's `error.type`
/// and the `error_type` label of a failed series. Server text is never copied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ErrorType {
    /// The operation context stopped (its deadline, including
    /// `command_timeout`, or caller cancellation), a setup or probe bound
    /// elapsed, or the client reported a timeout.
    Timeout,
    Io,
    Auth,
    Response,
    Parse,
    Other,
}

impl ErrorType {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Io => "io",
            Self::Auth => "auth",
            Self::Response => "response",
            Self::Parse => "parse",
            Self::Other => "other",
        }
    }
}

/// How a command ended. Every variant is one series of an operation.
#[derive(Clone, Copy)]
pub(crate) enum Outcome {
    Hit,
    Miss,
    Ok,
    Cancelled,
    Failed(ErrorType),
}

impl Outcome {
    /// The `outcome` label. A timeout keeps its own value so an alert on it
    /// needs no second label.
    fn label(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Ok => "ok",
            Self::Cancelled => "cancelled",
            Self::Failed(ErrorType::Timeout) => "timeout",
            Self::Failed(_) => "error",
        }
    }
}

/// One namespace's histogram handles, registered on first use so the scrape
/// shows only series that were recorded. Resolving a labelled key on every
/// call would hash and look it up in the recorder instead. A handle stays
/// bound to the recorder of its first use; bootstrap installs the recorder
/// before it opens the cache.
#[derive(Default)]
pub(crate) struct NamespaceHistograms {
    get: OutcomeHistograms,
    set: OutcomeHistograms,
    delete: OutcomeHistograms,
}

#[derive(Default)]
struct OutcomeHistograms {
    hit: OnceLock<Histogram>,
    miss: OnceLock<Histogram>,
    ok: OnceLock<Histogram>,
    cancelled: OnceLock<Histogram>,
    failed: ErrorHistograms,
}

#[derive(Default)]
struct ErrorHistograms {
    timeout: OnceLock<Histogram>,
    io: OnceLock<Histogram>,
    auth: OnceLock<Histogram>,
    response: OnceLock<Histogram>,
    parse: OnceLock<Histogram>,
    other: OnceLock<Histogram>,
}

impl ErrorHistograms {
    fn slot(&self, error_type: ErrorType) -> &OnceLock<Histogram> {
        match error_type {
            ErrorType::Timeout => &self.timeout,
            ErrorType::Io => &self.io,
            ErrorType::Auth => &self.auth,
            ErrorType::Response => &self.response,
            ErrorType::Parse => &self.parse,
            ErrorType::Other => &self.other,
        }
    }
}

impl std::fmt::Debug for NamespaceHistograms {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NamespaceHistograms")
            .finish_non_exhaustive()
    }
}

impl NamespaceHistograms {
    fn get(&self, namespace: &'static str, operation: Operation, outcome: Outcome) -> &Histogram {
        let histograms = match operation {
            Operation::Get => &self.get,
            Operation::Set => &self.set,
            Operation::Delete => &self.delete,
        };
        let (slot, error_label) = match outcome {
            Outcome::Hit => (&histograms.hit, None),
            Outcome::Miss => (&histograms.miss, None),
            Outcome::Ok => (&histograms.ok, None),
            Outcome::Cancelled => (&histograms.cancelled, None),
            Outcome::Failed(error_type) => {
                (histograms.failed.slot(error_type), Some(error_type.label()))
            }
        };
        slot.get_or_init(|| {
            // Only a failed series names its cause, so the cause is visible
            // without a trace or a debug log.
            if let Some(error_label) = error_label {
                metrics::histogram!(
                    OPERATION_DURATION_METRIC,
                    "cache" => namespace,
                    "operation" => operation.label(),
                    "outcome" => outcome.label(),
                    "error_type" => error_label,
                )
            } else {
                metrics::histogram!(
                    OPERATION_DURATION_METRIC,
                    "cache" => namespace,
                    "operation" => operation.label(),
                    "outcome" => outcome.label(),
                )
            }
        })
    }
}

/// One polled cache command. The guard keeps the admitted server identity and
/// the bounded labels (namespace, operation, outcome, error type), never the
/// key, the value, or server text. Dropping it before an outcome records
/// `cancelled`.
pub(crate) struct OperationGuard<'a> {
    started: Instant,
    namespace: &'static str,
    histograms: &'a NamespaceHistograms,
    operation: Operation,
    span: Span,
    finalized: bool,
}

impl<'a> OperationGuard<'a> {
    pub(crate) fn start(
        namespace: &'static str,
        histograms: &'a NamespaceHistograms,
        operation: Operation,
        server: &ServerEndpoint,
    ) -> Self {
        let span = tracing::info_span!(
            "cache",
            otel.name = operation.command(),
            otel.kind = "client",
            db.system.name = "redis",
            db.operation.name = operation.command(),
            cache.name = namespace,
            server.address = server.host.as_str(),
            server.port = server.port,
            cache.outcome = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        Self {
            started: Instant::now(),
            namespace,
            histograms,
            operation,
            span,
            finalized: false,
        }
    }

    pub(crate) fn succeed(&mut self, outcome: Outcome) {
        self.finish(outcome);
    }

    pub(crate) fn fail(&mut self, error_type: ErrorType) -> Unavailable {
        let label = error_type.label();
        self.span.record("error.type", label);
        self.span.record("otel.status_code", "ERROR");
        self.finish(Outcome::Failed(error_type));
        self.span.in_scope(|| {
            tracing::debug!(
                cache.name = self.namespace,
                cache.operation = self.operation.label(),
                error.type = label,
                "cache_operation_failed"
            );
        });
        Unavailable
    }

    fn finish(&mut self, outcome: Outcome) {
        self.finalized = true;
        self.span.record("cache.outcome", outcome.label());
        self.histograms
            .get(self.namespace, self.operation, outcome)
            .record(self.started.elapsed().as_secs_f64());
    }
}

impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        if !self.finalized {
            self.finish(Outcome::Cancelled);
        }
    }
}

/// Describes the histogram to the installed recorder. Repeating it is harmless.
pub(crate) fn describe() {
    metrics::describe_histogram!(
        OPERATION_DURATION_METRIC,
        Unit::Seconds,
        "Cache operation duration in seconds"
    );
}

/// Bounded cause of a Redis client failure.
pub(crate) fn error_type(error: &redis::RedisError) -> ErrorType {
    if error.is_timeout() {
        return ErrorType::Timeout;
    }
    // RESP3 authenticates inside `HELLO`, whose refusal is a plain server error.
    if matches!(error.code(), Some("WRONGPASS" | "NOAUTH")) {
        return ErrorType::Auth;
    }
    match error.kind() {
        redis::ErrorKind::Io => ErrorType::Io,
        redis::ErrorKind::AuthenticationFailed => ErrorType::Auth,
        // redis-rs names only some server codes; `WRONGTYPE`, `OOM` and the
        // other unnamed codes are `Extension`, still a server error reply.
        redis::ErrorKind::Server(_) | redis::ErrorKind::Extension => ErrorType::Response,
        redis::ErrorKind::Parse => ErrorType::Parse,
        // InvalidClientConfig and every other kind stay `other`.
        _ => ErrorType::Other,
    }
}
