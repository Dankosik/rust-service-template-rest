use std::mem::DropGuard;
use std::sync::OnceLock;
use std::time::Instant;

use enum_map::EnumMap;
use metrics::{Histogram, Unit};
use operation_context::Stopped;
use tracing::Span;

use crate::error::{CommandError, CommandFailure, ErrorType};
use crate::namespace::Operation;
use crate::{Namespace, ServerEndpoint};

const OPERATION_DURATION_METRIC: &str = "cache_operation_duration_seconds";

const OPERATION_DURATION_BUCKETS: &[f64] = &[
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 1.0,
];

pub const HISTOGRAMS: &[(&str, &[f64])] =
    &[(OPERATION_DURATION_METRIC, OPERATION_DURATION_BUCKETS)];

pub(crate) const ADMISSION_REFUSED_METRIC: &str = "cache_command_admission_refused_total";
pub(crate) const COMMANDS_IN_FLIGHT_METRIC: &str = "cache_commands_in_flight";
pub(crate) const RETIREMENTS_METRIC: &str = "cache_connection_retirements_total";
pub(crate) const PASSWORD_REFRESHES_METRIC: &str = "cache_password_file_refreshes_total";
const LOCAL_SIZE_REFUSALS_METRIC: &str = "cache_local_size_refusals_total";

pub(super) enum LocalSizeRefusal {
    Key,
    Value,
}

impl LocalSizeRefusal {
    pub(super) fn record(self, namespace: Namespace) {
        let reason = match self {
            Self::Key => "key_too_large",
            Self::Value => "value_too_large",
        };
        metrics::counter!(
            LOCAL_SIZE_REFUSALS_METRIC,
            "cache" => namespace.as_str(),
            "reason" => reason
        )
        .increment(1);
    }
}

fn operation_label(operation: Operation) -> &'static str {
    match operation {
        Operation::Get => "get",
        Operation::Set => "set",
        Operation::Delete => "delete",
    }
}

#[derive(Clone, Copy, enum_map::Enum)]
pub(crate) enum SuccessOutcome {
    Hit,
    Miss,
    Ok,
}

impl SuccessOutcome {
    const fn label(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Ok => "ok",
        }
    }
}

#[derive(Clone, Copy, enum_map::Enum)]
enum Outcome {
    Success(SuccessOutcome),
    Cancelled,
    Failed(ErrorType),
}

impl Outcome {
    fn error_label(self) -> Option<&'static str> {
        match self {
            Self::Failed(error_type) => Some(error_type.label()),
            Self::Success(_) | Self::Cancelled => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Success(outcome) => outcome.label(),
            Self::Cancelled => "cancelled",
            Self::Failed(ErrorType::Timeout) => "timeout",
            Self::Failed(_) => "error",
        }
    }
}

#[derive(derive_more::Debug)]
#[debug("NamespaceHistograms({:?}, ..)", self.namespace)]
pub(crate) struct NamespaceHistograms {
    namespace: Namespace,
    entries: EnumMap<Operation, EnumMap<Outcome, OnceLock<Histogram>>>,
}

impl NamespaceHistograms {
    pub(crate) fn new(namespace: Namespace) -> Self {
        Self {
            namespace,
            entries: EnumMap::default(),
        }
    }

    pub(crate) const fn namespace(&self) -> Namespace {
        self.namespace
    }

    fn histogram(&self, operation: Operation, outcome: Outcome) -> &Histogram {
        self.entries[operation][outcome].get_or_init(|| {
            let namespace = self.namespace.as_str();
            if let Some(error_label) = outcome.error_label() {
                metrics::histogram!(
                    OPERATION_DURATION_METRIC,
                    "cache" => namespace,
                    "operation" => operation_label(operation),
                    "outcome" => outcome.label(),
                    "error_type" => error_label,
                )
            } else {
                metrics::histogram!(
                    OPERATION_DURATION_METRIC,
                    "cache" => namespace,
                    "operation" => operation_label(operation),
                    "outcome" => outcome.label(),
                )
            }
        })
    }
}

struct OperationInner<'a> {
    started: Instant,
    histograms: &'a NamespaceHistograms,
    operation: Operation,
    span: Span,
}

impl OperationInner<'_> {
    fn abandon(self) {
        self.finish(Outcome::Cancelled);
    }

    fn finish(&self, outcome: Outcome) {
        self.span.record("cache.outcome", outcome.label());
        self.histograms
            .histogram(self.operation, outcome)
            .record(self.started.elapsed());
    }
}

pub(crate) struct OperationGuard<'a>(DropGuard<OperationInner<'a>, fn(OperationInner<'a>)>);

impl<'a> OperationGuard<'a> {
    pub(crate) fn start(
        histograms: &'a NamespaceHistograms,
        operation: Operation,
        server: &ServerEndpoint,
    ) -> Self {
        let span = tracing::info_span!(
            "cache",
            otel.name = operation.redis_name(),
            otel.kind = "client",
            db.system.name = "redis",
            db.namespace = %server.database_index,
            db.operation.name = operation.redis_name(),
            db.operation.batch.size = operation.redis_batch_size(),
            cache.name = histograms.namespace().as_str(),
            server.address = server.host.as_str(),
            server.port = server.port,
            cache.outcome = tracing::field::Empty,
            cache.mutation.outcome = tracing::field::Empty,
            cache.error.reason = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        Self(DropGuard::new(
            OperationInner {
                started: Instant::now(),
                histograms,
                operation,
                span,
            },
            OperationInner::abandon,
        ))
    }

    pub(crate) fn succeed(self, outcome: SuccessOutcome) {
        DropGuard::dismiss(self.0).finish(Outcome::Success(outcome));
    }

    pub(crate) fn fail(self, error: CommandError) {
        if matches!(self.0.operation, Operation::Set | Operation::Delete) {
            let effect = match error {
                CommandError::NotApplied(_) => "not_applied",
                CommandError::OutcomeUnknown(_) => "outcome_unknown",
            };
            self.0.span.record("cache.mutation.outcome", effect);
        }
        match error.failure() {
            CommandFailure::Stopped(Stopped::Cancelled) => {
                DropGuard::dismiss(self.0).finish(Outcome::Cancelled);
            }
            CommandFailure::Stopped(Stopped::Deadline) => {
                self.record_error(ErrorType::Timeout, None);
            }
            CommandFailure::Failed(error) => self.record_error(error, None),
            CommandFailure::PermissionDenied => {
                self.record_error(ErrorType::ServerErrorReply, Some("permission_denied"));
            }
            CommandFailure::InvalidTtl => self.record_error(ErrorType::ServerErrorReply, None),
        }
    }

    pub(crate) fn record_error(self, error_type: ErrorType, reason: Option<&'static str>) {
        let inner = DropGuard::dismiss(self.0);
        let label = error_type.label();
        inner.span.record("error.type", label);
        if let Some(reason) = reason {
            inner.span.record("cache.error.reason", reason);
        }
        inner.span.record("otel.status_code", "ERROR");
        inner.finish(Outcome::Failed(error_type));
        inner.span.in_scope(|| {
            tracing::debug!(
                cache.name = inner.histograms.namespace().as_str(),
                cache.operation = operation_label(inner.operation),
                cache.error.reason = reason,
                error.type = label,
                "cache_operation_failed"
            );
        });
    }
}

pub(crate) fn describe() {
    metrics::describe_histogram!(
        OPERATION_DURATION_METRIC,
        Unit::Seconds,
        "Cache operation duration in seconds including admission and command construction, excluding operations rejected by local key, value, or TTL validation"
    );
    metrics::describe_counter!(
        ADMISSION_REFUSED_METRIC,
        Unit::Count,
        "Cache commands refused because their admission slots are full"
    );
    metrics::describe_counter!(
        LOCAL_SIZE_REFUSALS_METRIC,
        Unit::Count,
        "Cache operations refused locally because the encoded key or supplied value exceeds its configured limit"
    );
    metrics::describe_gauge!(
        COMMANDS_IN_FLIGHT_METRIC,
        Unit::Count,
        "Admitted cache commands, including connection waits, until their futures complete or are dropped"
    );
    metrics::describe_counter!(
        RETIREMENTS_METRIC,
        Unit::Count,
        "Active cache connections taken out of use, including during shutdown"
    );
    metrics::describe_counter!(
        PASSWORD_REFRESHES_METRIC,
        Unit::Count,
        "Periodic cache password file re-reads on an open connection, by outcome (unchanged, auth_accepted, read_failed, refresh_failed) and the reason of a refresh_failed"
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_outcome_is_its_own_series() {
        let outcomes = EnumMap::<Outcome, ()>::default();
        let mut series = HashSet::new();
        for (outcome, ()) in &outcomes {
            let error_label = outcome.error_label();
            assert!(
                series.insert((outcome.label(), error_label)),
                "{} {error_label:?} is used twice",
                outcome.label()
            );
        }
        assert_eq!(series.len(), outcomes.len());
    }
}
