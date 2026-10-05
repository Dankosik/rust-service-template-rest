//! What an operator sees of the pool and of each transaction.
//!
//! Four questions, one signal each: how full the pool is (the occupancy
//! gauges and their limit), whether callers wait for a connection (the
//! transaction-only wait histogram and named acquisition events), how long
//! a transaction holds one and how it ends (the duration histogram and the
//! span), and how long each statement takes and how it fails ([`observed`]).
//! The driver's slow-statement warning adds the SQL text of the ones that matter.
//! [`CleanupPass`] records activity and confirmed progress across batch transactions.

use std::time::{Duration, Instant};

use metrics::{SharedString, Unit};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use sqlx::postgres::PgPool;
use tracing::{Instrument, Span};

use crate::error::{failure_cause, sqlstate};
use crate::pool::SLOW_ACQUIRE_THRESHOLD;
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
/// answer, or to the closure's error (the rollback is not awaited), by
/// `outcome`.
pub const TRANSACTION_DURATION_METRIC: &str = "postgres_transaction_duration_seconds";

/// Buckets in seconds for [`TRANSACTION_DURATION_METRIC`], up to the
/// statement budget; the composition root passes both to the Prometheus
/// recorder.
pub const TRANSACTION_DURATION_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 8.0,
];

/// How long one statement took from sending it to its complete answer
/// (`db.client.operation.duration`), by `db.query.summary` and, when it
/// failed, `error.type`.
pub const OPERATION_DURATION_METRIC: &str = "db_client_operation_duration_seconds";

/// Buckets in seconds for [`OPERATION_DURATION_METRIC`]: the boundaries the
/// OpenTelemetry convention advises for this histogram. The composition
/// root passes both to the Prometheus recorder.
pub const OPERATION_DURATION_BUCKETS: &[f64] =
    &[0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0];

/// `error.type` of a statement whose caller stopped waiting for it.
const CANCELLED: &str = "cancelled";

/// Whole-pass cleanup evidence, including direct and concurrent callers.
///
/// Start inside the polled pass before its first wait. Record a batch only
/// after its transaction confirms commit, including an empty terminal batch.
/// Mark a normal return completed or failed; dropping an unfinished pass
/// records cancellation and preserves its earlier confirmed progress.
#[derive(Debug)]
pub struct CleanupPass {
    cleanup: &'static str,
    started: tokio::time::Instant,
    outcome: &'static str,
    active: metrics::Gauge,
    batches: metrics::Counter,
    rows: metrics::Counter,
    committed_batches: u64,
    removed_rows: u64,
}

impl CleanupPass {
    /// `cleanup` is a callsite-owned literal: `jobs`, `http_idempotency`, or
    /// `webhook_receipts`, never a runtime-derived value.
    #[must_use]
    pub fn start(cleanup: &'static str) -> Self {
        let started = tokio::time::Instant::now();
        metrics::describe_gauge!(
            "postgres_cleanup_active_passes",
            Unit::Count,
            "Started PostgreSQL cleanup passes that have not terminated in this process"
        );
        metrics::describe_counter!(
            "postgres_cleanup_committed_batches_total",
            Unit::Count,
            "Cleanup batches confirmed committed, including empty terminal batches"
        );
        metrics::describe_counter!(
            "postgres_cleanup_removed_rows_total",
            Unit::Count,
            "Deleted rows confirmed committed by PostgreSQL cleanup"
        );
        metrics::describe_counter!(
            "postgres_cleanup_passes_total",
            Unit::Count,
            "Terminated PostgreSQL cleanup passes by outcome"
        );
        metrics::describe_histogram!(
            "postgres_cleanup_pass_duration_seconds",
            Unit::Seconds,
            "Whole PostgreSQL cleanup pass duration including admission and database waits"
        );
        let active = metrics::gauge!("postgres_cleanup_active_passes", "cleanup" => cleanup);
        active.increment(1.0);
        Self {
            cleanup,
            started,
            outcome: CANCELLED,
            active,
            batches: metrics::counter!("postgres_cleanup_committed_batches_total", "cleanup" => cleanup),
            rows: metrics::counter!("postgres_cleanup_removed_rows_total", "cleanup" => cleanup),
            committed_batches: 0,
            removed_rows: 0,
        }
    }

    /// Publish a known successful commit before the next wait in the pass.
    pub fn committed(&mut self, rows: u64) {
        self.batches.increment(1);
        self.rows.increment(rows);
        self.committed_batches = self.committed_batches.saturating_add(1);
        self.removed_rows = self.removed_rows.saturating_add(rows);
    }

    /// The pass is returning successfully after a short batch.
    pub fn completed(&mut self) {
        self.outcome = "completed";
    }

    /// The pass is returning its original error, including unknown commit.
    pub fn failed(&mut self) {
        self.outcome = "failed";
    }
}

impl Drop for CleanupPass {
    fn drop(&mut self) {
        let elapsed_seconds = self.started.elapsed().as_secs_f64();
        self.active.decrement(1.0);
        metrics::counter!(
            "postgres_cleanup_passes_total",
            "cleanup" => self.cleanup,
            "outcome" => self.outcome
        )
        .increment(1);
        metrics::histogram!(
            "postgres_cleanup_pass_duration_seconds",
            "cleanup" => self.cleanup,
            "outcome" => self.outcome
        )
        .record(elapsed_seconds);
        tracing::info!(
            cleanup = self.cleanup,
            outcome = self.outcome,
            elapsed_seconds,
            committed_batches = self.committed_batches,
            removed_rows = self.removed_rows,
            "postgres_cleanup_pass_finished"
        );
    }
}

/// Describes the pool, transaction and statement metrics to the installed
/// recorder.
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

/// Acquire a native pooled connection with bounded operation diagnostics.
///
/// `operation` is a callsite-owned static label, never a request or SQL value.
/// Slow successes and native acquire timeouts are logged; cancellation leaves
/// the native future and its ownership to `SQLx` without inventing an outcome.
///
/// # Errors
///
/// The native acquisition error, unchanged.
#[expect(
    clippy::disallowed_methods,
    reason = "the acquisition observer owns the native checkout"
)]
pub async fn acquire(
    pool: &PgPool,
    operation: &'static str,
) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    acquisition(
        operation,
        pool.options().get_acquire_timeout(),
        pool.acquire(),
    )
    .await
}

/// Shared with initial native connection establishment. This observer owns no
/// timeout, retry or connection cleanup.
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

    type EventFields = std::collections::BTreeMap<String, String>;

    #[derive(Clone)]
    struct Events {
        records: std::sync::Arc<std::sync::Mutex<Vec<EventFields>>>,
        level: tracing::Level,
    }

    impl Events {
        fn new(level: tracing::Level) -> Self {
            Self {
                records: std::sync::Arc::default(),
                level,
            }
        }
    }

    impl tracing::Subscriber for Events {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Fields(EventFields);
            impl tracing::field::Visit for Fields {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    self.0.insert(field.name().to_owned(), format!("{value:?}"));
                }
            }
            assert_eq!(*event.metadata().level(), self.level);
            let mut fields = Fields(std::collections::BTreeMap::new());
            event.record(&mut fields);
            self.records.lock().unwrap().push(fields.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn acquisition_reports_only_slow_success_and_native_timeout() {
        let events = Events::new(tracing::Level::WARN);
        let started = Instant::now();
        tracing::subscriber::with_default(events.clone(), || {
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(async {
                    let budget = Duration::from_millis(2500);
                    assert_eq!(
                        acquisition("fast", budget, async { Ok(7) }).await.unwrap(),
                        7
                    );
                    assert_eq!(
                        acquisition("slow", budget, async {
                            tokio::time::sleep(Duration::from_millis(1050)).await;
                            Ok(9)
                        })
                        .await
                        .unwrap(),
                        9
                    );
                    for error in [
                        sqlx::Error::PoolTimedOut,
                        sqlx::Error::PoolClosed,
                        sqlx::Error::Protocol("private server detail".to_owned()),
                    ] {
                        let expected = format!("{error:?}");
                        let returned = acquisition("failed", budget, async { Err::<(), _>(error) })
                            .await
                            .unwrap_err();
                        assert_eq!(format!("{returned:?}"), expected);
                    }
                    assert!(
                        tokio::time::timeout(
                            Duration::from_millis(1),
                            acquisition(
                                "cancelled",
                                budget,
                                std::future::pending::<Result<(), sqlx::Error>>()
                            ),
                        )
                        .await
                        .is_err()
                    );
                });
        });
        let events = events.records.lock().unwrap();
        assert_eq!(events.len(), 2, "{events:?}");
        let slow = &events[0];
        assert_eq!(slow["message"], "postgres_pool_acquire_slow");
        assert_eq!(slow["operation"], "\"slow\"");
        assert_eq!(slow["pool"], "\"postgres\"");
        assert_eq!(slow["threshold_seconds"], "1.0");
        let elapsed = slow["elapsed_seconds"].parse::<f64>().unwrap();
        assert!(elapsed > 1.0 && elapsed <= started.elapsed().as_secs_f64());
        assert_eq!(slow.len(), 5, "no additional disclosure fields");
        let timeout = &events[1];
        assert_eq!(timeout["message"], "postgres_pool_acquire_timeout");
        assert_eq!(timeout["operation"], "\"failed\"");
        assert_eq!(timeout["pool"], "\"postgres\"");
        assert_eq!(timeout["budget_seconds"], "2.5");
        assert!(timeout["elapsed_seconds"].parse::<f64>().unwrap() >= 0.0);
        assert_eq!(timeout.len(), 5, "no raw errors, DSNs or SQL");
    }

    #[test]
    fn cleanup_reports_live_progress_and_one_payload_free_terminal_event_per_pass() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let events = Events::new(tracing::Level::INFO);
        let started = tokio::time::Instant::now();
        metrics::with_local_recorder(&recorder, || {
            tracing::subscriber::with_default(events.clone(), || {
                let _interest =
                    tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
                let mut completed = CleanupPass::start("jobs");
                let mut failed = CleanupPass::start("jobs");
                let mut cancelled = CleanupPass::start("jobs");
                completed.committed(500);
                completed.committed(0);
                failed.committed(500);
                cancelled.committed(500);
                let scrape = recorder.handle().render();
                for line in [
                    "postgres_cleanup_active_passes{cleanup=\"jobs\"} 3",
                    "postgres_cleanup_committed_batches_total{cleanup=\"jobs\"} 4",
                    "postgres_cleanup_removed_rows_total{cleanup=\"jobs\"} 1500",
                ] {
                    assert!(scrape.contains(line), "{line} missing from {scrape}");
                }
                assert!(!scrape.contains("postgres_cleanup_passes_total{"));
                assert!(events.records.lock().unwrap().is_empty());
                completed.completed();
                drop(completed);
                assert!(
                    recorder
                        .handle()
                        .render()
                        .contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 2")
                );
                failed.failed();
                drop(failed);
                assert!(
                    recorder
                        .handle()
                        .render()
                        .contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 1")
                );
                drop(cancelled);
            });
        });
        let scrape = recorder.handle().render();
        assert!(scrape.contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 0"));
        for outcome in ["completed", "failed", "cancelled"] {
            for metric in [
                "postgres_cleanup_passes_total",
                "postgres_cleanup_pass_duration_seconds_count",
            ] {
                let line = format!("{metric}{{cleanup=\"jobs\",outcome=\"{outcome}\"}} 1");
                assert!(scrape.contains(&line), "{line} missing from {scrape}");
            }
        }
        let events = events.records.lock().unwrap();
        assert_eq!(events.len(), 3);
        for (event, (outcome, batches)) in
            events
                .iter()
                .zip([("completed", "2"), ("failed", "1"), ("cancelled", "1")])
        {
            assert_eq!(event["message"], "postgres_cleanup_pass_finished");
            assert_eq!(event["cleanup"], "\"jobs\"");
            assert_eq!(event["outcome"], format!("\"{outcome}\""));
            assert_eq!(event["committed_batches"], batches);
            assert_eq!(event["removed_rows"], "500");
            let elapsed = event["elapsed_seconds"].parse::<f64>().unwrap();
            assert!(elapsed >= 0.0 && elapsed <= started.elapsed().as_secs_f64());
            assert_eq!(event.len(), 6, "no identifiers, SQL, errors or payloads");
        }
    }

    #[test]
    fn cleanup_releases_the_original_active_gauge_after_moving_threads() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let pass = metrics::with_local_recorder(&recorder, || CleanupPass::start("jobs"));
        std::thread::spawn(move || drop(pass)).join().unwrap();
        assert!(
            recorder
                .handle()
                .render()
                .contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 0")
        );
    }

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

    #[test]
    fn a_statement_records_its_duration_by_summary_and_by_how_it_failed() {
        let recorder = PrometheusBuilder::new()
            .set_buckets_for_metric(
                Matcher::Full(OPERATION_DURATION_METRIC.to_owned()),
                OPERATION_DURATION_BUCKETS,
            )
            .expect("the buckets are valid")
            .build_recorder();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        metrics::with_local_recorder(&recorder, || {
            runtime.block_on(async {
                assert_eq!(observed("read widget", async { Ok(7) }).await.unwrap(), 7);
                let rejected = observed("write widget", async {
                    Err::<(), _>(crate::error::tests::database("23505"))
                })
                .await;
                assert_eq!(sqlstate(&rejected.unwrap_err()).as_deref(), Some("23505"));
                let lost = observed("write widget", async {
                    Err::<(), _>(sqlx::Error::Io(std::io::Error::other(
                        "reset by db.internal",
                    )))
                })
                .await;
                assert!(lost.is_err());
                // The caller stops waiting before the server answers.
                let abandoned = tokio::time::timeout(
                    Duration::from_millis(1),
                    observed(
                        "slow widget",
                        std::future::pending::<Result<(), sqlx::Error>>(),
                    ),
                )
                .await;
                assert!(abandoned.is_err());
            });
        });
        let scrape = recorder.handle().render();
        let series = |labels: &str| {
            format!(
                "db_client_operation_duration_seconds_count{{db_system_name=\"postgresql\",{labels}}} 1"
            )
        };
        for line in [
            series("db_query_summary=\"read widget\""),
            series("db_query_summary=\"write widget\",error_type=\"23505\""),
            series("db_query_summary=\"write widget\",error_type=\"io\""),
            series("db_query_summary=\"slow widget\",error_type=\"cancelled\""),
        ] {
            assert!(scrape.contains(&line), "{line} is missing from:\n{scrape}");
        }
        assert_eq!(scrape.matches("_count{").count(), 4, "{scrape}");
        assert!(!scrape.contains("db.internal"), "{scrape}");
    }
}
