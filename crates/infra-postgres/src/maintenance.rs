//! Maintenance observation for the tables a profile cleans up.
//!
//! Two signals per retained table owner: how old its oldest eligible row is
//! ([`MaintenanceObserver`]), and what each cleanup pass confirmed it removed
//! ([`CleanupPass`]). `env/monitoring/postgres-maintenance.rules.yml` holds
//! the alert examples built on them.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use metrics::Unit;
use sqlx::postgres::PgPool;
use tokio_util::sync::CancellationToken;

use crate::error::{failure_cause, sqlstate};
use crate::observe::observed;
use crate::transaction::{Isolation, Tx, TxError, TxOptions, in_tx_with, set_statement_timeout};

/// The terminal duration of a cleanup pass, including every admitted wait.
pub(crate) const CLEANUP_DURATION_METRIC: &str = "postgres_cleanup_pass_duration_seconds";

/// Buckets in seconds for [`CLEANUP_DURATION_METRIC`]. They reach 3600 s so
/// long passes, which have no deadline, stay visible.
pub(crate) const CLEANUP_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
    600.0, 1200.0, 1800.0, 3600.0,
];

/// Delay after a population observation attempt, including failed attempts.
pub const MAINTENANCE_OBSERVATION_INTERVAL: Duration = Duration::from_secs(30);

/// One absolute foreground deadline from permit admission through read commit.
const MAINTENANCE_OBSERVATION_BUDGET: Duration = Duration::from_secs(8);

/// Delay between scheduled cleanup passes.
const CLEANUP_INTERVAL: Duration = Duration::from_mins(1);

/// Matches the jobs retention backstop, with room for pool acquisition, the
/// cleanup statement, and transaction acknowledgement.
const CLEANUP_BATCH_BUDGET: Duration = Duration::from_secs(12);
const CLEANUP_STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);

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

/// How a cleanup pass that did not fail ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassEnd {
    Completed,
    /// The pass saw the owner's cancellation between batches.
    Cancelled,
}

/// Run `pass` on the [`CleanupSchedule`] and sample `cleanup`'s population
/// with `sample`, both until `cancel` fires, which drops whichever is in
/// flight. Each pass that ends without cancellation increments `runs_metric`
/// by `outcome` (`completed` or `failed`); a failed pass waits for the next
/// tick.
pub async fn run_scheduled_cleanup<E, P, S>(
    cleanup: Cleanup,
    runs_metric: &'static str,
    cancel: &CancellationToken,
    // Not `AsyncFnMut`: the compiler cannot prove its futures `Send` for a
    // spawned owner.
    mut pass: impl FnMut() -> P,
    mut sample: impl FnMut() -> S,
) where
    P: Future<Output = Result<PassEnd, E>>,
    S: Future<Output = Result<PopulationSample, MaintenanceFailure>>,
{
    if cancel.is_cancelled() {
        return;
    }
    // Started under the same check as the passes, so a cancelled owner never
    // publishes an observer start.
    let mut observer = MaintenanceObserver::start(MaintenancePopulation::Cleanup(cleanup));
    let passes = async {
        let mut schedule = CleanupSchedule::new(cleanup);
        // `run_until_cancelled` polls these loops before the token, so each
        // loop checks it before starting new work.
        while !cancel.is_cancelled() {
            schedule.next().await;
            if cancel.is_cancelled() {
                return;
            }
            let outcome = match pass().await {
                Ok(PassEnd::Cancelled) => return,
                Ok(PassEnd::Completed) => "completed",
                Err(_) => "failed",
            };
            metrics::counter!(runs_metric, "outcome" => outcome).increment(1);
        }
    };
    let observation = async {
        while !cancel.is_cancelled() {
            observer.sample(sample()).await;
            if cancel.is_cancelled() {
                return;
            }
            tokio::time::sleep(MAINTENANCE_OBSERVATION_INTERVAL).await;
        }
    };
    let _ = Box::pin(cancel.run_until_cancelled(async {
        tokio::join!(passes, observation);
    }))
    .await;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenancePopulation {
    Cleanup(Cleanup),
    FailedJobs,
}

impl MaintenancePopulation {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Cleanup(cleanup) => cleanup.label(),
            Self::FailedJobs => "failed_jobs",
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
    pub(crate) const fn label(self) -> &'static str {
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

fn describe_observer() {
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
}

impl MaintenanceObserver {
    /// Start once for this population's process owner, never once per sample.
    #[must_use]
    pub fn start(population: MaintenancePopulation) -> Self {
        describe_observer();
        let label = population.label();
        let gauge = |name| metrics::gauge!(name, "population" => label);
        let observations = |outcome: AttemptOutcome| metrics::counter!(OBSERVATIONS_METRIC, "population" => label, "outcome" => outcome.label());
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
            successes: observations(AttemptOutcome::Succeeded),
            failures: observations(AttemptOutcome::Failed),
            cancellations: observations(AttemptOutcome::Cancelled),
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
        if let MaintenancePopulation::Cleanup(cleanup) = population {
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
            outcome: AttemptOutcome::Cancelled,
        }
    }

    const fn observations(&self, outcome: AttemptOutcome) -> &metrics::Counter {
        match outcome {
            AttemptOutcome::Succeeded => &self.successes,
            AttemptOutcome::Failed => &self.failures,
            AttemptOutcome::Cancelled => &self.cancellations,
        }
    }

    /// Count an attempt that published no sample; one run of them is one
    /// warning.
    fn record_unsuccessful(&mut self, outcome: AttemptOutcome, cause: &'static str) {
        self.last_attempt_success.set(0.0);
        self.observations(outcome).increment(1);
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
enum AttemptOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl AttemptOutcome {
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
    outcome: AttemptOutcome,
}

impl MaintenanceAttempt<'_> {
    /// Publish only after the read transaction has acknowledged success.
    fn succeeded(self, sample: PopulationSample) {
        self.succeeded_at(sample, MaintenanceClock::now());
    }

    /// Whether the local clocks agree across the attempt and the sample's
    /// times fall inside it.
    fn accepts(&self, sample: PopulationSample, received: MaintenanceClock) -> bool {
        let PopulationSample {
            observed_at,
            oldest,
        } = sample;
        self.requested.agrees_with(self.observer.previous_clock)
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
                .is_none_or(|oldest| oldest.is_finite() && oldest > 0.0 && oldest <= observed_at)
    }

    /// An invalid sample is a failed attempt that keeps the last-good one.
    fn succeeded_at(mut self, sample: PopulationSample, received: MaintenanceClock) {
        if !self.accepts(sample, received) {
            self.failed_at(MaintenanceFailure::ClockInvalid, received);
            return;
        }
        let PopulationSample {
            observed_at,
            oldest,
        } = sample;
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
        self.outcome = AttemptOutcome::Succeeded;
        self.observer.observations(self.outcome).increment(1);
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
        self.outcome = AttemptOutcome::Failed;
        self.observer.previous_clock = clock;
        if failure == MaintenanceFailure::ClockInvalid {
            self.observer.clock_valid.set(0.0);
        }
        self.observer
            .record_unsuccessful(self.outcome, failure.label());
    }
}

impl Drop for MaintenanceAttempt<'_> {
    fn drop(&mut self) {
        if self.outcome == AttemptOutcome::Cancelled {
            self.observer.previous_clock = MaintenanceClock::now();
            self.observer
                .record_unsuccessful(self.outcome, self.outcome.label());
        }
        tracing::debug!(
            population = self.observer.population.label(),
            outcome = self.outcome.label(),
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
            limit_observation(tx)
                .await
                .map_err(|_| MaintenanceFailure::Statement)?;
            read(tx).await.map_err(|_| MaintenanceFailure::Statement)
        },
    )
    .await
}

async fn limit_observation(tx: &mut Tx<'_>) -> Result<(), sqlx::Error> {
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
        sqlx::query!("SET LOCAL idle_in_transaction_session_timeout = '5000ms'").execute(&mut *tx),
    )
    .await?;
    Ok(())
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

fn describe_cleanup() {
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

/// The failure class of a cleanup batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CleanupError {
    #[error("acquire")]
    Acquire,
    #[error("begin")]
    Begin,
    #[error("statement")]
    Statement,
    /// The commit failed or its outcome is unknown.
    #[error("commit")]
    Commit,
    /// The batch missed its client-side deadline and may have committed.
    #[error("cleanup batch timed out")]
    TimedOut,
}

struct BatchError {
    class: CleanupError,
    driver: sqlx::Error,
}

impl From<TxError> for BatchError {
    fn from(error: TxError) -> Self {
        let (class, driver) = match error {
            TxError::Acquire(driver) => (CleanupError::Acquire, driver),
            TxError::Begin(driver) => (CleanupError::Begin, driver),
            TxError::CommitFailed(driver) | TxError::CommitUnknown(driver) => {
                (CleanupError::Commit, driver)
            }
        };
        Self { class, driver }
    }
}

impl From<sqlx::Error> for BatchError {
    fn from(driver: sqlx::Error) -> Self {
        Self {
            class: CleanupError::Statement,
            driver,
        }
    }
}

impl CleanupPass {
    #[must_use]
    pub fn start(cleanup: Cleanup) -> Self {
        let started = tokio::time::Instant::now();
        describe_cleanup();
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

    /// Run batches until cancellation between batches or a batch removes
    /// fewer rows than its limit. Count rows only after commit acknowledgement.
    /// A short batch does not establish that the eligible population is empty.
    ///
    /// # Errors
    ///
    /// The failed batch's error. Earlier confirmed batches stay committed.
    pub async fn run_until_short_batch<F>(
        mut self,
        pool: &PgPool,
        options: TxOptions,
        cancel: Option<&CancellationToken>,
        batch_limit: u32,
        removed_metric: &'static str,
        delete: F,
    ) -> Result<(u64, PassEnd), CleanupError>
    where
        F: Copy + AsyncFnOnce(&mut Tx<'_>) -> Result<u64, sqlx::Error>,
    {
        let mut removed = 0;
        loop {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Ok((removed, PassEnd::Cancelled));
            }
            let rows = self.run_batch(pool, options, delete).await?;
            metrics::counter!(removed_metric).increment(rows);
            removed += rows;
            if rows < u64::from(batch_limit) {
                self.completed();
                return Ok((removed, PassEnd::Completed));
            }
        }
    }

    /// Execute one cleanup transaction with a server statement timeout and
    /// a client deadline from acquisition through commit acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns and logs the batch's failure class.
    pub async fn run_batch<F>(
        &mut self,
        pool: &PgPool,
        options: TxOptions,
        delete: F,
    ) -> Result<u64, CleanupError>
    where
        F: AsyncFnOnce(&mut Tx<'_>) -> Result<u64, sqlx::Error>,
    {
        self.await_batch(in_tx_with(
            pool,
            options,
            async |tx| -> Result<u64, BatchError> {
                // The server still bounds its statements if the client stops waiting.
                set_statement_timeout(tx, CLEANUP_STATEMENT_TIMEOUT).await?;
                delete(tx).await.map_err(BatchError::from)
            },
        ))
        .await
    }

    async fn await_batch(
        &mut self,
        batch: impl Future<Output = Result<u64, BatchError>>,
    ) -> Result<u64, CleanupError> {
        let failure = match tokio::time::timeout(CLEANUP_BATCH_BUDGET, batch).await {
            Ok(Ok(rows)) => {
                self.committed(rows);
                return Ok(rows);
            }
            Ok(Err(BatchError { class, driver })) => {
                let message = match self.cleanup {
                    Cleanup::HttpIdempotency => "http_idempotency_cleanup_failed",
                    Cleanup::WebhookReceipts => "webhook_receipt_cleanup_failed",
                    Cleanup::Jobs => "postgres_cleanup_batch_failed",
                };
                tracing::warn!(
                    failure = %class,
                    sqlstate = sqlstate(&driver).as_deref(),
                    cause = failure_cause(&driver),
                    "{message}"
                );
                class
            }
            Err(_) => {
                tracing::warn!(
                    cleanup = self.cleanup.label(),
                    "postgres_cleanup_batch_timed_out"
                );
                CleanupError::TimedOut
            }
        };
        self.failed();
        Err(failure)
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

#[cfg(test)]
mod tests;
