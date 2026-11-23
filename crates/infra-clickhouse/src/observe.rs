use std::mem::DropGuard;
use std::sync::OnceLock;
use std::time::Instant;

use enum_map::EnumMap;
use metrics::{Counter, Gauge, Histogram, Unit};
use operation_context::Stopped;
use tracing::Span;

use crate::InsertReport;
use crate::error::{
    FailureDetail, FailureReason, InsertError, InsertFailure, ObservedFailure, QueryError,
};

const OPERATION_DURATION_METRIC: &str = "clickhouse_operation_duration_seconds";

const OPERATION_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

pub const HISTOGRAMS: &[(&str, &[f64])] =
    &[(OPERATION_DURATION_METRIC, OPERATION_DURATION_BUCKETS)];

const OPERATIONS_IN_FLIGHT_METRIC: &str = "clickhouse_operations_in_flight";
const ADMISSION_REFUSED_METRIC: &str = "clickhouse_admission_refused_total";
const INSERT_ROWS_METRIC: &str = "clickhouse_insert_rows_total";
const READ_ROWS_METRIC: &str = "clickhouse_read_rows_total";
const READ_BYTES_METRIC: &str = "clickhouse_read_bytes_total";
const PASSWORD_REFRESHES_METRIC: &str = "clickhouse_password_file_refreshes_total";

#[derive(Clone, Debug)]
pub(crate) struct EndpointMetadata {
    pub(crate) database: String,
    pub(crate) host: String,
    pub(crate) port: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
pub(crate) enum Operation {
    FetchOne,
    FetchOptional,
    FetchAll,
    InsertBatch,
    InsertAsync,
}

impl Operation {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::FetchOne => "fetch_one",
            Self::FetchOptional => "fetch_optional",
            Self::FetchAll => "fetch_all",
            Self::InsertBatch => "insert_batch",
            Self::InsertAsync => "insert_async",
        }
    }

    const fn db_operation_name(self) -> &'static str {
        match self {
            Self::FetchOne | Self::FetchOptional | Self::FetchAll => "SELECT",
            Self::InsertBatch | Self::InsertAsync => "INSERT",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
pub(crate) enum Outcome {
    Ok,
    NotFound,
    TooManyRows,
    TooLarge,
    TooManyParts,
    Encode,
    Decode,
    Rejected,
    Unavailable,
    Timeout,
    Cancelled,
    Unknown,
}

impl Outcome {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotFound => "not_found",
            Self::TooManyRows => "too_many_rows",
            Self::TooLarge => "too_large",
            Self::TooManyParts => "too_many_parts",
            Self::Encode => "encode",
            Self::Decode => "decode",
            Self::Rejected => "rejected",
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Unknown => "outcome_unknown",
        }
    }

    const fn error_type(self) -> Option<&'static str> {
        match self {
            Self::Ok => None,
            _ => Some(self.label()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
pub(crate) enum InsertModeLabel {
    Batch,
    Async,
}

impl InsertModeLabel {
    const fn label(self) -> &'static str {
        match self {
            Self::Batch => "batch",
            Self::Async => "async",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
pub(crate) enum PasswordRefreshOutcome {
    Unchanged,
    Updated,
    ReadFailed,
}

impl PasswordRefreshOutcome {
    const fn label(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Updated => "updated",
            Self::ReadFailed => "read_failed",
        }
    }
}

#[derive(derive_more::Debug)]
#[debug("ClientMetrics {{ .. }}")]
pub(crate) struct ClientMetrics {
    histograms: EnumMap<Operation, EnumMap<Outcome, OnceLock<Histogram>>>,
    admission_refused: EnumMap<Operation, OnceLock<Counter>>,
    insert_rows: EnumMap<InsertModeLabel, OnceLock<Counter>>,
    password_refreshes: EnumMap<PasswordRefreshOutcome, OnceLock<Counter>>,
    in_flight: Gauge,
    read_rows: Counter,
    read_bytes: Counter,
}

impl ClientMetrics {
    pub(crate) fn register() -> Self {
        let in_flight = metrics::gauge!(OPERATIONS_IN_FLIGHT_METRIC);
        in_flight.increment(0.0);
        Self {
            histograms: EnumMap::default(),
            admission_refused: EnumMap::default(),
            insert_rows: EnumMap::default(),
            password_refreshes: EnumMap::default(),
            in_flight,
            read_rows: metrics::counter!(READ_ROWS_METRIC),
            read_bytes: metrics::counter!(READ_BYTES_METRIC),
        }
    }

    pub(crate) fn record_admission_refused(&self, operation: Operation) {
        self.admission_refused[operation]
            .get_or_init(|| {
                metrics::counter!(
                    ADMISSION_REFUSED_METRIC,
                    "operation" => operation.label(),
                )
            })
            .increment(1);
    }

    pub(crate) fn record_password_refresh(&self, outcome: PasswordRefreshOutcome) {
        self.password_refreshes[outcome]
            .get_or_init(|| {
                metrics::counter!(
                    PASSWORD_REFRESHES_METRIC,
                    "outcome" => outcome.label(),
                )
            })
            .increment(1);
    }

    fn record_insert_ok(&self, mode: InsertModeLabel, rows: u64) {
        self.insert_rows[mode]
            .get_or_init(|| {
                metrics::counter!(
                    INSERT_ROWS_METRIC,
                    "mode" => mode.label(),
                )
            })
            .increment(rows);
    }

    fn record_read_ok(&self, rows: u64, decoded_bytes: u64) {
        self.read_rows.increment(rows);
        self.read_bytes.increment(decoded_bytes);
    }

    fn histogram(&self, operation: Operation, outcome: Outcome) -> &Histogram {
        self.histograms[operation][outcome].get_or_init(|| {
            metrics::histogram!(
                OPERATION_DURATION_METRIC,
                "operation" => operation.label(),
                "outcome" => outcome.label(),
            )
        })
    }
}

struct OperationInner<'a> {
    started: Instant,
    metrics: &'a ClientMetrics,
    operation: Operation,
    summary: &'static str,
    endpoint: &'a EndpointMetadata,
    span: Span,
    query_id: Option<String>,
    dispatched: bool,
}

impl OperationInner<'_> {
    fn abandon(self) {
        let outcome = if self.dispatched {
            Outcome::Unknown
        } else {
            Outcome::Cancelled
        };
        self.finish(
            outcome,
            Some(FailureDetail::reason(FailureReason::FutureDropped)),
        );
    }

    fn finish(self, outcome: Outcome, detail: Option<FailureDetail>) {
        self.metrics.in_flight.decrement(1.0);
        if let Some(error_type) = outcome.error_type() {
            let reason = detail.map(|item| item.reason.label());
            let status_code = detail
                .and_then(|item| item.status_code)
                .map(|code| code.to_string());
            self.span.record("error.type", error_type);
            self.span.record("otel.status_code", "ERROR");
            if let Some(reason) = reason {
                self.span.record("clickhouse.error.reason", reason);
            }
            if let Some(status_code) = status_code.as_deref() {
                self.span.record("db.response.status_code", status_code);
            }
            self.emit_failure_event(outcome, error_type, reason, status_code.as_deref());
        }
        self.metrics
            .histogram(self.operation, outcome)
            .record(self.started.elapsed());
    }

    fn emit_failure_event(
        &self,
        outcome: Outcome,
        error_type: &'static str,
        reason: Option<&'static str>,
        status_code: Option<&str>,
    ) {
        self.span.in_scope(|| {
            macro_rules! emit_failure {
                ($level:expr) => {
                    tracing::event!(
                        $level,
                        db.operation.name = self.operation.db_operation_name(),
                        db.query.summary = self.summary,
                        db.namespace = self.endpoint.database.as_str(),
                        server.address = self.endpoint.host.as_str(),
                        server.port = self.endpoint.port,
                        clickhouse.operation = self.operation.label(),
                        clickhouse.outcome = outcome.label(),
                        clickhouse.error.reason = reason,
                        db.response.status_code = status_code,
                        clickhouse.query_id = self.query_id.as_deref(),
                        error.type = error_type,
                        "clickhouse_operation_failed"
                    )
                };
            }

            match outcome {
                Outcome::Ok => {}
                Outcome::NotFound
                | Outcome::TooManyRows
                | Outcome::TooLarge
                | Outcome::Cancelled => emit_failure!(tracing::Level::DEBUG),
                Outcome::TooManyParts
                | Outcome::Encode
                | Outcome::Decode
                | Outcome::Rejected
                | Outcome::Unavailable
                | Outcome::Timeout
                | Outcome::Unknown => emit_failure!(tracing::Level::WARN),
            }
        });
    }
}

pub(crate) struct OperationGuard<'a>(DropGuard<OperationInner<'a>, fn(OperationInner<'a>)>);

impl<'a> OperationGuard<'a> {
    pub(crate) fn start(
        metrics: &'a ClientMetrics,
        operation: Operation,
        summary: &'static str,
        endpoint: &'a EndpointMetadata,
    ) -> Self {
        metrics.in_flight.increment(1.0);
        let span = tracing::info_span!(
            "clickhouse",
            otel.name = %format_args!("{} {summary}", operation.db_operation_name()),
            otel.kind = "client",
            db.system.name = "clickhouse",
            db.operation.name = operation.db_operation_name(),
            db.query.summary = summary,
            db.namespace = endpoint.database.as_str(),
            server.address = endpoint.host.as_str(),
            server.port = endpoint.port,
            clickhouse.query_id = tracing::field::Empty,
            db.response.status_code = tracing::field::Empty,
            clickhouse.error.reason = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );
        Self(DropGuard::new(
            OperationInner {
                started: Instant::now(),
                metrics,
                operation,
                summary,
                endpoint,
                span,
                query_id: None,
                dispatched: false,
            },
            OperationInner::abandon,
        ))
    }

    pub(crate) fn record_query_id(&mut self, query_id: String) {
        self.0.span.record("clickhouse.query_id", query_id.as_str());
        self.0.query_id = Some(query_id);
    }

    pub(crate) fn mark_dispatched(&mut self) {
        self.0.dispatched = true;
    }

    pub(crate) fn inject_trace_context(
        &self,
        mut client: clickhouse::Client,
    ) -> clickhouse::Client {
        let mut headers = http::HeaderMap::new();
        tracing_opentelemetry_instrumentation_sdk::http::inject_context(
            &tracing_opentelemetry_instrumentation_sdk::find_context_from_tracing(&self.0.span),
            &mut headers,
        );
        for (name, value) in &headers {
            if let Ok(text) = value.to_str() {
                client = client.with_header(name.as_str(), text);
            }
        }
        client
    }

    pub(crate) fn finish_query<T>(
        self,
        result: Result<(T, u64, u64), ObservedFailure<QueryError>>,
    ) -> Result<T, QueryError> {
        let inner = DropGuard::dismiss(self.0);
        match result {
            Ok((value, rows, decoded_bytes)) => {
                inner.metrics.record_read_ok(rows, decoded_bytes);
                inner.finish(Outcome::Ok, None);
                Ok(value)
            }
            Err(failure) => {
                inner.finish(query_error_outcome(failure.error), Some(failure.detail));
                Err(failure.error)
            }
        }
    }

    pub(crate) fn finish_insert(
        self,
        mode: InsertModeLabel,
        result: Result<InsertReport, ObservedFailure<InsertError>>,
    ) -> Result<InsertReport, InsertError> {
        let inner = DropGuard::dismiss(self.0);
        match result {
            Ok(report) => {
                inner.metrics.record_insert_ok(mode, report.rows);
                inner.finish(Outcome::Ok, None);
                Ok(report)
            }
            Err(failure) => {
                inner.finish(insert_error_outcome(failure.error), Some(failure.detail));
                Err(failure.error)
            }
        }
    }
}

const fn query_error_outcome(error: QueryError) -> Outcome {
    match error {
        QueryError::NotFound => Outcome::NotFound,
        QueryError::TooManyRows => Outcome::TooManyRows,
        QueryError::TooLarge => Outcome::TooLarge,
        QueryError::Rejected => Outcome::Rejected,
        QueryError::Decode => Outcome::Decode,
        QueryError::Busy | QueryError::Unavailable | QueryError::ResourceLimit => {
            Outcome::Unavailable
        }
        QueryError::Stopped(Stopped::Deadline) => Outcome::Timeout,
        QueryError::Stopped(Stopped::Cancelled) => Outcome::Cancelled,
    }
}

const fn insert_error_outcome(error: InsertError) -> Outcome {
    match error {
        InsertError::NotApplied(failure) => match failure {
            InsertFailure::TooManyParts => Outcome::TooManyParts,
            InsertFailure::TooLarge => Outcome::TooLarge,
            InsertFailure::Encode => Outcome::Encode,
            InsertFailure::InvalidInput | InsertFailure::Rejected => Outcome::Rejected,
            InsertFailure::Busy | InsertFailure::Unavailable => Outcome::Unavailable,
            InsertFailure::Stopped(Stopped::Deadline) => Outcome::Timeout,
            InsertFailure::Stopped(Stopped::Cancelled) => Outcome::Cancelled,
        },
        InsertError::OutcomeUnknown(_) => Outcome::Unknown,
    }
}

pub(crate) fn describe() {
    metrics::describe_histogram!(
        OPERATION_DURATION_METRIC,
        Unit::Seconds,
        "ClickHouse operation duration in seconds excluding pre-admission checks and empty inserts"
    );
    metrics::describe_gauge!(
        OPERATIONS_IN_FLIGHT_METRIC,
        Unit::Count,
        "Admitted non-empty ClickHouse operations currently in flight"
    );
    metrics::describe_counter!(
        ADMISSION_REFUSED_METRIC,
        Unit::Count,
        "ClickHouse operations shed immediately because read or write concurrency permits are exhausted"
    );
    metrics::describe_counter!(
        INSERT_ROWS_METRIC,
        Unit::Count,
        "Rows confirmed written to ClickHouse by insert_batch or insert_async"
    );
    metrics::describe_counter!(
        READ_ROWS_METRIC,
        Unit::Count,
        "Rows returned from completed ClickHouse read operations"
    );
    metrics::describe_counter!(
        READ_BYTES_METRIC,
        Unit::Bytes,
        "Decoded RowBinary bytes returned from completed ClickHouse read operations"
    );
    metrics::describe_counter!(
        PASSWORD_REFRESHES_METRIC,
        Unit::Count,
        "Periodic ClickHouse password_file refresh attempts by outcome"
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_outcome_has_distinct_label_and_error_type_contract() {
        let outcomes = EnumMap::<Outcome, ()>::default();
        let mut labels = HashSet::new();
        for (outcome, ()) in &outcomes {
            assert!(
                labels.insert(outcome.label()),
                "{} used twice",
                outcome.label()
            );
            if outcome == Outcome::Ok {
                assert_eq!(outcome.error_type(), None);
            } else {
                assert_eq!(outcome.error_type(), Some(outcome.label()));
            }
        }
        assert_eq!(labels.len(), 12);
    }
}
