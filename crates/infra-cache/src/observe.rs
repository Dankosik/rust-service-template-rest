use std::sync::OnceLock;
use std::time::Instant;

use enum_map::EnumMap;
use metrics::{Histogram, Unit};
use operation_context::Stopped;
use tracing::Span;

use crate::ServerEndpoint;
use crate::connection::{CommandError, CommandFailure, ErrorType};
use crate::namespace::Operation;

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
    pub(super) fn record(self, namespace: &'static str) {
        let reason = match self {
            Self::Key => "key_too_large",
            Self::Value => "value_too_large",
        };
        metrics::counter!(LOCAL_SIZE_REFUSALS_METRIC, "cache" => namespace, "reason" => reason)
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
pub(crate) enum Outcome {
    Hit,
    Miss,
    Ok,
    Cancelled,
    Failed(ErrorType),
}

impl Outcome {
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

#[derive(Default, derive_more::Debug)]
#[debug("NamespaceHistograms {{ .. }}")]
pub(crate) struct NamespaceHistograms(EnumMap<Operation, EnumMap<Outcome, OnceLock<Histogram>>>);

impl NamespaceHistograms {
    fn histogram(
        &self,
        namespace: &'static str,
        operation: Operation,
        outcome: Outcome,
    ) -> &Histogram {
        let error_label = match outcome {
            Outcome::Failed(error_type) => Some(error_type.label()),
            Outcome::Hit | Outcome::Miss | Outcome::Ok | Outcome::Cancelled => None,
        };
        self.0[operation][outcome].get_or_init(|| {
            if let Some(error_label) = error_label {
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
            otel.name = operation.redis_name(),
            otel.kind = "client",
            db.system.name = "redis",
            db.namespace = %server.database_index,
            db.operation.name = operation.redis_name(),
            db.operation.batch.size = matches!(operation, Operation::Get).then_some(2_i64),
            cache.name = namespace,
            server.address = server.host.as_str(),
            server.port = server.port,
            cache.outcome = tracing::field::Empty,
            cache.mutation.outcome = tracing::field::Empty,
            cache.error.reason = tracing::field::Empty,
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

    pub(crate) fn succeed(mut self, outcome: Outcome) {
        self.finish(outcome);
    }

    pub(crate) fn failed(mut self, error: CommandError) {
        if matches!(self.operation, Operation::Set | Operation::Delete) {
            let effect = match error {
                CommandError::NotApplied(_) => "not_applied",
                CommandError::OutcomeUnknown(_) => "outcome_unknown",
            };
            self.span.record("cache.mutation.outcome", effect);
        }
        match error.failure() {
            CommandFailure::Stopped(Stopped::Cancelled) => self.finish(Outcome::Cancelled),
            CommandFailure::Stopped(Stopped::Deadline) => self.fail(ErrorType::Timeout, None),
            CommandFailure::Failed(error) => self.fail(error, None),
            CommandFailure::PermissionDenied => {
                self.fail(ErrorType::ServerErrorReply, Some("permission_denied"));
            }
            CommandFailure::InvalidTtl => self.fail(ErrorType::ServerErrorReply, None),
        }
    }

    pub(crate) fn fail(mut self, error_type: ErrorType, reason: Option<&'static str>) {
        let label = error_type.label();
        self.span.record("error.type", label);
        if let Some(reason) = reason {
            self.span.record("cache.error.reason", reason);
        }
        self.span.record("otel.status_code", "ERROR");
        self.finish(Outcome::Failed(error_type));
        self.span.in_scope(|| {
            tracing::debug!(
                cache.name = self.namespace,
                cache.operation = operation_label(self.operation),
                cache.error.reason = reason,
                error.type = label,
                "cache_operation_failed"
            );
        });
    }

    fn finish(&mut self, outcome: Outcome) {
        self.finalized = true;
        self.span.record("cache.outcome", outcome.label());
        self.histograms
            .histogram(self.namespace, self.operation, outcome)
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
            let error_label = match outcome {
                Outcome::Failed(error_type) => Some(error_type.label()),
                _ => None,
            };
            assert!(
                series.insert((outcome.label(), error_label)),
                "{} {error_label:?} is used twice",
                outcome.label()
            );
        }
        assert_eq!(series.len(), outcomes.len());
    }
}
