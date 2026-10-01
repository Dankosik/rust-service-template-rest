//! What an operator sees of the pool and of each transaction.
//!
//! Three questions, one signal each: how full the pool is (the occupancy
//! gauges and their limit), whether callers wait for a connection (the wait
//! histogram), and how long a transaction holds one and how it ends (the
//! duration histogram and the span). Statements are not instrumented here;
//! the driver's slow-statement warning names the ones that matter.

use std::time::Instant;

use metrics::Unit;
use sqlx::postgres::PgPool;
use tracing::Span;

use crate::error::{failure_cause, sqlstate};
use crate::transaction::TxError;

/// Pool occupancy, named after the OpenTelemetry database client semantic
/// convention `db.client.connection.count` with its required attributes
/// `db.client.connection.pool.name` and `db.client.connection.state`
/// (`idle`, `used`). The Prometheus exporter spells dots as underscores.
pub(crate) const CONNECTION_COUNT_METRIC: &str = "db_client_connection_count";

/// The pool's connection limit (`db.client.connection.max`); occupancy over
/// it is saturation.
pub(crate) const CONNECTION_MAX_METRIC: &str = "db_client_connection_max";

/// How long a transaction waited for its pooled connection
/// (`db.client.connection.wait_time`), including opening a new one.
pub const CONNECTION_WAIT_METRIC: &str = "db_client_connection_wait_time_seconds";

/// Buckets in seconds for [`CONNECTION_WAIT_METRIC`], up to the acquire
/// budget; the composition root passes both to the Prometheus recorder.
pub const CONNECTION_WAIT_BUCKETS: &[f64] = &[0.0005, 0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 3.0];

/// How long a transaction took from asking for a connection to its commit
/// or rollback answer, by `outcome`.
pub const TRANSACTION_DURATION_METRIC: &str = "postgres_transaction_duration_seconds";

/// Buckets in seconds for [`TRANSACTION_DURATION_METRIC`], up to the
/// statement budget; the composition root passes both to the Prometheus
/// recorder.
pub const TRANSACTION_DURATION_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 8.0,
];

/// Describes the pool and transaction metrics to the installed recorder.
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
}

/// Every `outcome` label of [`TRANSACTION_DURATION_METRIC`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Committed,
    /// The closure returned `Err`; the boundary rolled back.
    RolledBack,
    AcquireFailed,
    BeginFailed,
    CommitFailed,
    CommitUnknown,
    /// The caller dropped the transaction before it ended.
    Cancelled,
}

impl Outcome {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::RolledBack => "rolled_back",
            Self::AcquireFailed => "acquire_failed",
            Self::BeginFailed => "begin_failed",
            Self::CommitFailed => "commit_failed",
            Self::CommitUnknown => "commit_unknown",
            Self::Cancelled => "cancelled",
        }
    }

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
pub(crate) struct Observed {
    started: Instant,
    span: Span,
    outcome: Outcome,
}

impl Observed {
    pub(crate) fn start(span: Span) -> Self {
        Self {
            started: Instant::now(),
            span,
            outcome: Outcome::Cancelled,
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

    pub(crate) fn end(&mut self, outcome: Outcome) {
        self.outcome = outcome;
    }

    /// End with the boundary's own failure: its SQLSTATE when the server
    /// supplied one, otherwise the driver's failure class.
    pub(crate) fn fail(&mut self, error: TxError) -> TxError {
        let (TxError::Acquire(source)
        | TxError::Begin(source)
        | TxError::CommitFailed(source)
        | TxError::CommitUnknown(source)) = &error;
        match sqlstate(source) {
            Some(code) => self.span.record("error.type", code.as_ref()),
            None => self.span.record("error.type", failure_cause(source)),
        };
        self.span.record("otel.status_code", "ERROR");
        self.outcome = Outcome::of(&error);
        error
    }
}

impl Drop for Observed {
    fn drop(&mut self) {
        self.span
            .record("postgres.transaction.outcome", self.outcome.label());
        metrics::histogram!(TRANSACTION_DURATION_METRIC, "outcome" => self.outcome.label())
            .record(self.started.elapsed().as_secs_f64());
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
    use sqlx::postgres::PgPoolOptions;

    use super::*;
    use crate::{Dsn, in_tx};

    #[test]
    fn every_boundary_failure_has_its_own_outcome() {
        let io = || sqlx::Error::Io(std::io::Error::other("reset"));
        let outcomes = [
            (TxError::Acquire(io()), "acquire_failed"),
            (TxError::Begin(io()), "begin_failed"),
            (TxError::CommitFailed(io()), "commit_failed"),
            (TxError::CommitUnknown(io()), "commit_unknown"),
        ];
        for (error, label) in outcomes {
            assert_eq!(Outcome::of(&error).label(), label);
        }
    }

    #[test]
    fn a_transaction_that_gets_no_connection_records_its_wait_and_its_outcome() {
        let mut builder = PrometheusBuilder::new();
        for (metric, buckets) in [
            (CONNECTION_WAIT_METRIC, CONNECTION_WAIT_BUCKETS),
            (TRANSACTION_DURATION_METRIC, TRANSACTION_DURATION_BUCKETS),
        ] {
            builder = builder
                .set_buckets_for_metric(Matcher::Full(metric.to_owned()), buckets)
                .expect("the buckets are valid");
        }
        let recorder = builder.build_recorder();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        metrics::with_local_recorder(&recorder, || {
            describe();
            runtime.block_on(async {
                // Port 1 on loopback is refused at once; the pool retries
                // until its acquire budget ends.
                let dsn = Dsn::admit_with_environment(
                    "postgres://app:pw@127.0.0.1:1/app?sslmode=disable",
                    |_| false,
                )
                .expect("the dsn is admitted");
                let pool = PgPoolOptions::new()
                    .acquire_timeout(Duration::from_millis(50))
                    .connect_lazy_with(dsn.connect_options());
                let result: Result<(), TxError> = in_tx(&pool, async |_tx| Ok(())).await;
                assert!(matches!(result, Err(TxError::Acquire(_))), "{result:?}");
            });
            // A transaction dropped before it ended.
            drop(Observed::start(Span::none()));
        });
        let scrape = recorder.handle().render();
        for line in [
            "db_client_connection_wait_time_seconds_count{db_client_connection_pool_name=\"postgres\"} 1",
            "postgres_transaction_duration_seconds_count{outcome=\"acquire_failed\"} 1",
            "postgres_transaction_duration_seconds_count{outcome=\"cancelled\"} 1",
        ] {
            assert!(scrape.contains(line), "{line} is missing from:\n{scrape}");
        }
        // Two outcomes and one pool: nothing else became a series.
        assert_eq!(scrape.matches("_count{").count(), 3, "{scrape}");
        assert!(!scrape.contains("pw"), "{scrape}");
    }
}
