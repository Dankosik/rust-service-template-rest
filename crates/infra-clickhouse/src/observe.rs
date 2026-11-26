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

const _: () = {
    let mut index = 1;
    while index < OPERATION_DURATION_BUCKETS.len() {
        assert!(OPERATION_DURATION_BUCKETS[index - 1] < OPERATION_DURATION_BUCKETS[index]);
        index += 1;
    }
};

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
pub(crate) enum ReadKind {
    One,
    Optional,
    All,
}

impl ReadKind {
    const fn label(self) -> &'static str {
        match self {
            Self::One => "fetch_one",
            Self::Optional => "fetch_optional",
            Self::All => "fetch_all",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
pub(crate) enum InsertKind {
    Batch,
    Async,
}

impl InsertKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Batch => "batch",
            Self::Async => "async",
        }
    }

    const fn operation_label(self) -> &'static str {
        match self {
            Self::Batch => "insert_batch",
            Self::Async => "insert_async",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
pub(crate) enum Operation {
    Read(ReadKind),
    Insert(InsertKind),
}

impl Operation {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Read(kind) => kind.label(),
            Self::Insert(kind) => kind.operation_label(),
        }
    }

    const fn db_operation_name(self) -> &'static str {
        match self {
            Self::Read(_) => "SELECT",
            Self::Insert(_) => "INSERT",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
pub(crate) enum FailureOutcome {
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

impl FailureOutcome {
    pub(crate) const fn label(self) -> &'static str {
        match self {
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
pub(crate) enum Outcome {
    Ok,
    Failed(FailureOutcome),
}

impl Outcome {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed(failure) => failure.label(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReadStats {
    pub(crate) rows: u64,
    pub(crate) decoded_bytes: u64,
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
    insert_rows: EnumMap<InsertKind, OnceLock<Counter>>,
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

    fn record_insert_ok(&self, mode: InsertKind, rows: u64) {
        self.insert_rows[mode]
            .get_or_init(|| {
                metrics::counter!(
                    INSERT_ROWS_METRIC,
                    "mode" => mode.label(),
                )
            })
            .increment(rows);
    }

    fn record_read_ok(&self, stats: ReadStats) {
        self.read_rows.increment(stats.rows);
        self.read_bytes.increment(stats.decoded_bytes);
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

struct OperationInner<'a, S> {
    started: Instant,
    metrics: &'a ClientMetrics,
    summary: &'static str,
    endpoint: &'a EndpointMetadata,
    span: Span,
    query_id: Option<String>,
    state: S,
}

impl<'a, S> OperationInner<'a, S> {
    fn start(
        metrics: &'a ClientMetrics,
        operation: Operation,
        summary: &'static str,
        endpoint: &'a EndpointMetadata,
        state: S,
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
        Self {
            started: Instant::now(),
            metrics,
            summary,
            endpoint,
            span,
            query_id: None,
            state,
        }
    }

    fn record_query_id(&mut self, query_id: String) {
        self.span.record("clickhouse.query_id", query_id.as_str());
        self.query_id = Some(query_id);
    }

    fn inject_trace_context(&self, mut client: clickhouse::Client) -> clickhouse::Client {
        let mut headers = http::HeaderMap::new();
        tracing_opentelemetry_instrumentation_sdk::http::inject_context(
            &tracing_opentelemetry_instrumentation_sdk::find_context_from_tracing(&self.span),
            &mut headers,
        );
        for (name, value) in &headers {
            if let Ok(text) = value.to_str() {
                client = client.with_header(name.as_str(), text);
            }
        }
        client
    }

    fn finish_ok(self, operation: Operation) {
        self.metrics.in_flight.decrement(1.0);
        self.metrics
            .histogram(operation, Outcome::Ok)
            .record(self.started.elapsed());
    }

    fn finish_err(self, operation: Operation, outcome: FailureOutcome, detail: FailureDetail) {
        self.metrics.in_flight.decrement(1.0);
        let error_type = outcome.label();
        let reason = detail.reason.label();
        let status_code = detail.status_code.map(|code| code.to_string());
        self.span.record("error.type", error_type);
        self.span.record("otel.status_code", "ERROR");
        self.span.record("clickhouse.error.reason", reason);
        if let Some(status_code) = status_code.as_deref() {
            self.span.record("db.response.status_code", status_code);
        }
        self.emit_failure_event(operation, outcome, reason, status_code.as_deref());
        self.metrics
            .histogram(operation, Outcome::Failed(outcome))
            .record(self.started.elapsed());
    }

    fn emit_failure_event(
        &self,
        operation: Operation,
        outcome: FailureOutcome,
        reason: &'static str,
        status_code: Option<&str>,
    ) {
        let error_type = outcome.label();
        self.span.in_scope(|| {
            macro_rules! emit_failure {
                ($level:expr) => {
                    tracing::event!(
                        $level,
                        db.operation.name = operation.db_operation_name(),
                        db.query.summary = self.summary,
                        db.namespace = self.endpoint.database.as_str(),
                        server.address = self.endpoint.host.as_str(),
                        server.port = self.endpoint.port,
                        clickhouse.operation = operation.label(),
                        clickhouse.outcome = error_type,
                        clickhouse.error.reason = reason,
                        db.response.status_code = status_code,
                        clickhouse.query_id = self.query_id.as_deref(),
                        error.type = error_type,
                        "clickhouse_operation_failed"
                    )
                };
            }

            match outcome {
                FailureOutcome::NotFound
                | FailureOutcome::TooManyRows
                | FailureOutcome::TooLarge
                | FailureOutcome::Cancelled => emit_failure!(tracing::Level::DEBUG),
                FailureOutcome::TooManyParts
                | FailureOutcome::Encode
                | FailureOutcome::Decode
                | FailureOutcome::Rejected
                | FailureOutcome::Unavailable
                | FailureOutcome::Timeout
                | FailureOutcome::Unknown => emit_failure!(tracing::Level::WARN),
            }
        });
    }
}

pub(crate) struct ReadGuard<'a>(
    DropGuard<OperationInner<'a, ReadKind>, fn(OperationInner<'a, ReadKind>)>,
);

impl<'a> ReadGuard<'a> {
    pub(crate) fn start(
        metrics: &'a ClientMetrics,
        kind: ReadKind,
        summary: &'static str,
        endpoint: &'a EndpointMetadata,
    ) -> Self {
        Self(DropGuard::new(
            OperationInner::start(metrics, Operation::Read(kind), summary, endpoint, kind),
            Self::abandon,
        ))
    }

    fn abandon(inner: OperationInner<'_, ReadKind>) {
        let operation = Operation::Read(inner.state);
        inner.finish_err(
            operation,
            FailureOutcome::Cancelled,
            FailureDetail::reason(FailureReason::FutureDropped),
        );
    }

    pub(crate) fn record_query_id(&mut self, query_id: String) {
        self.0.record_query_id(query_id);
    }

    pub(crate) fn inject_trace_context(&self, client: clickhouse::Client) -> clickhouse::Client {
        self.0.inject_trace_context(client)
    }

    pub(crate) fn finish<T>(
        self,
        result: Result<(T, ReadStats), ObservedFailure<QueryError>>,
    ) -> Result<T, QueryError> {
        let inner = DropGuard::dismiss(self.0);
        let operation = Operation::Read(inner.state);
        match result {
            Ok((value, stats)) => {
                inner.metrics.record_read_ok(stats);
                inner.finish_ok(operation);
                Ok(value)
            }
            Err(failure) => {
                inner.finish_err(
                    operation,
                    query_error_outcome(failure.error),
                    failure.detail,
                );
                Err(failure.error)
            }
        }
    }
}

struct InsertState {
    mode: InsertKind,
    dispatched: bool,
}

pub(crate) struct InsertGuard<'a>(
    DropGuard<OperationInner<'a, InsertState>, fn(OperationInner<'a, InsertState>)>,
);

impl<'a> InsertGuard<'a> {
    pub(crate) fn start(
        metrics: &'a ClientMetrics,
        mode: InsertKind,
        summary: &'static str,
        endpoint: &'a EndpointMetadata,
    ) -> Self {
        Self(DropGuard::new(
            OperationInner::start(
                metrics,
                Operation::Insert(mode),
                summary,
                endpoint,
                InsertState {
                    mode,
                    dispatched: false,
                },
            ),
            Self::abandon,
        ))
    }

    fn abandon(inner: OperationInner<'_, InsertState>) {
        let operation = Operation::Insert(inner.state.mode);
        let outcome = if inner.state.dispatched {
            FailureOutcome::Unknown
        } else {
            FailureOutcome::Cancelled
        };
        inner.finish_err(
            operation,
            outcome,
            FailureDetail::reason(FailureReason::FutureDropped),
        );
    }

    pub(crate) fn record_query_id(&mut self, query_id: String) {
        self.0.record_query_id(query_id);
    }

    pub(crate) fn mark_dispatched(&mut self) {
        self.0.state.dispatched = true;
    }

    pub(crate) fn inject_trace_context(&self, client: clickhouse::Client) -> clickhouse::Client {
        self.0.inject_trace_context(client)
    }

    pub(crate) fn finish(
        self,
        result: Result<InsertReport, ObservedFailure<InsertError>>,
    ) -> Result<InsertReport, InsertError> {
        let inner = DropGuard::dismiss(self.0);
        let mode = inner.state.mode;
        let operation = Operation::Insert(mode);
        match result {
            Ok(report) => {
                inner.metrics.record_insert_ok(mode, report.rows);
                inner.finish_ok(operation);
                Ok(report)
            }
            Err(failure) => {
                inner.finish_err(
                    operation,
                    insert_error_outcome(failure.error),
                    failure.detail,
                );
                Err(failure.error)
            }
        }
    }
}

const fn query_error_outcome(error: QueryError) -> FailureOutcome {
    match error {
        QueryError::NotFound => FailureOutcome::NotFound,
        QueryError::TooManyRows => FailureOutcome::TooManyRows,
        QueryError::TooLarge => FailureOutcome::TooLarge,
        QueryError::Rejected => FailureOutcome::Rejected,
        QueryError::Decode => FailureOutcome::Decode,
        QueryError::Busy | QueryError::Unavailable | QueryError::ResourceLimit => {
            FailureOutcome::Unavailable
        }
        QueryError::Stopped(Stopped::Deadline) => FailureOutcome::Timeout,
        QueryError::Stopped(Stopped::Cancelled) => FailureOutcome::Cancelled,
    }
}

const fn insert_error_outcome(error: InsertError) -> FailureOutcome {
    match error {
        InsertError::NotApplied(failure) => match failure {
            InsertFailure::TooManyParts => FailureOutcome::TooManyParts,
            InsertFailure::TooLarge => FailureOutcome::TooLarge,
            InsertFailure::Encode => FailureOutcome::Encode,
            InsertFailure::InvalidInput | InsertFailure::Rejected => FailureOutcome::Rejected,
            InsertFailure::Busy | InsertFailure::Unavailable => FailureOutcome::Unavailable,
            InsertFailure::Stopped(Stopped::Deadline) => FailureOutcome::Timeout,
            InsertFailure::Stopped(Stopped::Cancelled) => FailureOutcome::Cancelled,
        },
        InsertError::OutcomeUnknown(_) => FailureOutcome::Unknown,
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
    fn every_operation_and_outcome_has_distinct_label() {
        let operations = EnumMap::<Operation, ()>::default();
        let mut op_labels = HashSet::new();
        for (operation, ()) in &operations {
            assert!(
                op_labels.insert(operation.label()),
                "{} used twice",
                operation.label()
            );
        }
        assert_eq!(op_labels.len(), 5);

        let outcomes = EnumMap::<Outcome, ()>::default();
        let mut labels = HashSet::new();
        for (outcome, ()) in &outcomes {
            assert!(
                labels.insert(outcome.label()),
                "{} used twice",
                outcome.label()
            );
        }
        assert_eq!(labels.len(), 12);
    }
}
