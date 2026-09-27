use std::time::Instant;

use metrics::Unit;
use tracing::Span;

use crate::Unavailable;

const OPERATION_DURATION_METRIC: &str = "cache_operation_duration_seconds";

/// One polled cache command. The guard keeps only the namespace label and
/// the outcome, never the key, the value, or server text.
pub(crate) struct OperationGuard {
    started: Instant,
    cache: &'static str,
    operation: &'static str,
    span: Span,
    finalized: bool,
}

impl OperationGuard {
    pub(crate) fn start(
        cache: &'static str,
        operation: &'static str,
        redis_operation: &'static str,
        server_address: &str,
        server_port: u16,
    ) -> Self {
        describe_histogram();
        let span = tracing::info_span!(
            "cache",
            otel.kind = "client",
            db.system.name = "redis",
            db.operation.name = redis_operation,
            cache.name = cache,
            server.address = server_address,
            server.port = server_port,
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
        self.finish(outcome, None);
    }

    pub(crate) fn fail(&mut self, outcome: &'static str, error_type: &'static str) -> Unavailable {
        self.finish(outcome, Some(error_type));
        self.span.in_scope(|| {
            tracing::debug!(
                cache.name = self.cache,
                cache.operation = self.operation,
                error.type = error_type,
                "cache_operation_failed"
            );
        });
        Unavailable
    }

    fn finish(&mut self, outcome: &'static str, error_type: Option<&'static str>) {
        if self.finalized {
            return;
        }
        self.finalized = true;
        self.span.record("cache.outcome", outcome);
        if let Some(error_type) = error_type {
            self.span.record("error.type", error_type);
            self.span.record("otel.status_code", "ERROR");
        }
        metrics::histogram!(
            OPERATION_DURATION_METRIC,
            "cache" => self.cache,
            "operation" => self.operation,
            "outcome" => outcome,
        )
        .record(self.started.elapsed().as_secs_f64());
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        if !self.finalized {
            self.finish("cancelled", None);
        }
    }
}

fn describe_histogram() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        metrics::describe_histogram!(
            OPERATION_DURATION_METRIC,
            Unit::Seconds,
            "Cache operation duration in seconds"
        );
    });
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

pub(crate) fn classify<T>(
    result: Result<Result<T, redis::RedisError>, tokio::time::error::Elapsed>,
) -> Result<T, (&'static str, &'static str)> {
    match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) if error.is_timeout() => Err(("timeout", "timeout")),
        Ok(Err(error)) => Err(("error", error_type(&error))),
        Err(_) => Err(("timeout", "timeout")),
    }
}
