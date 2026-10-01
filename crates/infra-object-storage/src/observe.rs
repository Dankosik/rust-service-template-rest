//! One duration histogram and one span per operation. Neither carries a key,
//! bucket, endpoint, URL, or provider message.

use std::sync::OnceLock;
use std::time::Instant;

use metrics::{Histogram, Unit};
use tracing::Span;

use crate::ObjectStorageError;

/// Object storage operation duration histogram.
pub const OPERATION_DURATION_METRIC: &str = "object_storage_operation_duration_seconds";

/// Buckets in seconds for [`OPERATION_DURATION_METRIC`], from a fast head to
/// a large transfer; the composition root passes both to the Prometheus
/// recorder.
pub const OPERATION_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];

/// The closed set of operations.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Operation {
    Put,
    Get,
    Head,
    Delete,
    PresignGet,
    Probe,
}

impl Operation {
    const COUNT: usize = 6;

    fn label(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Get => "get",
            Self::Head => "head",
            Self::Delete => "delete",
            Self::PresignGet => "presign_get",
            Self::Probe => "probe",
        }
    }

    /// The exported span name: `Service.Operation`, the OpenTelemetry
    /// convention for AWS SDK client spans.
    fn span_name(self) -> &'static str {
        match self {
            Self::Put => "S3.PutObject",
            Self::Get => "S3.GetObject",
            Self::Head => "S3.HeadObject",
            Self::Delete => "S3.DeleteObject",
            Self::PresignGet => "S3.PresignGetObject",
            Self::Probe => "S3.HeadBucket",
        }
    }

    /// The S3 API operation, recorded as `rpc.method`.
    fn method(self) -> &'static str {
        match self {
            Self::Put => "PutObject",
            Self::Get => "GetObject",
            Self::Head => "HeadObject",
            Self::Delete => "DeleteObject",
            Self::PresignGet => "PresignGetObject",
            Self::Probe => "HeadBucket",
        }
    }
}

/// `ok`, `cancelled`, and every [`ObjectStorageError`] label.
#[derive(Clone, Copy)]
enum Outcome {
    Ok,
    Cancelled,
    Failure(ObjectStorageError),
}

impl Outcome {
    const COUNT: usize = 2 + ObjectStorageError::COUNT;

    fn metric_slot(self) -> (usize, &'static str) {
        match self {
            Self::Ok => (0, "ok"),
            Self::Cancelled => (1, "cancelled"),
            Self::Failure(error) => {
                let (column, label) = error.metric_slot();
                (2 + column, label)
            }
        }
    }
}

/// Histogram handles, registered on first use so the scrape shows only
/// recorded series. A handle stays bound to the recorder of its first use;
/// bootstrap installs the recorder before it builds the client.
pub(crate) struct Histograms([OnceLock<Histogram>; Operation::COUNT * Outcome::COUNT]);

impl Default for Histograms {
    fn default() -> Self {
        Self(std::array::from_fn(|_| OnceLock::new()))
    }
}

impl std::fmt::Debug for Histograms {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Histograms").finish_non_exhaustive()
    }
}

impl Histograms {
    fn get(&self, operation: Operation, outcome: Outcome) -> &Histogram {
        let (column, label) = outcome.metric_slot();
        self.0[operation as usize * Outcome::COUNT + column].get_or_init(|| {
            metrics::histogram!(
                OPERATION_DURATION_METRIC,
                "operation" => operation.label(),
                "outcome" => label,
            )
        })
    }
}

/// One operation from admission to its outcome. Dropping it first records
/// `cancelled`: in Rust a dropped future is the cancellation.
pub(crate) struct OperationGuard {
    started: Instant,
    operation: Operation,
    span: Span,
    histograms: std::sync::Arc<Histograms>,
    finalized: bool,
}

impl std::fmt::Debug for OperationGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OperationGuard")
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}

impl OperationGuard {
    pub(crate) fn start(histograms: std::sync::Arc<Histograms>, operation: Operation) -> Self {
        let span = tracing::info_span!(
            "object_storage",
            otel.name = operation.span_name(),
            otel.kind = "client",
            rpc.system = "aws-api",
            rpc.service = "S3",
            rpc.method = operation.method(),
            object_storage.outcome = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        Self {
            started: Instant::now(),
            operation,
            span,
            histograms,
            finalized: false,
        }
    }

    pub(crate) fn succeed(&mut self) {
        self.finish(Outcome::Ok);
    }

    /// Record a failure. `error_type` is bounded: a provider error code, an
    /// HTTP status, or a transport class. The caller's error carries no
    /// provider detail, so the event is the operator's only record of it
    /// without tracing: a failure of the store, the configuration, or the
    /// data is a warning; an answer about the object or the admission limit
    /// stays at DEBUG.
    pub(crate) fn fail(
        &mut self,
        error: ObjectStorageError,
        error_type: &str,
    ) -> ObjectStorageError {
        self.span.record("error.type", error_type);
        self.span.record("otel.status_code", "ERROR");
        self.finish(Outcome::Failure(error));
        let operation = self.operation.label();
        self.span.in_scope(|| match error {
            ObjectStorageError::NotFound
            | ObjectStorageError::AlreadyExists
            | ObjectStorageError::TooLarge
            | ObjectStorageError::Busy => tracing::debug!(
                object_storage.operation = operation,
                object_storage.outcome = error.label(),
                error.type = error_type,
                "object_storage_operation_failed"
            ),
            ObjectStorageError::Unavailable
            | ObjectStorageError::Rejected
            | ObjectStorageError::OutcomeUnknown
            | ObjectStorageError::Integrity => tracing::warn!(
                object_storage.operation = operation,
                object_storage.outcome = error.label(),
                error.type = error_type,
                "object_storage_operation_failed"
            ),
        });
        error
    }

    fn finish(&mut self, outcome: Outcome) {
        self.finalized = true;
        self.span
            .record("object_storage.outcome", outcome.metric_slot().1);
        self.histograms
            .get(self.operation, outcome)
            .record(self.started.elapsed().as_secs_f64());
    }
}

impl Drop for OperationGuard {
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
        "Object storage operation duration in seconds; a get includes its body"
    );
}
