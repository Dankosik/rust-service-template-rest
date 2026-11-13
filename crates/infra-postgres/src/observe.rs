//! What an operator sees of the pool and of each transaction.
//!
//! Four questions, one signal each: how full the pool is (the occupancy
//! gauges and their limit), whether callers wait for a connection (the
//! transaction-only wait histogram and named acquisition events), how long
//! a transaction holds one and how it ends (the duration histogram and the
//! span), and how long each statement takes and how it fails ([`observed`]).
//! The driver's slow-statement warning adds the SQL text of the ones that matter.

use std::time::{Duration, Instant};

use metrics::{SharedString, Unit};
use sqlx::postgres::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span};

use crate::error::{failure_cause, sqlstate};
use crate::maintenance::{CLEANUP_DURATION_BUCKETS, CLEANUP_DURATION_METRIC};
use crate::transaction::TxError;

/// Successful acquisitions slower than this receive operation diagnostics.
const SLOW_ACQUIRE_THRESHOLD: Duration = Duration::from_secs(1);

/// Pool occupancy, named after the OpenTelemetry database client semantic
/// convention `db.client.connection.count` with its required attributes
/// `db.client.connection.pool.name` and `db.client.connection.state`
/// (`idle`, `used`). The Prometheus exporter spells dots as underscores.
const CONNECTION_COUNT_METRIC: &str = "db_client_connection_count";

/// The pool's connection limit (`db.client.connection.max`); occupancy over
/// it is saturation.
const CONNECTION_MAX_METRIC: &str = "db_client_connection_max";

/// How long a transaction waited for its pooled connection
/// (`db.client.connection.wait_time`), including opening a new one.
pub(crate) const CONNECTION_WAIT_METRIC: &str = "db_client_connection_wait_time_seconds";

/// Buckets in seconds for [`CONNECTION_WAIT_METRIC`], up to the acquire
/// budget.
pub(crate) const CONNECTION_WAIT_BUCKETS: &[f64] = &[
    0.0005,
    0.001,
    0.005,
    0.01,
    0.05,
    0.1,
    0.5,
    1.0,
    crate::ACQUIRE_TIMEOUT.as_secs_f64(),
];

/// How long a transaction took from asking for a connection to its commit
/// answer, or to the closure's error (the rollback is not awaited), by
/// `outcome`.
pub(crate) const TRANSACTION_DURATION_METRIC: &str = "postgres_transaction_duration_seconds";

/// Buckets in seconds for [`TRANSACTION_DURATION_METRIC`], up to the
/// statement budget.
pub(crate) const TRANSACTION_DURATION_BUCKETS: &[f64] = &[
    0.001,
    0.0025,
    0.005,
    0.01,
    0.025,
    0.05,
    0.1,
    0.25,
    0.5,
    1.0,
    2.5,
    crate::pool::STATEMENT_TIMEOUT.as_secs_f64(),
];

const _: () = {
    assert!(strictly_increasing(CONNECTION_WAIT_BUCKETS));
    assert!(strictly_increasing(TRANSACTION_DURATION_BUCKETS));
};

const fn strictly_increasing(buckets: &[f64]) -> bool {
    let mut index = 1;
    while index < buckets.len() {
        if buckets[index - 1] >= buckets[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// How long one statement took from sending it to its complete answer
/// (`db.client.operation.duration`), by `db.query.summary` and, when it
/// failed, `error.type`.
pub(crate) const OPERATION_DURATION_METRIC: &str = "db_client_operation_duration_seconds";

/// Buckets in seconds for [`OPERATION_DURATION_METRIC`]: the boundaries the
/// OpenTelemetry convention advises for this histogram. The composition
/// root passes both to the Prometheus recorder.
pub(crate) const OPERATION_DURATION_BUCKETS: &[f64] =
    &[0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0];

/// Every histogram this crate records. A composition root hands it to the
/// Prometheus recorder; a metric left out renders the default buckets.
pub const HISTOGRAMS: &[(&str, &[f64])] = &[
    (CONNECTION_WAIT_METRIC, CONNECTION_WAIT_BUCKETS),
    (TRANSACTION_DURATION_METRIC, TRANSACTION_DURATION_BUCKETS),
    (OPERATION_DURATION_METRIC, OPERATION_DURATION_BUCKETS),
    (CLEANUP_DURATION_METRIC, CLEANUP_DURATION_BUCKETS),
];

/// Repeating it is harmless.
pub(crate) fn describe() {
    metrics::describe_gauge!(
        CONNECTION_COUNT_METRIC,
        "Pooled PostgreSQL connections by state"
    );
    metrics::describe_gauge!(
        CONNECTION_MAX_METRIC,
        "Upper bound on pooled PostgreSQL connections"
    );
    metrics::describe_histogram!(
        CONNECTION_WAIT_METRIC,
        Unit::Seconds,
        "Time a transaction waited for a pooled PostgreSQL connection in seconds"
    );
    metrics::describe_histogram!(
        TRANSACTION_DURATION_METRIC,
        Unit::Seconds,
        "PostgreSQL transaction duration in seconds, including the wait for a connection"
    );
    metrics::describe_histogram!(
        OPERATION_DURATION_METRIC,
        Unit::Seconds,
        "PostgreSQL statement duration in seconds"
    );
}

/// Record occupancy and capacity under a stable name unique to this pool in the recorder.
pub fn record_metrics(pool: &PgPool, pool_name: &'static str) {
    let size = f64::from(pool.size());
    #[expect(
        clippy::cast_precision_loss,
        reason = "a metric gauge is f64; counts stay far below 2^52"
    )]
    let idle = pool.num_idle() as f64;
    metrics::gauge!(
        CONNECTION_COUNT_METRIC,
        "db.client.connection.pool.name" => pool_name,
        "db.client.connection.state" => "idle"
    )
    .set(idle);
    metrics::gauge!(
        CONNECTION_COUNT_METRIC,
        "db.client.connection.pool.name" => pool_name,
        "db.client.connection.state" => "used"
    )
    .set((size - idle).max(0.0));
    metrics::gauge!(CONNECTION_MAX_METRIC, "db.client.connection.pool.name" => pool_name)
        .set(f64::from(pool.options().get_max_connections()));
}

/// Observe one pool under the same naming contract as [`record_metrics`].
/// A late tick shifts the schedule instead of bursting to catch up.
pub async fn record_metrics_periodically(
    pool: PgPool,
    pool_name: &'static str,
    interval: Duration,
    cancel: CancellationToken,
) {
    let _ = cancel
        .run_until_cancelled(async {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                record_metrics(&pool, pool_name);
            }
        })
        .await;
}

/// This observer owns no timeout, retry or connection cleanup; `budget` only
/// labels the timeout event.
pub(crate) async fn acquisition<T>(
    operation: &'static str,
    budget: Duration,
    future: impl Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, sqlx::Error> {
    let started = tokio::time::Instant::now();
    let result = future.await;
    let elapsed = started.elapsed();
    match &result {
        Ok(_) if elapsed > SLOW_ACQUIRE_THRESHOLD => tracing::warn!(
            pool = "postgres",
            operation,
            elapsed_seconds = elapsed.as_secs_f64(),
            threshold_seconds = SLOW_ACQUIRE_THRESHOLD.as_secs_f64(),
            "postgres_pool_acquire_slow"
        ),
        Err(sqlx::Error::PoolTimedOut) => tracing::warn!(
            pool = "postgres",
            operation,
            elapsed_seconds = elapsed.as_secs_f64(),
            budget_seconds = budget.as_secs_f64(),
            "postgres_pool_acquire_timeout"
        ),
        _ => {}
    }
    result
}

/// `error.type` of a statement whose caller stopped waiting for it.
const CANCELLED: &str = "cancelled";

/// Run one statement and record how long it took and how it ended.
///
/// `summary` is the statement's `db.query.summary`: its SQL command and the
/// table it targets (`"UPDATE background_jobs"`), written by the caller
/// because nothing here parses SQL. It becomes a metric label and the span
/// name, so it is a literal, never built from a value.
///
/// The statement gets a client span only inside another span. A background
/// loop that polls outside any span would otherwise start a one-span trace
/// per tick; its statements are still measured.
///
/// # Errors
///
/// The statement's own error, unchanged.
pub async fn observed<T>(
    summary: &'static str,
    statement: impl Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, sqlx::Error> {
    let span = if Span::current().is_none() {
        Span::none()
    } else {
        tracing::info_span!(
            "postgres_statement",
            otel.name = summary,
            otel.kind = "client",
            db.system.name = "postgresql",
            db.query.summary = summary,
            db.response.status_code = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        )
    };
    let mut statement_end = StatementEnd {
        started: Instant::now(),
        span: span.clone(),
        summary,
        error_type: Some(SharedString::const_str(CANCELLED)),
    };
    let result = statement.instrument(span).await;
    statement_end.error_type = match &result {
        Ok(_) => None,
        Err(error) => Some(match sqlstate(error) {
            Some(code) => {
                statement_end
                    .span
                    .record("db.response.status_code", code.as_ref());
                SharedString::from(code.into_owned())
            }
            None => SharedString::const_str(failure_cause(error)),
        }),
    };
    result
}

/// One statement being observed. Dropping it records the end it holds;
/// until the statement answers, that is `cancelled`.
struct StatementEnd {
    started: Instant,
    span: Span,
    summary: &'static str,
    error_type: Option<SharedString>,
}

impl Drop for StatementEnd {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed().as_secs_f64();
        match self.error_type.take() {
            None => metrics::histogram!(
                OPERATION_DURATION_METRIC,
                "db.system.name" => "postgresql",
                "db.query.summary" => self.summary
            )
            .record(elapsed),
            Some(error_type) => {
                self.span.record("error.type", error_type.as_ref());
                self.span.record("otel.status_code", "ERROR");
                metrics::histogram!(
                    OPERATION_DURATION_METRIC,
                    "db.system.name" => "postgresql",
                    "db.query.summary" => self.summary,
                    "error.type" => error_type
                )
                .record(elapsed);
            }
        }
    }
}

/// Every `outcome` label of [`TRANSACTION_DURATION_METRIC`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransactionOutcome {
    Committed,
    /// The closure returned `Err`. Dropping the transaction queues rollback
    /// without waiting for PostgreSQL to acknowledge it.
    RolledBack,
    AcquireFailed,
    BeginFailed,
    CommitFailed,
    CommitUnknown,
    /// The caller dropped the transaction before it ended.
    Cancelled,
}

// One table feeds both the span label and the histogram so each value has a
// single spelling. `metrics` keeps a static key only for literal label
// values; a runtime value allocates the label vector and its retained copy on
// every transaction.
macro_rules! transaction_outcomes {
    ($($variant:ident => $label:literal),* $(,)?) => {
        impl TransactionOutcome {
            pub(crate) const fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label,)*
                }
            }

            fn duration_histogram(self) -> metrics::Histogram {
                match self {
                    $(Self::$variant => metrics::histogram!(
                        TRANSACTION_DURATION_METRIC,
                        "outcome" => $label
                    ),)*
                }
            }
        }
    };
}

transaction_outcomes!(
    Committed => "committed",
    RolledBack => "rolled_back",
    AcquireFailed => "acquire_failed",
    BeginFailed => "begin_failed",
    CommitFailed => "commit_failed",
    CommitUnknown => "commit_unknown",
    Cancelled => "cancelled",
);

impl TransactionOutcome {
    const fn of(error: &TxError) -> Self {
        match error {
            TxError::Acquire(_) => Self::AcquireFailed,
            TxError::Begin(_) => Self::BeginFailed,
            TxError::CommitFailed(_) => Self::CommitFailed,
            TxError::CommitUnknown(_) => Self::CommitUnknown,
        }
    }
}

/// The client span of one transaction. It carries the target the operator
/// configured and no statement text; the outcome and a bounded `error.type`
/// are recorded when the transaction ends.
pub(crate) fn transaction_span(pool: &PgPool) -> Span {
    let target = pool.connect_options();
    tracing::info_span!(
        "postgres_transaction",
        otel.kind = "client",
        db.system.name = "postgresql",
        db.namespace = target.get_database(),
        server.address = target.get_host(),
        server.port = target.get_port(),
        postgres.transaction.outcome = tracing::field::Empty,
        error.type = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
    )
}

/// One transaction being observed. Dropping it records the outcome it holds;
/// until the boundary sets one, that is `cancelled`.
pub(crate) struct TransactionObservation {
    started: Instant,
    span: Span,
    outcome: TransactionOutcome,
}

impl TransactionObservation {
    pub(crate) fn start(span: Span) -> Self {
        Self {
            started: Instant::now(),
            span,
            outcome: TransactionOutcome::Cancelled,
        }
    }

    /// Record how long the wait for a connection took, whether or not one
    /// arrived.
    pub(crate) fn waited(&self) {
        metrics::histogram!(
            CONNECTION_WAIT_METRIC,
            "db.client.connection.pool.name" => "postgres"
        )
        .record(self.started.elapsed().as_secs_f64());
    }

    pub(crate) fn end(&mut self, outcome: TransactionOutcome) {
        self.outcome = outcome;
    }

    /// End with the boundary's own failure: its SQLSTATE when the server
    /// supplied one, otherwise the driver's failure class.
    pub(crate) fn fail(&mut self, error: TxError) -> TxError {
        let source = error.driver();
        match sqlstate(source) {
            Some(code) => self.span.record("error.type", code.as_ref()),
            None => self.span.record("error.type", failure_cause(source)),
        };
        self.span.record("otel.status_code", "ERROR");
        self.outcome = TransactionOutcome::of(&error);
        error
    }
}

impl Drop for TransactionObservation {
    fn drop(&mut self) {
        self.span
            .record("postgres.transaction.outcome", self.outcome.label());
        self.outcome
            .duration_histogram()
            .record(self.started.elapsed().as_secs_f64());
    }
}

#[cfg(test)]
pub(crate) mod tests;
