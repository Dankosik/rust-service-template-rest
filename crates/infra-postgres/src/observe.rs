use std::time::{Duration, Instant};

use metrics::{SharedString, Unit};
use sqlx::postgres::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span};

use crate::error::{failure_cause, sqlstate};
use crate::maintenance::{CLEANUP_DURATION_BUCKETS, CLEANUP_DURATION_METRIC};
use crate::transaction::TxError;

const SLOW_ACQUIRE_THRESHOLD: Duration = Duration::from_secs(1);

const CONNECTION_COUNT_METRIC: &str = "db_client_connection_count";

const CONNECTION_MAX_METRIC: &str = "db_client_connection_max";

pub(crate) const CONNECTION_WAIT_METRIC: &str = "db_client_connection_wait_time_seconds";

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

pub(crate) const TRANSACTION_DURATION_METRIC: &str = "postgres_transaction_duration_seconds";

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

pub(crate) const OPERATION_DURATION_METRIC: &str = "db_client_operation_duration_seconds";

pub(crate) const OPERATION_DURATION_BUCKETS: &[f64] =
    &[0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0];

pub const HISTOGRAMS: &[(&str, &[f64])] = &[
    (CONNECTION_WAIT_METRIC, CONNECTION_WAIT_BUCKETS),
    (TRANSACTION_DURATION_METRIC, TRANSACTION_DURATION_BUCKETS),
    (OPERATION_DURATION_METRIC, OPERATION_DURATION_BUCKETS),
    (CLEANUP_DURATION_METRIC, CLEANUP_DURATION_BUCKETS),
];

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
        "PostgreSQL operation duration in seconds"
    );
}

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

const CANCELLED: &str = "cancelled";

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
    let guard = std::mem::DropGuard::new((Instant::now(), span.clone()), |(started, span)| {
        record_statement_error(started, &span, summary, SharedString::const_str(CANCELLED));
    });
    let result = statement.instrument(span).await;
    let (started, span) = std::mem::DropGuard::dismiss(guard);
    match &result {
        Ok(_) => metrics::histogram!(
            OPERATION_DURATION_METRIC,
            "db.system.name" => "postgresql",
            "db.query.summary" => summary
        )
        .record(started.elapsed().as_secs_f64()),
        Err(error) => {
            let error_type = match sqlstate(error) {
                Some(code) => {
                    span.record("db.response.status_code", code.as_ref());
                    SharedString::from(code.into_owned())
                }
                None => SharedString::const_str(failure_cause(error)),
            };
            record_statement_error(started, &span, summary, error_type);
        }
    }
    result
}

fn record_statement_error(
    started: Instant,
    span: &Span,
    summary: &'static str,
    error_type: SharedString,
) {
    span.record("error.type", error_type.as_ref());
    span.record("otel.status_code", "ERROR");
    metrics::histogram!(
        OPERATION_DURATION_METRIC,
        "db.system.name" => "postgresql",
        "db.query.summary" => summary,
        "error.type" => error_type
    )
    .record(started.elapsed().as_secs_f64());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransactionOutcome {
    Committed,
    RolledBack,
    AcquireFailed,
    BeginFailed,
    CommitFailed,
    CommitUnknown,
    Cancelled,
}

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

pub(crate) struct TransactionObservation {
    started: Instant,
    span: Span,
    outcome: TransactionOutcome,
    commit_started: bool,
}

impl TransactionObservation {
    pub(crate) fn start(span: Span) -> Self {
        Self {
            started: Instant::now(),
            span,
            outcome: TransactionOutcome::Cancelled,
            commit_started: false,
        }
    }

    pub(crate) fn waited(&self) {
        metrics::histogram!(
            CONNECTION_WAIT_METRIC,
            "db.client.connection.pool.name" => "postgres"
        )
        .record(self.started.elapsed().as_secs_f64());
    }

    pub(crate) fn begin_commit(&mut self) {
        self.commit_started = true;
    }

    pub(crate) fn end(&mut self, outcome: TransactionOutcome) {
        self.outcome = outcome;
    }

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
        if self.commit_started && self.outcome == TransactionOutcome::Cancelled {
            tracing::warn!(
                parent: &self.span,
                phase = "commit",
                commit_outcome = "unknown",
                "postgres_transaction_cancelled"
            );
        }
        self.span
            .record("postgres.transaction.outcome", self.outcome.label());
        self.outcome
            .duration_histogram()
            .record(self.started.elapsed().as_secs_f64());
    }
}

#[cfg(test)]
pub(crate) mod tests;
