//! One duration histogram and one span per operation, carrying no key, bucket,
//! endpoint, URL, or provider message. The span and failure event carry the
//! provider's request identifiers; on Amazon S3 the span also carries the region.

use std::sync::OnceLock;
use std::time::Instant;

use aws_sdk_s3::operation::{RequestId, RequestIdExt};
use compact_str::CompactString;
use enum_map::EnumMap;
use metrics::{Histogram, Unit};
use tracing::Span;

use crate::ObjectStorageError;
use crate::error::Failure;

const OPERATION_DURATION_METRIC: &str = "object_storage_operation_duration_seconds";

const OPERATION_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];

/// A metric left out of the recorder renders default buckets.
pub const HISTOGRAMS: &[(&str, &[f64])] =
    &[(OPERATION_DURATION_METRIC, OPERATION_DURATION_BUCKETS)];

#[derive(Clone, Copy, Debug, enum_map::Enum)]
pub(crate) enum Operation {
    Put,
    Get,
    Head,
    Delete,
    PresignGet,
    Probe,
}

impl Operation {
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

    /// Follows OpenTelemetry's `Service.Operation` convention for AWS SDK client spans.
    fn rpc_names(self) -> (&'static str, &'static str) {
        match self {
            Self::Put => ("S3.PutObject", "PutObject"),
            Self::Get => ("S3.GetObject", "GetObject"),
            Self::Head => ("S3.HeadObject", "HeadObject"),
            Self::Delete => ("S3.DeleteObject", "DeleteObject"),
            Self::PresignGet => ("S3.PresignGetObject", "PresignGetObject"),
            Self::Probe => ("S3.HeadBucket", "HeadBucket"),
        }
    }
}

#[derive(Clone, Copy, enum_map::Enum)]
enum Outcome {
    Ok,
    Cancelled,
    Failure(ObjectStorageError),
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Cancelled => "cancelled",
            Self::Failure(error) => error.label(),
        }
    }
}

/// Registered on first use so the scrape shows only recorded series; a handle
/// stays bound to its first recorder, so bootstrap installs it before the client.
#[derive(Default, derive_more::Debug)]
#[debug("Histograms {{ .. }}")]
pub(crate) struct Histograms(EnumMap<Operation, EnumMap<Outcome, OnceLock<Histogram>>>);

impl Histograms {
    fn get(&self, operation: Operation, outcome: Outcome) -> &Histogram {
        self.0[operation][outcome].get_or_init(|| {
            metrics::histogram!(
                OPERATION_DURATION_METRIC,
                "operation" => operation.label(),
                "outcome" => outcome.label(),
            )
        })
    }
}

pub(crate) struct OperationGuard {
    started: Instant,
    operation: Operation,
    span: Span,
    histograms: std::sync::Arc<Histograms>,
    request_id: Option<CompactString>,
    extended_request_id: Option<CompactString>,
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
    pub(crate) fn start(
        histograms: std::sync::Arc<Histograms>,
        operation: Operation,
        cloud_region: Option<&str>,
    ) -> Self {
        let (span_name, method) = operation.rpc_names();
        let span = tracing::info_span!(
            "object_storage",
            otel.name = span_name,
            otel.kind = "client",
            rpc.system = "aws-api",
            rpc.service = "S3",
            rpc.method = method,
            cloud.region = cloud_region,
            object_storage.outcome = tracing::field::Empty,
            aws.request_id = tracing::field::Empty,
            aws.extended_request_id = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        Self {
            started: Instant::now(),
            operation,
            span,
            histograms,
            request_id: None,
            extended_request_id: None,
            finalized: false,
        }
    }

    /// `x-amz-request-id` and S3's `x-amz-id-2`, which its support asks for. Later
    /// failures (such as a checksum-failing body) report them too; after a retry they
    /// name the last attempt.
    pub(crate) fn answered_by(&mut self, answer: &(impl RequestId + RequestIdExt)) {
        self.request_id = answer.request_id().and_then(identifier);
        self.extended_request_id = answer.extended_request_id().and_then(identifier);
        if let Some(request_id) = self.request_id.as_deref() {
            self.span.record("aws.request_id", request_id);
        }
        if let Some(extended_request_id) = self.extended_request_id.as_deref() {
            self.span
                .record("aws.extended_request_id", extended_request_id);
        }
    }

    pub(crate) fn succeed(&mut self) {
        self.finish(Outcome::Ok);
    }

    /// `failure.error_type` is bounded: a provider error code, HTTP status, transport
    /// class, or fixed local label. The caller's error carries no provider detail,
    /// so this event is the operator's only record without tracing: a failure of the
    /// store, configuration, or data warns; an answer about the object or admission
    /// limit stays at DEBUG.
    pub(crate) fn fail(&mut self, failure: &Failure) -> ObjectStorageError {
        self.span.record("error.type", failure.error_type.as_str());
        self.span.record("otel.status_code", "ERROR");
        self.finish(Outcome::Failure(failure.error));
        let operation = self.operation.label();
        self.span.in_scope(|| {
            macro_rules! emit_failure {
                ($level:expr) => {
                    tracing::event!(
                        $level,
                        object_storage.operation = operation,
                        object_storage.outcome = failure.error.label(),
                        error.type = failure.error_type.as_str(),
                        aws.request_id = self.request_id.as_deref(),
                        aws.extended_request_id = self.extended_request_id.as_deref(),
                        "object_storage_operation_failed"
                    )
                };
            }

            match failure.error {
                ObjectStorageError::NotFound
                | ObjectStorageError::AlreadyExists
                | ObjectStorageError::TooLarge
                | ObjectStorageError::Busy => emit_failure!(tracing::Level::DEBUG),
                ObjectStorageError::Unavailable
                | ObjectStorageError::Rejected
                | ObjectStorageError::OutcomeUnknown
                | ObjectStorageError::Integrity => emit_failure!(tracing::Level::WARN),
            }
        });
        failure.error
    }

    pub(crate) fn cancel(&mut self) {
        self.finish(Outcome::Cancelled);
    }

    fn finish(&mut self, outcome: Outcome) {
        self.finalized = true;
        self.span.record("object_storage.outcome", outcome.label());
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

/// S3's `x-amz-id-2` is under 100 bytes.
const MAX_IDENTIFIER_BYTES: usize = 128;

/// Kept only as a short visible-ASCII token, so a store cannot put arbitrary
/// text into a log line.
fn identifier(value: &str) -> Option<CompactString> {
    let bounded = !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value.bytes().all(|byte| byte.is_ascii_graphic());
    bounded.then(|| CompactString::from(value))
}

pub(crate) fn describe() {
    metrics::describe_histogram!(
        OPERATION_DURATION_METRIC,
        Unit::Seconds,
        "Object storage operation duration in seconds; a get includes its body"
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_outcome_has_its_own_label() {
        let outcomes = EnumMap::<Outcome, ()>::default();
        let mut labels = HashSet::new();
        for (outcome, ()) in &outcomes {
            assert!(
                labels.insert(outcome.label()),
                "{} is used twice",
                outcome.label()
            );
        }
        assert_eq!(labels.len(), outcomes.len());
    }
}
