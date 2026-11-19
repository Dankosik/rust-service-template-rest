use std::mem::DropGuard;
use std::sync::OnceLock;
use std::time::Instant;

use aws_sdk_s3::operation::{RequestId, RequestIdExt};
use compact_str::CompactString;
use enum_map::EnumMap;
use metrics::{Histogram, Unit};
use tracing::Span;

use crate::error::{Failure, MutationError, MutationFailure, ObjectStorageError};

const OPERATION_DURATION_METRIC: &str = "object_storage_operation_duration_seconds";

const OPERATION_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];

pub const HISTOGRAMS: &[(&str, &[f64])] =
    &[(OPERATION_DURATION_METRIC, OPERATION_DURATION_BUCKETS)];

#[derive(Clone, Copy, Debug, enum_map::Enum)]
pub(crate) enum Operation {
    Put,
    Get,
    Head,
    Delete,
    PresignGet,
}

impl Operation {
    fn label(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Get => "get",
            Self::Head => "head",
            Self::Delete => "delete",
            Self::PresignGet => "presign_get",
        }
    }

    fn span_and_rpc_method(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Put => ("S3.PutObject", Some("PutObject")),
            Self::Get => ("S3.GetObject", Some("GetObject")),
            Self::Head => ("S3.HeadObject", Some("HeadObject")),
            Self::Delete => ("S3.DeleteObject", Some("DeleteObject")),
            Self::PresignGet => ("S3.PresignGetObject", None),
        }
    }
}

#[derive(Clone, Copy, enum_map::Enum)]
enum FailureOutcome {
    NotFound,
    AlreadyExists,
    TooLarge,
    Busy,
    Unavailable,
    Rejected,
    Unknown,
    Integrity,
}

impl FailureOutcome {
    const fn label(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::AlreadyExists => "already_exists",
            Self::TooLarge => "too_large",
            Self::Busy => "busy",
            Self::Unavailable => "unavailable",
            Self::Rejected => "rejected",
            Self::Unknown => "outcome_unknown",
            Self::Integrity => "integrity",
        }
    }
}

#[derive(Clone, Copy, enum_map::Enum)]
enum Outcome {
    Ok,
    Cancelled,
    Failed(FailureOutcome),
}

impl Outcome {
    const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Cancelled => "cancelled",
            Self::Failed(failure) => failure.label(),
        }
    }
}

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

struct OperationInner {
    started: Instant,
    operation: Operation,
    span: Span,
    histograms: std::sync::Arc<Histograms>,
    request_id: Option<CompactString>,
    extended_request_id: Option<CompactString>,
}

impl OperationInner {
    fn abandon(self) {
        self.finish(Outcome::Cancelled);
    }

    fn finish(&self, outcome: Outcome) {
        self.span.record("object_storage.outcome", outcome.label());
        self.histograms
            .get(self.operation, outcome)
            .record(self.started.elapsed());
    }

    fn record_failure<E>(&self, outcome: FailureOutcome, failure: &Failure<E>) {
        let error_type = failure.error_type.as_str();
        let timeout_source = failure.timeout_source;
        self.span.record("error.type", error_type);
        if let Some(source) = timeout_source {
            self.span.record("object_storage.timeout_source", source);
        }
        self.span.record("otel.status_code", "ERROR");
        self.finish(Outcome::Failed(outcome));
        let operation = self.operation.label();
        self.span.in_scope(|| {
            macro_rules! emit_failure {
                ($level:expr) => {
                    tracing::event!(
                        $level,
                        object_storage.operation = operation,
                        object_storage.outcome = outcome.label(),
                        object_storage.timeout_source = timeout_source,
                        error.type = error_type,
                        aws.request_id = self.request_id.as_deref(),
                        aws.extended_request_id = self.extended_request_id.as_deref(),
                        "object_storage_operation_failed"
                    )
                };
            }

            match outcome {
                FailureOutcome::NotFound
                | FailureOutcome::AlreadyExists
                | FailureOutcome::TooLarge
                | FailureOutcome::Busy => emit_failure!(tracing::Level::DEBUG),
                FailureOutcome::Unavailable
                | FailureOutcome::Rejected
                | FailureOutcome::Unknown
                | FailureOutcome::Integrity => emit_failure!(tracing::Level::WARN),
            }
        });
    }
}

pub(crate) struct OperationGuard(DropGuard<OperationInner, fn(OperationInner)>);

impl OperationGuard {
    pub(crate) fn start(
        histograms: std::sync::Arc<Histograms>,
        operation: Operation,
        cloud_region: Option<&str>,
    ) -> Self {
        let (span_name, rpc_method) = operation.span_and_rpc_method();
        let span = tracing::info_span!(
            "object_storage",
            otel.name = span_name,
            otel.kind = if rpc_method.is_some() {
                "client"
            } else {
                "internal"
            },
            rpc.system = rpc_method.map(|_| "aws-api"),
            rpc.service = rpc_method.map(|_| "S3"),
            rpc.method = rpc_method,
            cloud.region = cloud_region,
            object_storage.outcome = tracing::field::Empty,
            object_storage.timeout_source = tracing::field::Empty,
            aws.request_id = tracing::field::Empty,
            aws.extended_request_id = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        Self(DropGuard::new(
            OperationInner {
                started: Instant::now(),
                operation,
                span,
                histograms,
                request_id: None,
                extended_request_id: None,
            },
            OperationInner::abandon,
        ))
    }

    pub(crate) fn record_request_ids(&mut self, response: &(impl RequestId + RequestIdExt)) {
        let inner = &mut *self.0;
        inner.request_id = response.request_id().and_then(bounded_request_id);
        inner.extended_request_id = response.extended_request_id().and_then(bounded_request_id);
        if let Some(request_id) = inner.request_id.as_deref() {
            inner.span.record("aws.request_id", request_id);
        }
        if let Some(extended_request_id) = inner.extended_request_id.as_deref() {
            inner
                .span
                .record("aws.extended_request_id", extended_request_id);
        }
    }

    pub(crate) fn succeed(self) {
        DropGuard::dismiss(self.0).finish(Outcome::Ok);
    }

    pub(crate) fn fail(self, failure: &Failure) -> ObjectStorageError {
        let outcome = match failure.error {
            ObjectStorageError::NotFound => FailureOutcome::NotFound,
            ObjectStorageError::TooLarge => FailureOutcome::TooLarge,
            ObjectStorageError::Busy => FailureOutcome::Busy,
            ObjectStorageError::Unavailable | ObjectStorageError::Stopped(_) => {
                FailureOutcome::Unavailable
            }
            ObjectStorageError::Rejected => FailureOutcome::Rejected,
            ObjectStorageError::Integrity => FailureOutcome::Integrity,
        };
        DropGuard::dismiss(self.0).record_failure(outcome, failure);
        failure.error
    }

    fn fail_mutation(self, failure: &Failure<MutationError>) -> MutationError {
        let outcome = match failure.error {
            MutationError::OutcomeUnknown(_) => FailureOutcome::Unknown,
            MutationError::NotApplied(reason) => match reason {
                MutationFailure::AlreadyExists => FailureOutcome::AlreadyExists,
                MutationFailure::TooLarge => FailureOutcome::TooLarge,
                MutationFailure::Busy => FailureOutcome::Busy,
                MutationFailure::Conflict
                | MutationFailure::Unavailable
                | MutationFailure::Stopped(_) => FailureOutcome::Unavailable,
                MutationFailure::Rejected => FailureOutcome::Rejected,
            },
        };
        DropGuard::dismiss(self.0).record_failure(outcome, failure);
        failure.error
    }

    pub(crate) fn finish_read<T>(
        self,
        result: Result<T, Failure>,
    ) -> Result<T, ObjectStorageError> {
        match result {
            Ok(value) => {
                self.succeed();
                Ok(value)
            }
            Err(failure) => Err(self.fail(&failure)),
        }
    }

    pub(crate) fn finish_mutation<T>(
        self,
        result: Result<T, Failure<MutationError>>,
    ) -> Result<T, MutationError> {
        match result {
            Ok(value) => {
                self.succeed();
                Ok(value)
            }
            Err(failure) => Err(self.fail_mutation(&failure)),
        }
    }
}

const MAX_REQUEST_ID_BYTES: usize = 128;

fn bounded_request_id(value: &str) -> Option<CompactString> {
    let bounded = !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
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
