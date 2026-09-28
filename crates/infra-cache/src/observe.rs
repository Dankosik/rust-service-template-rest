use std::sync::OnceLock;
use std::time::Instant;

use metrics::{Histogram, Unit};
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
    const COUNT: usize = 3;

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

/// Every `outcome` label.
#[derive(Clone, Copy)]
pub(crate) enum Outcome {
    Hit,
    Miss,
    Ok,
    Timeout,
    Error,
    Cancelled,
}

impl Outcome {
    const COUNT: usize = 6;

    fn label(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Ok => "ok",
            Self::Timeout => "timeout",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One namespace's histogram handles, registered on first use so the scrape
/// shows only series that were recorded. Resolving a labelled key on every
/// call would hash and look it up in the recorder instead. A handle stays
/// bound to the recorder of its first use; bootstrap installs the recorder
/// before it opens the cache.
#[derive(Default)]
pub(crate) struct Histograms([OnceLock<Histogram>; Operation::COUNT * Outcome::COUNT]);

impl std::fmt::Debug for Histograms {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Histograms").finish_non_exhaustive()
    }
}

impl Histograms {
    fn get(&self, cache: &'static str, operation: Operation, outcome: Outcome) -> &Histogram {
        self.0[operation as usize * Outcome::COUNT + outcome as usize].get_or_init(|| {
            metrics::histogram!(
                OPERATION_DURATION_METRIC,
                "cache" => cache,
                "operation" => operation.label(),
                "outcome" => outcome.label(),
            )
        })
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

    fn outcome(self) -> Outcome {
        match self {
            Self::Timeout => Outcome::Timeout,
            Self::Redis(_) => Outcome::Error,
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
pub(crate) struct OperationGuard<'a> {
    started: Instant,
    cache: &'static str,
    histograms: &'a Histograms,
    operation: Operation,
    span: Span,
    finalized: bool,
}

impl<'a> OperationGuard<'a> {
    pub(crate) fn start(
        cache: &'static str,
        histograms: &'a Histograms,
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
            histograms,
            operation,
            span,
            finalized: false,
        }
    }

    pub(crate) fn succeed(&mut self, outcome: Outcome) {
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

    fn finish(&mut self, outcome: Outcome) {
        self.finalized = true;
        self.span.record("cache.outcome", outcome.label());
        self.histograms
            .get(self.cache, self.operation, outcome)
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
