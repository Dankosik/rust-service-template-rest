use std::time::Instant;

use metrics::Unit;
use tracing::Span;

use crate::{ServerIdentity, Unavailable};

/// Cache command duration histogram.
pub const OPERATION_DURATION_METRIC: &str = "cache_operation_duration_seconds";

/// Buckets in seconds for [`OPERATION_DURATION_METRIC`], including a
/// degraded timeout; the composition root passes both to the Prometheus
/// recorder.
pub const OPERATION_DURATION_BUCKETS: &[f64] = &[
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 1.0,
];

/// The closed set of namespace operations.
#[derive(Clone, Copy)]
pub(crate) enum Operation {
    Get,
    Set,
    Delete,
}

impl Operation {
    /// The `operation` metric label.
    fn label(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Set => "set",
            Self::Delete => "delete",
        }
    }

    /// The Redis command, recorded as `db.operation.name`.
    fn command(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Set => "SET",
            Self::Delete => "DEL",
        }
    }
}

/// Why a command returned no reply.
#[derive(Clone, Copy)]
pub(crate) enum Failure {
    /// `command_timeout` elapsed, or the client reported a timeout.
    Timeout,
    /// A Redis client error, as its bounded `error.type`.
    Redis(&'static str),
}

impl Failure {
    pub(crate) fn from_error(error: &redis::RedisError) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else {
            Self::Redis(error_type(error))
        }
    }

    fn outcome(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Redis(_) => "error",
        }
    }

    fn error_type(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Redis(error_type) => error_type,
        }
    }
}

/// One polled cache command. The guard keeps only the namespace label and
/// the outcome, never the key, the value, or server text. Dropping it before
/// an outcome records `cancelled`.
pub(crate) struct OperationGuard {
    started: Instant,
    cache: &'static str,
    operation: Operation,
    span: Span,
    finalized: bool,
}

impl OperationGuard {
    pub(crate) fn start(
        cache: &'static str,
        operation: Operation,
        server: &ServerIdentity,
    ) -> Self {
        let span = tracing::info_span!(
            "cache",
            otel.kind = "client",
            db.system.name = "redis",
            db.operation.name = operation.command(),
            cache.name = cache,
            server.address = server.host.as_str(),
            server.port = server.port,
            cache.outcome = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        Self {
            started: Instant::now(),
            cache,
            operation,
            span,
            finalized: false,
        }
    }

    pub(crate) fn span(&self) -> Span {
        self.span.clone()
    }

    pub(crate) fn succeed(&mut self, outcome: &'static str) {
        self.finish(outcome);
    }

    pub(crate) fn fail(&mut self, failure: Failure) -> Unavailable {
        let error_type = failure.error_type();
        self.span.record("error.type", error_type);
        self.span.record("otel.status_code", "ERROR");
        self.finish(failure.outcome());
        self.span.in_scope(|| {
            tracing::debug!(
                cache.name = self.cache,
                cache.operation = self.operation.label(),
                error.type = error_type,
                "cache_operation_failed"
            );
        });
        Unavailable
    }

    fn finish(&mut self, outcome: &'static str) {
        self.finalized = true;
        self.span.record("cache.outcome", outcome);
        metrics::histogram!(
            OPERATION_DURATION_METRIC,
            "cache" => self.cache,
            "operation" => self.operation.label(),
            "outcome" => outcome,
        )
        .record(self.started.elapsed().as_secs_f64());
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        if !self.finalized {
            self.finish("cancelled");
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

/// Bounded `error.type` for a Redis client failure. Server text is not copied.
pub(crate) fn error_type(error: &redis::RedisError) -> &'static str {
    if error.is_timeout() {
        return "timeout";
    }
    match error.kind() {
        redis::ErrorKind::Io => "io",
        redis::ErrorKind::AuthenticationFailed => "auth",
        redis::ErrorKind::Server(_) => "response",
        redis::ErrorKind::Parse => "parse",
        // InvalidClientConfig and every other kind stay `other`.
        _ => "other",
    }
}
