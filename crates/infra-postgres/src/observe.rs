//! What an operator sees of the pool and of each transaction.
//!
//! Four questions, one signal each: how full the pool is (the occupancy
//! gauges and their limit), whether callers wait for a connection (the
//! transaction-only wait histogram and named acquisition events), how long
//! a transaction holds one and how it ends (the duration histogram and the
//! span), and how long each statement takes and how it fails ([`observed`]).
//! The driver's slow-statement warning adds the SQL text of the ones that matter.
//! [`CleanupPass`] records activity and confirmed progress across batch transactions.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// The terminal duration of a cleanup pass, including every admitted wait.
pub const CLEANUP_DURATION_METRIC: &str = "postgres_cleanup_pass_duration_seconds";

/// Exact-name buckets for cleanup, including older unbounded producers.
pub const CLEANUP_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
    600.0, 1200.0, 1800.0, 3600.0,
];

/// Delay after a population observation attempt, including failed attempts.
pub const MAINTENANCE_OBSERVATION_INTERVAL: Duration = Duration::from_secs(30);

/// One absolute foreground deadline from permit admission through read commit.
pub const MAINTENANCE_OBSERVATION_BUDGET: Duration = Duration::from_secs(8);

// The measurement candidate is P0 until the accepted comparison selects a policy.
// Replay patches change only these constants. Final selection removes unused paths.
const CLEANUP_ADMISSION_BUDGET: Option<Duration> = None;
const CLEANUP_BATCH_BUDGET: Option<Duration> = None;
const CLEANUP_BATCH_PAUSE: Duration = Duration::ZERO;
const CLEANUP_STARTUP_SPREAD: bool = false;
const CLEANUP_FIXED_DELAY: bool = false;

/// Admission and pacing for one pass; the provider still owns every transaction.
#[derive(Debug)]
pub struct CleanupBudget {
    started: tokio::time::Instant,
    first: bool,
    admission: Option<Duration>,
    batch: Option<Duration>,
    pause: Duration,
}

impl CleanupBudget {
    /// Construct inside the polled pass, before its first wait.
    #[must_use]
    pub fn start() -> Self {
        Self {
            started: tokio::time::Instant::now(),
            first: true,
            admission: CLEANUP_ADMISSION_BUDGET,
            batch: CLEANUP_BATCH_BUDGET,
            pause: CLEANUP_BATCH_PAUSE,
        }
    }

    /// Admit the next batch, checking the cutoff before and after any pacing.
    /// Call only after accounting a preceding confirmed full batch.
    pub async fn admit(&mut self) -> bool {
        if self.exhausted() {
            return false;
        }
        if !self.first && !self.pause.is_zero() {
            tokio::time::sleep(self.pause).await;
        }
        self.first = false;
        !self.exhausted()
    }

    fn exhausted(&self) -> bool {
        self.admission
            .is_some_and(|budget| self.started.elapsed() >= budget)
    }

    /// Bound an admitted batch, including its provider permit and commit answer.
    ///
    /// # Errors
    /// Client expiry says nothing about backend termination or commit finality.
    pub async fn batch<T>(
        &self,
        future: impl Future<Output = T>,
    ) -> Result<T, tokio::time::error::Elapsed> {
        match self.batch {
            Some(budget) => tokio::time::timeout(budget, future).await,
            None => Ok(future.await),
        }
    }
}

/// Cadence for the existing scheduled owner; direct cleanup calls do not use it.
#[derive(Debug)]
pub struct CleanupSchedule {
    interval: tokio::time::Interval,
    first_delay: Option<Duration>,
    fixed_delay: bool,
}

impl CleanupSchedule {
    /// Create one process/family-local schedule, with no task or extra owner.
    #[must_use]
    pub fn new(cleanup: &'static str) -> Self {
        let offset = if CLEANUP_STARTUP_SPREAD {
            RandomState::new().hash_one(cleanup) % 30_001
        } else {
            0
        };
        tracing::info!(
            cleanup,
            offset_milliseconds = offset,
            "postgres_cleanup_scheduled"
        );
        Self::with_offset(Duration::from_millis(offset))
    }

    fn with_offset(offset: Duration) -> Self {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self {
            interval,
            first_delay: Some(offset),
            fixed_delay: CLEANUP_FIXED_DELAY,
        }
    }

    /// Wait before a pass. The owner cancels this future with its existing token.
    pub async fn next(&mut self) {
        if self.fixed_delay {
            let delay = self.first_delay.take().unwrap_or(Duration::from_secs(60));
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        } else {
            self.interval.tick().await;
        }
    }
}

/// The complete, finite vocabulary for current maintenance populations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenancePopulation {
    Jobs,
    HttpIdempotency,
    WebhookReceipts,
    FailedJobs,
}

impl MaintenancePopulation {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Jobs => "jobs",
            Self::HttpIdempotency => "http_idempotency",
            Self::WebhookReceipts => "webhook_receipts",
            Self::FailedJobs => "failed_jobs",
        }
    }
}

/// Bounded failure classes; no driver text or arbitrary stored kind is emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceFailure {
    Permit,
    Acquire,
    Begin,
    Statement,
    CommitFailed,
    CommitUnknown,
    TimedOut,
    ClockInvalid,
}

impl MaintenanceFailure {
    const fn label(self) -> &'static str {
        match self {
            Self::Permit => "permit",
            Self::Acquire => "acquire",
            Self::Begin => "begin",
            Self::Statement => "statement",
            Self::CommitFailed => "commit_failed",
            Self::CommitUnknown => "commit_unknown",
            Self::TimedOut => "timeout",
            Self::ClockInvalid => "clock_invalid",
        }
    }
}

impl From<TxError> for MaintenanceFailure {
    fn from(error: TxError) -> Self {
        match error {
            TxError::Acquire(_) => Self::Acquire,
            TxError::Begin(_) => Self::Begin,
            TxError::CommitFailed(_) => Self::CommitFailed,
            TxError::CommitUnknown(_) => Self::CommitUnknown,
        }
    }
}

impl From<sqlx::Error> for MaintenanceFailure {
    fn from(_: sqlx::Error) -> Self {
        Self::Statement
    }
}

#[derive(Clone, Copy, Debug)]
struct MaintenanceClock {
    wall: Option<f64>,
    monotonic: tokio::time::Instant,
}

impl MaintenanceClock {
    fn now() -> Self {
        Self {
            wall: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|time| time.as_secs_f64()),
            monotonic: tokio::time::Instant::now(),
        }
    }

    fn agrees_with(self, earlier: Self) -> bool {
        let (Some(now), Some(before)) = (self.wall, earlier.wall) else {
            return false;
        };
        let elapsed = self
            .monotonic
            .duration_since(earlier.monotonic)
            .as_secs_f64();
        (now - before - elapsed).abs() <= 5.0
    }
}

/// One profile-owned sampler's last-good evidence; this object spawns no work.
#[derive(Debug)]
pub struct MaintenanceObserver {
    population: MaintenancePopulation,
    enabled: metrics::Gauge,
    last_success: metrics::Gauge,
    database_observed: metrics::Gauge,
    present: metrics::Gauge,
    oldest: metrics::Gauge,
    last_attempt_success: metrics::Gauge,
    clock_valid: metrics::Gauge,
    successes: metrics::Counter,
    failures: metrics::Counter,
    cancellations: metrics::Counter,
    previous_clock: MaintenanceClock,
    failing: bool,
}

impl MaintenanceObserver {
    /// Start once for this population's process owner, never once per sample.
    #[must_use]
    pub fn start(population: MaintenancePopulation) -> Self {
        for (name, description) in [
            (
                "postgres_maintenance_observer_enabled",
                "Whether this process owns this maintenance population",
            ),
            (
                "postgres_maintenance_observer_started_timestamp_seconds",
                "Local Unix time at sampler startup",
            ),
            (
                "postgres_maintenance_last_success_timestamp_seconds",
                "Local receipt time of the last valid acknowledged read",
            ),
            (
                "postgres_maintenance_database_observed_timestamp_seconds",
                "Database time belonging to the last valid acknowledged read",
            ),
            (
                "postgres_maintenance_present",
                "Whether the last valid population sample was nonempty; NaN before success",
            ),
            (
                "postgres_maintenance_oldest_timestamp_seconds",
                "Oldest population time; zero when empty and NaN before success",
            ),
            (
                "postgres_maintenance_last_attempt_success",
                "Whether the last population attempt succeeded",
            ),
            (
                "postgres_maintenance_clock_valid",
                "Whether the last checked local and database clocks were consistent",
            ),
        ] {
            metrics::describe_gauge!(name, description);
        }
        metrics::describe_counter!(
            "postgres_maintenance_observations_total",
            Unit::Count,
            "Population attempts by success, failed, or cancelled outcome"
        );
        let label = population.label();
        let gauge = |name| metrics::gauge!(name, "population" => label);
        let previous_clock = MaintenanceClock::now();
        gauge("postgres_maintenance_observer_started_timestamp_seconds")
            .set(previous_clock.wall.unwrap_or(0.0));
        let this = Self {
            population,
            enabled: gauge("postgres_maintenance_observer_enabled"),
            last_success: gauge("postgres_maintenance_last_success_timestamp_seconds"),
            database_observed: gauge("postgres_maintenance_database_observed_timestamp_seconds"),
            present: gauge("postgres_maintenance_present"),
            oldest: gauge("postgres_maintenance_oldest_timestamp_seconds"),
            last_attempt_success: gauge("postgres_maintenance_last_attempt_success"),
            clock_valid: gauge("postgres_maintenance_clock_valid"),
            successes: metrics::counter!("postgres_maintenance_observations_total", "population" => label, "outcome" => "success"),
            failures: metrics::counter!("postgres_maintenance_observations_total", "population" => label, "outcome" => "failed"),
            cancellations: metrics::counter!("postgres_maintenance_observations_total", "population" => label, "outcome" => "cancelled"),
            previous_clock,
            failing: false,
        };
        this.enabled.set(1.0);
        this.last_success.set(0.0);
        this.database_observed.set(0.0);
        this.present.set(f64::NAN);
        this.oldest.set(f64::NAN);
        this.last_attempt_success.set(0.0);
        this.clock_valid.set(0.0);
        this.successes.increment(0);
        this.failures.increment(0);
        this.cancellations.increment(0);
        if population != MaintenancePopulation::FailedJobs {
            // Register the writer before its first pass without resetting progress.
            metrics::counter!("postgres_cleanup_removed_rows_total", "cleanup" => label)
                .increment(0);
        }
        this
    }

    /// Begin before permit/acquisition. Dropping the attempt records cancellation.
    pub fn attempt(&mut self) -> MaintenanceAttempt<'_> {
        MaintenanceAttempt {
            observer: self,
            requested: MaintenanceClock::now(),
            outcome: CANCELLED,
        }
    }

    fn failed(&mut self, cause: &'static str, cancelled: bool) {
        self.last_attempt_success.set(0.0);
        if cancelled {
            self.cancellations.increment(1);
        } else {
            self.failures.increment(1);
        }
        if !self.failing {
            tracing::warn!(
                population = self.population.label(),
                cause,
                "postgres_maintenance_observation_failed"
            );
        }
        self.failing = true;
    }
}

impl Drop for MaintenanceObserver {
    fn drop(&mut self) {
        self.enabled.set(0.0);
    }
}

/// One observation attempt, including all waits through transaction acknowledgement.
#[derive(Debug)]
pub struct MaintenanceAttempt<'a> {
    observer: &'a mut MaintenanceObserver,
    requested: MaintenanceClock,
    outcome: &'static str,
}

impl MaintenanceAttempt<'_> {
    /// Publish only after the read transaction has acknowledged success.
    ///
    /// # Errors
    /// Invalid sample clocks retain the preceding dated sample.
    pub fn succeeded(
        self,
        observed_at: f64,
        oldest: Option<f64>,
    ) -> Result<(), MaintenanceFailure> {
        self.succeeded_at(observed_at, oldest, MaintenanceClock::now())
    }

    fn succeeded_at(
        mut self,
        observed_at: f64,
        oldest: Option<f64>,
        received: MaintenanceClock,
    ) -> Result<(), MaintenanceFailure> {
        let valid = self.requested.agrees_with(self.observer.previous_clock)
            && received.agrees_with(self.requested)
            && observed_at.is_finite()
            && observed_at > 0.0
            && self
                .requested
                .wall
                .is_some_and(|start| observed_at >= start - 5.0)
            && received.wall.is_some_and(|end| observed_at <= end + 5.0)
            && oldest
                .is_none_or(|oldest| oldest.is_finite() && oldest > 0.0 && oldest <= observed_at);
        self.observer.previous_clock = received;
        if !valid {
            self.outcome = "failed";
            self.observer.clock_valid.set(0.0);
            self.observer
                .failed(MaintenanceFailure::ClockInvalid.label(), false);
            return Err(MaintenanceFailure::ClockInvalid);
        }
        // Publish the dated sample before its final success markers. A scrape
        // between writes may conservatively observe unknown, never fresh absence.
        self.observer.last_attempt_success.set(0.0);
        self.observer.database_observed.set(observed_at);
        self.observer
            .present
            .set(if oldest.is_some() { 1.0 } else { 0.0 });
        self.observer.oldest.set(oldest.unwrap_or(0.0));
        self.observer.clock_valid.set(1.0);
        self.observer.last_success.set(received.wall.unwrap_or(0.0));
        self.observer.last_attempt_success.set(1.0);
        self.observer.successes.increment(1);
        self.outcome = "success";
        if self.observer.failing {
            tracing::info!(
                population = self.observer.population.label(),
                "postgres_maintenance_observation_resumed"
            );
        }
        self.observer.failing = false;
        Ok(())
    }

    /// A failed read never overwrites its last-good population or sample times.
    pub fn failed(mut self, failure: MaintenanceFailure) {
        self.outcome = "failed";
        self.observer.previous_clock = MaintenanceClock::now();
        if failure == MaintenanceFailure::ClockInvalid {
            self.observer.clock_valid.set(0.0);
        }
        self.observer.failed(failure.label(), false);
    }
}

impl Drop for MaintenanceAttempt<'_> {
    fn drop(&mut self) {
        if self.outcome == CANCELLED {
            self.observer.previous_clock = MaintenanceClock::now();
            self.observer.failed("cancelled", true);
        }
        tracing::debug!(
            population = self.observer.population.label(),
            outcome = self.outcome,
            elapsed_seconds = self.requested.monotonic.elapsed().as_secs_f64(),
            "postgres_maintenance_observation_finished"
        );
    }
}

/// `error.type` of a statement whose caller stopped waiting for it.
const CANCELLED: &str = "cancelled";

/// Whole-pass cleanup evidence, including direct and concurrent callers.
///
/// Start inside the polled pass before its first wait. Record a batch only
/// after its transaction confirms commit, including an empty terminal batch.
/// Mark the terminal disposition completed, budget exhausted, or failed;
/// dropping an unfinished pass records cancellation and preserves its earlier
/// confirmed progress.
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
            CLEANUP_DURATION_METRIC,
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

    /// Another full batch would exceed the pass admission budget.
    pub fn budget_exhausted(&mut self) {
        self.outcome = "budget_exhausted";
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
            CLEANUP_DURATION_METRIC,
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

    #[tokio::test]
    async fn cleanup_admission_checks_both_sides_of_pacing_without_losing_confirmed_progress() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let _recording = metrics::set_default_local_recorder(&recorder);
        let mut pass = CleanupPass::start("jobs");
        pass.committed(500);
        let mut budget = CleanupBudget {
            started: tokio::time::Instant::now(),
            first: false,
            admission: Some(Duration::from_millis(100)),
            batch: Some(Duration::from_secs(12)),
            pause: Duration::from_millis(200),
        };
        let admission = budget.admit();
        tokio::pin!(admission);
        // Establish that the pre-pacing cutoff did not decide this attempt.
        assert!(futures_util::poll!(&mut admission).is_pending());
        assert!(!admission.await);
        pass.budget_exhausted();
        drop(pass);
        let scrape = recorder.handle().render();
        assert!(scrape.contains("postgres_cleanup_removed_rows_total{cleanup=\"jobs\"} 500"));
        assert!(scrape.contains(
            "postgres_cleanup_passes_total{cleanup=\"jobs\",outcome=\"budget_exhausted\"} 1"
        ));

        // An already expired pass returns synchronously, before another pace.
        let mut expired = CleanupBudget {
            started: tokio::time::Instant::now() - Duration::from_secs(5),
            first: false,
            admission: Some(Duration::from_secs(5)),
            batch: None,
            pause: Duration::from_secs(60),
        };
        let mut admission = std::pin::pin!(expired.admit());
        assert_eq!(
            futures_util::poll!(admission.as_mut()),
            std::task::Poll::Ready(false)
        );
    }

    #[tokio::test]
    async fn cleanup_batch_deadline_distinguishes_ready_acknowledgement_from_pending_work() {
        let budget = CleanupBudget {
            started: tokio::time::Instant::now(),
            first: true,
            admission: Some(Duration::from_secs(5)),
            batch: Some(Duration::ZERO),
            pause: Duration::ZERO,
        };
        assert_eq!(budget.batch(async { 500 }).await.unwrap(), 500);
        assert!(budget.batch(std::future::pending::<u64>()).await.is_err());
    }

    #[tokio::test]
    async fn scheduled_spread_and_subsequent_fixed_delay_do_not_tick_immediately() {
        let mut schedule = CleanupSchedule::with_offset(Duration::from_millis(100));
        schedule.fixed_delay = true;
        {
            let mut first = std::pin::pin!(schedule.next());
            assert!(futures_util::poll!(first.as_mut()).is_pending());
            first.await;
        }
        let mut subsequent = std::pin::pin!(schedule.next());
        assert!(futures_util::poll!(subsequent.as_mut()).is_pending());
    }

    fn population_gauge(
        recorder: &metrics_exporter_prometheus::PrometheusRecorder,
        name: &str,
    ) -> f64 {
        let name = format!("postgres_maintenance_{name}");
        recorder
            .handle()
            .render()
            .lines()
            .find(|line| line.starts_with(&format!("{name}{{population=\"jobs\"}} ")))
            .and_then(|line| line.rsplit_once(' '))
            .unwrap_or_else(|| panic!("missing {name}"))
            .1
            .parse()
            .unwrap()
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "discrete states and retained identical samples require exact equality"
    )]
    fn population_failure_and_cancellation_retain_dated_success_until_recovery() {
        let recorder = PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&recorder, || {
            let mut observer = MaintenanceObserver::start(MaintenancePopulation::Jobs);
            assert!(population_gauge(&recorder, "present").is_nan());
            assert!(population_gauge(&recorder, "oldest_timestamp_seconds").is_nan());
            assert_eq!(
                population_gauge(&recorder, "last_success_timestamp_seconds"),
                0.0
            );
            assert!(
                recorder
                    .handle()
                    .render()
                    .contains("postgres_cleanup_removed_rows_total{cleanup=\"jobs\"} 0")
            );

            let database_time = observer.previous_clock.wall.unwrap();
            let oldest = database_time - 600.0;
            observer
                .attempt()
                .succeeded(database_time, Some(oldest))
                .unwrap();
            let receipt = population_gauge(&recorder, "last_success_timestamp_seconds");
            for cause in [
                MaintenanceFailure::Acquire,
                MaintenanceFailure::TimedOut,
                MaintenanceFailure::CommitUnknown,
            ] {
                observer.attempt().failed(cause);
                assert_eq!(population_gauge(&recorder, "present"), 1.0);
                assert_eq!(
                    population_gauge(&recorder, "oldest_timestamp_seconds"),
                    oldest
                );
                assert_eq!(
                    population_gauge(&recorder, "database_observed_timestamp_seconds"),
                    database_time
                );
                assert_eq!(
                    population_gauge(&recorder, "last_success_timestamp_seconds"),
                    receipt
                );
                assert_eq!(population_gauge(&recorder, "last_attempt_success"), 0.0);
            }
            drop(observer.attempt());
            assert_eq!(
                population_gauge(&recorder, "oldest_timestamp_seconds"),
                oldest
            );
            assert_eq!(
                population_gauge(&recorder, "last_success_timestamp_seconds"),
                receipt
            );
            let scrape = recorder.handle().render();
            for (outcome, value) in [("success", 1), ("failed", 3), ("cancelled", 1)] {
                assert!(
                    scrape.lines().any(|line| line
                        .starts_with("postgres_maintenance_observations_total{")
                        && line.contains(&format!("outcome=\"{outcome}\""))
                        && line.ends_with(&format!(" {value}"))),
                    "{scrape}"
                );
            }
            observer.attempt().succeeded(database_time, None).unwrap();
            assert_eq!(population_gauge(&recorder, "present"), 0.0);
            assert_eq!(population_gauge(&recorder, "oldest_timestamp_seconds"), 0.0);
            assert_eq!(population_gauge(&recorder, "last_attempt_success"), 1.0);
            assert_eq!(population_gauge(&recorder, "clock_valid"), 1.0);
            drop(observer);
            assert_eq!(population_gauge(&recorder, "observer_enabled"), 0.0);
        });
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "discrete states and retained identical samples require exact equality"
    )]
    fn clock_inconsistency_never_replaces_a_valid_population_with_empty() {
        // A stable clock starts at wall=1000. Request mono=1s, receipt mono=2s.
        // Each row violates a distinct clock/sample condition, independently of SQL.
        for (request_wall, receipt_wall, database_time, oldest) in [
            (1001.0, 1002.0, 1008.0, None), // database ahead of receipt bracket
            (1001.0, 1002.0, 995.0, None),  // database behind request bracket
            (1010.0, 1011.0, 1010.0, None), // local jump between attempts
            (990.0, 991.0, 990.0, None),    // local backward movement
            (1001.0, 1012.0, 1001.0, None), // local jump within attempt
            (1001.0, 1002.0, 1001.0, Some(1003.0)), // future oldest value
            (1001.0, 1002.0, f64::NAN, None),
            (1001.0, 1002.0, 1001.0, Some(f64::INFINITY)),
        ] {
            let recorder = PrometheusBuilder::new().build_recorder();
            metrics::with_local_recorder(&recorder, || {
                let mut observer = MaintenanceObserver::start(MaintenancePopulation::Jobs);
                let base = tokio::time::Instant::now();
                let original = MaintenanceClock {
                    wall: Some(1000.0),
                    monotonic: base,
                };
                observer.previous_clock = original;
                let mut initial = observer.attempt();
                initial.requested = original;
                initial.succeeded_at(1000.0, Some(400.0), original).unwrap();
                let mut attempt = observer.attempt();
                attempt.requested = MaintenanceClock {
                    wall: Some(request_wall),
                    monotonic: base + Duration::from_secs(1),
                };
                assert_eq!(
                    attempt.succeeded_at(
                        database_time,
                        oldest,
                        MaintenanceClock {
                            wall: Some(receipt_wall),
                            monotonic: base + Duration::from_secs(2),
                        }
                    ),
                    Err(MaintenanceFailure::ClockInvalid)
                );
                assert_eq!(population_gauge(&recorder, "clock_valid"), 0.0);
                assert_eq!(population_gauge(&recorder, "present"), 1.0);
                assert_eq!(
                    population_gauge(&recorder, "oldest_timestamp_seconds"),
                    400.0
                );
                assert_eq!(
                    population_gauge(&recorder, "last_success_timestamp_seconds"),
                    1000.0
                );
                // A stable new bracket restores observation after the rejected jump.
                let mut resumed = observer.attempt();
                resumed.requested = MaintenanceClock {
                    wall: Some(receipt_wall + 1.0),
                    monotonic: base + Duration::from_secs(3),
                };
                resumed
                    .succeeded_at(
                        receipt_wall + 1.0,
                        None,
                        MaintenanceClock {
                            wall: Some(receipt_wall + 2.0),
                            monotonic: base + Duration::from_secs(4),
                        },
                    )
                    .unwrap();
                assert_eq!(population_gauge(&recorder, "clock_valid"), 1.0);
                assert_eq!(population_gauge(&recorder, "present"), 0.0);
            });
        }
    }

    #[test]
    fn cleanup_buckets_expose_budget_duration_and_upper_tail_overflow_separately() {
        let recorder = PrometheusBuilder::new()
            .set_buckets_for_metric(
                Matcher::Full(CLEANUP_DURATION_METRIC.to_owned()),
                CLEANUP_DURATION_BUCKETS,
            )
            .unwrap()
            .build_recorder();
        metrics::with_local_recorder(&recorder, || {
            let mut yielded = CleanupPass::start("jobs");
            yielded.started -= Duration::from_secs(17);
            yielded.committed(500);
            yielded.budget_exhausted();
            drop(yielded);
            let mut long = CleanupPass::start("jobs");
            long.started -= Duration::from_secs(3601);
            long.completed();
            drop(long);
        });
        let scrape = recorder.handle().render();
        for (outcome, bound, count) in [
            ("budget_exhausted", "15", 0),
            ("budget_exhausted", "30", 1),
            ("completed", "3600", 0),
            ("completed", "+Inf", 1),
        ] {
            assert!(
                scrape.lines().any(|line| line
                    .starts_with("postgres_cleanup_pass_duration_seconds_bucket{")
                    && line.contains(&format!("outcome=\"{outcome}\""))
                    && line.contains(&format!("le=\"{bound}\""))
                    && line.ends_with(&format!(" {count}"))),
                "{scrape}"
            );
        }
        assert!(scrape.contains("postgres_cleanup_removed_rows_total{cleanup=\"jobs\"} 500"));
        assert!(scrape.contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 0"));
    }

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
