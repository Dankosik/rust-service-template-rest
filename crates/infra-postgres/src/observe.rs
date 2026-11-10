//! What an operator sees of the pool and of each transaction.
//!
//! Four questions, one signal each: how full the pool is (the occupancy
//! gauges and their limit), whether callers wait for a connection (the
//! transaction-only wait histogram and named acquisition events), how long
//! a transaction holds one and how it ends (the duration histogram and the
//! span), and how long each statement takes and how it fails ([`observed`]).
//! The driver's slow-statement warning adds the SQL text of the ones that matter.
//! [`CleanupPass`] records activity and confirmed progress across batch transactions.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use metrics::{SharedString, Unit};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use sqlx::postgres::PgPool;
use tracing::{Instrument, Span};

use crate::error::{failure_cause, sqlstate};
use crate::pool::SLOW_ACQUIRE_THRESHOLD;
use crate::transaction::{Isolation, Tx, TxError, TxOptions, in_tx_with};

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
/// budget.
pub const CONNECTION_WAIT_BUCKETS: &[f64] = &[
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
pub const TRANSACTION_DURATION_METRIC: &str = "postgres_transaction_duration_seconds";

/// Buckets in seconds for [`TRANSACTION_DURATION_METRIC`], up to the
/// statement budget.
pub const TRANSACTION_DURATION_BUCKETS: &[f64] = &[
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
    crate::STATEMENT_TIMEOUT.as_secs_f64(),
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
pub const OPERATION_DURATION_METRIC: &str = "db_client_operation_duration_seconds";

/// Buckets in seconds for [`OPERATION_DURATION_METRIC`]: the boundaries the
/// OpenTelemetry convention advises for this histogram. The composition
/// root passes both to the Prometheus recorder.
pub const OPERATION_DURATION_BUCKETS: &[f64] =
    &[0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0];

/// The terminal duration of a cleanup pass, including every admitted wait.
pub const CLEANUP_DURATION_METRIC: &str = "postgres_cleanup_pass_duration_seconds";

/// Buckets in seconds for [`CLEANUP_DURATION_METRIC`]. They reach 3600 s so
/// long passes, which have no deadline, stay visible.
/// Roots override only this name.
pub const CLEANUP_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
    600.0, 1200.0, 1800.0, 3600.0,
];

/// Every histogram this crate records. A composition root hands it to the
/// Prometheus recorder; a metric left out renders the default buckets.
pub const HISTOGRAMS: &[(&str, &[f64])] = &[
    (CONNECTION_WAIT_METRIC, CONNECTION_WAIT_BUCKETS),
    (TRANSACTION_DURATION_METRIC, TRANSACTION_DURATION_BUCKETS),
    (OPERATION_DURATION_METRIC, OPERATION_DURATION_BUCKETS),
    (CLEANUP_DURATION_METRIC, CLEANUP_DURATION_BUCKETS),
];

/// Delay after a population observation attempt, including failed attempts.
pub const MAINTENANCE_OBSERVATION_INTERVAL: Duration = Duration::from_secs(30);

/// One absolute foreground deadline from permit admission through read commit.
const MAINTENANCE_OBSERVATION_BUDGET: Duration = Duration::from_secs(8);

/// Delay between scheduled cleanup passes.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

/// Cadence for the existing scheduled owner; direct cleanup calls do not use it.
#[derive(Debug)]
pub struct CleanupSchedule {
    interval: tokio::time::Interval,
}

impl CleanupSchedule {
    #[must_use]
    pub fn new(cleanup: Cleanup) -> Self {
        tracing::info!(cleanup = cleanup.label(), "postgres_cleanup_scheduled");
        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self { interval }
    }

    /// Wait before a pass; the first wait ends at once. The owner cancels
    /// this future with its existing token.
    pub async fn next(&mut self) {
        self.interval.tick().await;
    }
}

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

    const fn cleanup(self) -> Option<Cleanup> {
        match self {
            Self::Jobs => Some(Cleanup::Jobs),
            Self::HttpIdempotency => Some(Cleanup::HttpIdempotency),
            Self::WebhookReceipts => Some(Cleanup::WebhookReceipts),
            Self::FailedJobs => None,
        }
    }
}

/// Every `cleanup` label of the cleanup metrics: the populations a cleanup
/// pass deletes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cleanup {
    Jobs,
    HttpIdempotency,
    WebhookReceipts,
}

impl Cleanup {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Jobs => "jobs",
            Self::HttpIdempotency => "http_idempotency",
            Self::WebhookReceipts => "webhook_receipts",
        }
    }
}

/// One population read: the database's time and the population's oldest
/// time, both in Unix seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopulationSample {
    pub observed_at: f64,
    /// `None` when the population is empty.
    pub oldest: Option<f64>,
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

/// How far local and database clocks may disagree before a sample is
/// rejected; an operational assumption, not proof of synchronization.
const CLOCK_TOLERANCE_SECONDS: f64 = 5.0;

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
        (now - before - elapsed).abs() <= CLOCK_TOLERANCE_SECONDS
    }
}

const OBSERVER_ENABLED_METRIC: &str = "postgres_maintenance_observer_enabled";
const OBSERVER_STARTED_METRIC: &str = "postgres_maintenance_observer_started_timestamp_seconds";
const LAST_SUCCESS_METRIC: &str = "postgres_maintenance_last_success_timestamp_seconds";
const DATABASE_OBSERVED_METRIC: &str = "postgres_maintenance_database_observed_timestamp_seconds";
const PRESENT_METRIC: &str = "postgres_maintenance_present";
const OLDEST_METRIC: &str = "postgres_maintenance_oldest_timestamp_seconds";
const LAST_ATTEMPT_SUCCESS_METRIC: &str = "postgres_maintenance_last_attempt_success";
const CLOCK_VALID_METRIC: &str = "postgres_maintenance_clock_valid";
const OBSERVATIONS_METRIC: &str = "postgres_maintenance_observations_total";
const CLEANUP_ACTIVE_METRIC: &str = "postgres_cleanup_active_passes";
const CLEANUP_BATCHES_METRIC: &str = "postgres_cleanup_committed_batches_total";
const CLEANUP_REMOVED_ROWS_METRIC: &str = "postgres_cleanup_removed_rows_total";
const CLEANUP_PASSES_METRIC: &str = "postgres_cleanup_passes_total";

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
                OBSERVER_ENABLED_METRIC,
                "Whether this process owns this maintenance population",
            ),
            (
                OBSERVER_STARTED_METRIC,
                "Local Unix time at sampler startup",
            ),
            (
                LAST_SUCCESS_METRIC,
                "Local receipt time of the last valid acknowledged read",
            ),
            (
                DATABASE_OBSERVED_METRIC,
                "Database time belonging to the last valid acknowledged read",
            ),
            (
                PRESENT_METRIC,
                "Whether the last valid population sample was nonempty; NaN before success",
            ),
            (
                OLDEST_METRIC,
                "Oldest population time; zero when empty and NaN before success",
            ),
            (
                LAST_ATTEMPT_SUCCESS_METRIC,
                "Whether the last population attempt succeeded",
            ),
            (
                CLOCK_VALID_METRIC,
                "Whether the last checked local and database clocks were consistent",
            ),
        ] {
            metrics::describe_gauge!(name, description);
        }
        metrics::describe_counter!(
            OBSERVATIONS_METRIC,
            Unit::Count,
            "Population attempts by success, failed, or cancelled outcome"
        );
        let label = population.label();
        let gauge = |name| metrics::gauge!(name, "population" => label);
        let observations = |outcome: AttemptEnd| metrics::counter!(OBSERVATIONS_METRIC, "population" => label, "outcome" => outcome.label());
        let previous_clock = MaintenanceClock::now();
        gauge(OBSERVER_STARTED_METRIC).set(previous_clock.wall.unwrap_or(0.0));
        let this = Self {
            population,
            enabled: gauge(OBSERVER_ENABLED_METRIC),
            last_success: gauge(LAST_SUCCESS_METRIC),
            database_observed: gauge(DATABASE_OBSERVED_METRIC),
            present: gauge(PRESENT_METRIC),
            oldest: gauge(OLDEST_METRIC),
            last_attempt_success: gauge(LAST_ATTEMPT_SUCCESS_METRIC),
            clock_valid: gauge(CLOCK_VALID_METRIC),
            successes: observations(AttemptEnd::Succeeded),
            failures: observations(AttemptEnd::Failed),
            cancellations: observations(AttemptEnd::Cancelled),
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
        if let Some(cleanup) = population.cleanup() {
            // Register the writer before its first pass without resetting progress.
            metrics::counter!(CLEANUP_REMOVED_ROWS_METRIC, "cleanup" => cleanup.label())
                .increment(0);
        }
        this
    }

    /// One attempt within the observation budget, counted from before
    /// `read` waits for a permit or a connection. An invalid sample clock keeps
    /// the preceding dated sample.
    pub async fn sample(
        &mut self,
        read: impl Future<Output = Result<PopulationSample, MaintenanceFailure>>,
    ) {
        let attempt = self.attempt();
        match tokio::time::timeout(MAINTENANCE_OBSERVATION_BUDGET, read).await {
            Ok(Ok(sample)) => attempt.succeeded(sample),
            Ok(Err(failure)) => attempt.failed(failure),
            Err(_) => attempt.failed(MaintenanceFailure::TimedOut),
        }
    }

    /// Dropping the attempt records cancellation.
    fn attempt(&mut self) -> MaintenanceAttempt<'_> {
        MaintenanceAttempt {
            observer: self,
            requested: MaintenanceClock::now(),
            end: AttemptEnd::Cancelled,
        }
    }

    const fn observations(&self, end: AttemptEnd) -> &metrics::Counter {
        match end {
            AttemptEnd::Succeeded => &self.successes,
            AttemptEnd::Failed => &self.failures,
            AttemptEnd::Cancelled => &self.cancellations,
        }
    }

    /// Count an attempt that published no sample; one run of them is one
    /// warning.
    fn record_unsuccessful(&mut self, end: AttemptEnd, cause: &'static str) {
        self.last_attempt_success.set(0.0);
        self.observations(end).increment(1);
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

/// How one observation attempt ended; the `outcome` label of
/// `postgres_maintenance_observations_total`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttemptEnd {
    Succeeded,
    Failed,
    Cancelled,
}

impl AttemptEnd {
    const fn label(self) -> &'static str {
        match self {
            Self::Succeeded => "success",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One observation attempt, including all waits through transaction acknowledgement.
#[derive(Debug)]
struct MaintenanceAttempt<'a> {
    observer: &'a mut MaintenanceObserver,
    requested: MaintenanceClock,
    end: AttemptEnd,
}

impl MaintenanceAttempt<'_> {
    /// Publish only after the read transaction has acknowledged success.
    fn succeeded(self, sample: PopulationSample) {
        self.succeeded_at(sample, MaintenanceClock::now());
    }

    /// An invalid sample is a failed attempt that keeps the last-good one.
    fn succeeded_at(mut self, sample: PopulationSample, received: MaintenanceClock) {
        let PopulationSample {
            observed_at,
            oldest,
        } = sample;
        let valid = self.requested.agrees_with(self.observer.previous_clock)
            && received.agrees_with(self.requested)
            && observed_at.is_finite()
            && observed_at > 0.0
            && self
                .requested
                .wall
                .is_some_and(|start| observed_at >= start - CLOCK_TOLERANCE_SECONDS)
            && received
                .wall
                .is_some_and(|end| observed_at <= end + CLOCK_TOLERANCE_SECONDS)
            && oldest
                .is_none_or(|oldest| oldest.is_finite() && oldest > 0.0 && oldest <= observed_at);
        if !valid {
            self.failed_at(MaintenanceFailure::ClockInvalid, received);
            return;
        }
        self.observer.previous_clock = received;
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
        self.end = AttemptEnd::Succeeded;
        self.observer.observations(self.end).increment(1);
        if self.observer.failing {
            tracing::info!(
                population = self.observer.population.label(),
                "postgres_maintenance_observation_resumed"
            );
        }
        self.observer.failing = false;
    }

    /// A failed read never overwrites its last-good population or sample times.
    fn failed(self, failure: MaintenanceFailure) {
        self.failed_at(failure, MaintenanceClock::now());
    }

    fn failed_at(mut self, failure: MaintenanceFailure, clock: MaintenanceClock) {
        self.end = AttemptEnd::Failed;
        self.observer.previous_clock = clock;
        if failure == MaintenanceFailure::ClockInvalid {
            self.observer.clock_valid.set(0.0);
        }
        self.observer.record_unsuccessful(self.end, failure.label());
    }
}

impl Drop for MaintenanceAttempt<'_> {
    fn drop(&mut self) {
        if self.end == AttemptEnd::Cancelled {
            self.observer.previous_clock = MaintenanceClock::now();
            self.observer
                .record_unsuccessful(self.end, self.end.label());
        }
        tracing::debug!(
            population = self.observer.population.label(),
            outcome = self.end.label(),
            elapsed_seconds = self.requested.monotonic.elapsed().as_secs_f64(),
            "postgres_maintenance_observation_finished"
        );
    }
}

/// Run one population read in a read-only transaction whose limits end a stuck
/// statement with its own failure before the observation budget cancels the
/// attempt.
///
/// # Errors
///
/// The transaction's or the statement's failure class.
pub async fn in_observation_tx<T, F>(pool: &PgPool, read: F) -> Result<T, MaintenanceFailure>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, sqlx::Error>,
{
    in_tx_with(
        pool,
        TxOptions {
            isolation: Isolation::ReadCommitted,
            read_only: true,
        },
        async |tx| -> Result<T, MaintenanceFailure> {
            let read = async || -> Result<T, sqlx::Error> {
                observed(
                    "set observation statement timeout",
                    sqlx::query!("SET LOCAL statement_timeout = '2000ms'").execute(&mut *tx),
                )
                .await?;
                observed(
                    "set observation lock timeout",
                    sqlx::query!("SET LOCAL lock_timeout = '100ms'").execute(&mut *tx),
                )
                .await?;
                observed(
                    "set observation idle timeout",
                    sqlx::query!("SET LOCAL idle_in_transaction_session_timeout = '5000ms'")
                        .execute(&mut *tx),
                )
                .await?;
                read(tx).await
            };
            read()
                .await
                .map_err(|_: sqlx::Error| MaintenanceFailure::Statement)
        },
    )
    .await
}

/// Every `outcome` label of a cleanup pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanupOutcome {
    Completed,
    Failed,
    Cancelled,
}

impl CleanupOutcome {
    const fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Whole-pass cleanup evidence, including direct and concurrent callers.
///
/// Start inside the polled pass before its first wait. Record a batch only
/// after its transaction confirms commit, including an empty terminal batch.
/// Mark the terminal disposition completed or failed; dropping an unfinished
/// pass records cancellation and preserves its earlier confirmed progress.
#[derive(Debug)]
pub struct CleanupPass {
    cleanup: Cleanup,
    started: tokio::time::Instant,
    outcome: CleanupOutcome,
    active: metrics::Gauge,
    batches: metrics::Counter,
    rows: metrics::Counter,
    committed_batches: u64,
    removed_rows: u64,
}

impl CleanupPass {
    #[must_use]
    pub fn start(cleanup: Cleanup) -> Self {
        let started = tokio::time::Instant::now();
        metrics::describe_gauge!(
            CLEANUP_ACTIVE_METRIC,
            Unit::Count,
            "Started PostgreSQL cleanup passes that have not terminated in this process"
        );
        metrics::describe_counter!(
            CLEANUP_BATCHES_METRIC,
            Unit::Count,
            "Cleanup batches confirmed committed, including empty terminal batches"
        );
        metrics::describe_counter!(
            CLEANUP_REMOVED_ROWS_METRIC,
            Unit::Count,
            "Deleted rows confirmed committed by PostgreSQL cleanup"
        );
        metrics::describe_counter!(
            CLEANUP_PASSES_METRIC,
            Unit::Count,
            "Terminated PostgreSQL cleanup passes by outcome"
        );
        metrics::describe_histogram!(
            CLEANUP_DURATION_METRIC,
            Unit::Seconds,
            "Whole PostgreSQL cleanup pass duration including admission and database waits"
        );
        let label = cleanup.label();
        let active = metrics::gauge!(CLEANUP_ACTIVE_METRIC, "cleanup" => label);
        active.increment(1.0);
        Self {
            cleanup,
            started,
            outcome: CleanupOutcome::Cancelled,
            active,
            batches: metrics::counter!(CLEANUP_BATCHES_METRIC, "cleanup" => label),
            rows: metrics::counter!(CLEANUP_REMOVED_ROWS_METRIC, "cleanup" => label),
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
        self.outcome = CleanupOutcome::Completed;
    }

    /// The pass is returning its original error, including unknown commit.
    pub fn failed(&mut self) {
        self.outcome = CleanupOutcome::Failed;
    }
}

impl Drop for CleanupPass {
    fn drop(&mut self) {
        let elapsed_seconds = self.started.elapsed().as_secs_f64();
        let cleanup = self.cleanup.label();
        let outcome = self.outcome.label();
        self.active.decrement(1.0);
        metrics::counter!(
            CLEANUP_PASSES_METRIC,
            "cleanup" => cleanup,
            "outcome" => outcome
        )
        .increment(1);
        metrics::histogram!(
            CLEANUP_DURATION_METRIC,
            "cleanup" => cleanup,
            "outcome" => outcome
        )
        .record(elapsed_seconds);
        tracing::info!(
            cleanup,
            outcome,
            elapsed_seconds,
            committed_batches = self.committed_batches,
            removed_rows = self.removed_rows,
            "postgres_cleanup_pass_finished"
        );
    }
}

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

/// [`acquire`] for a caller whose own deadline bounds the work: the wait lasts
/// until `deadline`, not [`ACQUIRE_TIMEOUT`](crate::ACQUIRE_TIMEOUT). A caller
/// that waits is served once a connection frees, and cutting its wait short
/// refuses work the pool could still do. Pass the deadline of the request or
/// job, never an invented one; without a deadline use [`acquire`].
///
/// The pool bounds each native wait by its fixed acquire timeout, so this waits
/// again while `deadline` allows. Each wait queues behind those that arrived
/// meanwhile; a connection still frees in the order the pool serves it. A wait
/// that ends at `deadline` is the native `PoolTimedOut`, so callers keep one
/// failure to map.
///
/// # Errors
///
/// The native acquisition error, unchanged, or `PoolTimedOut` at `deadline`.
#[expect(
    clippy::disallowed_methods,
    reason = "the acquisition observer owns the native checkout"
)]
pub async fn acquire_until(
    pool: &PgPool,
    operation: &'static str,
    deadline: tokio::time::Instant,
) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    acquisition(
        operation,
        deadline.saturating_duration_since(tokio::time::Instant::now()),
        async {
            loop {
                match tokio::time::timeout_at(deadline, pool.acquire()).await {
                    Err(_elapsed) => return Err(sqlx::Error::PoolTimedOut),
                    Ok(Err(sqlx::Error::PoolTimedOut))
                        if tokio::time::Instant::now() < deadline => {}
                    Ok(result) => return result,
                }
            }
        },
    )
    .await
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
    /// The closure returned `Err`; the boundary rolled back.
    RolledBack,
    AcquireFailed,
    BeginFailed,
    CommitFailed,
    CommitUnknown,
    /// The caller dropped the transaction before it ended.
    Cancelled,
}

impl TransactionOutcome {
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
        metrics::histogram!(TRANSACTION_DURATION_METRIC, "outcome" => self.outcome.label())
            .record(self.started.elapsed().as_secs_f64());
    }
}

#[cfg(test)]
mod tests;
