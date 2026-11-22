use std::mem::DropGuard;
use std::ops::Deref;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use aws_sdk_s3::operation::{RequestId, RequestIdExt};
use compact_str::CompactString;
use enum_map::EnumMap;
use metrics::{Histogram, Unit};
use tracing::Span;

use crate::error::{Failure, OutcomeError};

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
pub(crate) enum FailureOutcome {
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

#[derive(Clone, Copy, Default)]
struct RequestIds<'a> {
    request_id: Option<&'a str>,
    extended_request_id: Option<&'a str>,
}

impl<'a> RequestIds<'a> {
    fn from_reply(reply: &'a (impl RequestId + RequestIdExt)) -> Self {
        Self {
            request_id: reply.request_id().and_then(safe_request_id),
            extended_request_id: reply.extended_request_id().and_then(safe_request_id),
        }
    }

    fn record(self, span: &Span) {
        if let Some(request_id) = self.request_id {
            span.record("aws.request_id", request_id);
        }
        if let Some(extended_request_id) = self.extended_request_id {
            span.record("aws.extended_request_id", extended_request_id);
        }
    }
}

struct OperationInner<H: Deref<Target = Histograms>> {
    started: Instant,
    operation: Operation,
    span: Span,
    histograms: H,
    request_id: Option<CompactString>,
    extended_request_id: Option<CompactString>,
}

impl<H: Deref<Target = Histograms>> OperationInner<H> {
    fn abandon(self) {
        self.record_outcome(Outcome::Cancelled);
    }

    fn record_outcome(&self, outcome: Outcome) {
        self.span.record("object_storage.outcome", outcome.label());
        self.histograms
            .get(self.operation, outcome)
            .record(self.started.elapsed());
    }

    fn stored_request_ids(&self) -> RequestIds<'_> {
        RequestIds {
            request_id: self.request_id.as_deref(),
            extended_request_id: self.extended_request_id.as_deref(),
        }
    }

    fn record_failure<E>(
        &self,
        outcome: FailureOutcome,
        failure: &Failure<E>,
        ids: RequestIds<'_>,
    ) {
        let error_type = failure.error_type.as_str();
        let timeout_source = failure.timeout_source;
        self.span.record("error.type", error_type);
        if let Some(source) = timeout_source {
            self.span.record("object_storage.timeout_source", source);
        }
        self.span.record("otel.status_code", "ERROR");
        self.record_outcome(Outcome::Failed(outcome));
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
                        aws.request_id = ids.request_id,
                        aws.extended_request_id = ids.extended_request_id,
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

pub(crate) struct OperationGuard<H: Deref<Target = Histograms> = Arc<Histograms>>(
    DropGuard<OperationInner<H>, fn(OperationInner<H>)>,
);

impl<H: Deref<Target = Histograms>> OperationGuard<H> {
    pub(crate) fn start(histograms: H, operation: Operation, cloud_region: Option<&str>) -> Self {
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

    pub(crate) fn retain_request_ids(&mut self, response: &(impl RequestId + RequestIdExt)) {
        let inner = &mut *self.0;
        let ids = RequestIds::from_reply(response);
        ids.record(&inner.span);
        inner.request_id = ids.request_id.map(CompactString::from);
        inner.extended_request_id = ids.extended_request_id.map(CompactString::from);
    }

    pub(crate) fn succeed(self) {
        DropGuard::dismiss(self.0).record_outcome(Outcome::Ok);
    }

    pub(crate) fn succeed_with_reply(self, reply: &(impl RequestId + RequestIdExt)) {
        let inner = DropGuard::dismiss(self.0);
        RequestIds::from_reply(reply).record(&inner.span);
        inner.record_outcome(Outcome::Ok);
    }

    pub(crate) fn fail<E: OutcomeError>(self, failure: &Failure<E>) -> E {
        let inner = DropGuard::dismiss(self.0);
        let ids = inner.stored_request_ids();
        inner.record_failure(failure.error.failure_outcome(), failure, ids);
        failure.error
    }

    pub(crate) fn fail_with_reply<E: OutcomeError>(
        self,
        failure: &Failure<E>,
        reply: &(impl RequestId + RequestIdExt),
    ) -> E {
        let inner = DropGuard::dismiss(self.0);
        let ids = RequestIds::from_reply(reply);
        ids.record(&inner.span);
        inner.record_failure(failure.error.failure_outcome(), failure, ids);
        failure.error
    }

    pub(crate) fn finish<T, E: OutcomeError>(self, result: Result<T, Failure<E>>) -> Result<T, E> {
        match result {
            Ok(value) => {
                self.succeed();
                Ok(value)
            }
            Err(failure) => Err(self.fail(&failure)),
        }
    }

    pub(crate) fn finish_with_reply<T, E: OutcomeError>(
        self,
        result: Result<T, Failure<E>>,
        reply: Option<&(impl RequestId + RequestIdExt)>,
    ) -> Result<T, E> {
        match (result, reply) {
            (Ok(value), Some(reply)) => {
                self.succeed_with_reply(reply);
                Ok(value)
            }
            (Ok(value), None) => {
                self.succeed();
                Ok(value)
            }
            (Err(failure), Some(reply)) => Err(self.fail_with_reply(&failure, reply)),
            (Err(failure), None) => Err(self.fail(&failure)),
        }
    }
}

const MAX_REQUEST_ID_BYTES: usize = 128;

fn safe_request_id(value: &str) -> Option<&str> {
    let bounded = !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
        && value.bytes().all(|byte| byte.is_ascii_graphic());
    bounded.then_some(value)
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
