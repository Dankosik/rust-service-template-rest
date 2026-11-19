use std::time::{Duration, SystemTime, UNIX_EPOCH};

use metrics::Unit;
use sqlx::postgres::PgPool;

use super::LOG_TARGET;
use super::cleanup::{CLEANUP_REMOVED_ROWS_METRIC, Cleanup};
use crate::observe::observed;
use crate::transaction::{Isolation, Tx, TxError, TxOptions, in_tx_with};

pub const MAINTENANCE_OBSERVATION_INTERVAL: Duration = Duration::from_secs(30);

const MAINTENANCE_OBSERVATION_BUDGET: Duration = Duration::from_secs(8);

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopulationSample {
    pub observed_at_unix_seconds: f64,
    pub oldest_at_unix_seconds: Option<f64>,
}

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
            metrics::counter!(CLEANUP_REMOVED_ROWS_METRIC, "cleanup" => cleanup.label())
                .increment(0);
        }
        this
    }

    pub async fn sample(
        &mut self,
        read: impl Future<Output = Result<PopulationSample, MaintenanceFailure>>,
    ) {
        let attempt = self.attempt();
        match tokio::time::timeout(MAINTENANCE_OBSERVATION_BUDGET, read).await {
            Ok(Ok(sample)) => attempt.record_sample(sample),
            Ok(Err(failure)) => attempt.failed(failure),
            Err(_) => attempt.failed(MaintenanceFailure::TimedOut),
        }
    }

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

    fn record_unsuccessful(&mut self, outcome: AttemptOutcome, cause: &'static str) {
        self.last_attempt_success.set(0.0);
        self.observations(outcome).increment(1);
        if !self.failing {
            tracing::warn!(
                target: LOG_TARGET,
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

#[derive(Debug)]
struct MaintenanceAttempt<'a> {
    observer: &'a mut MaintenanceObserver,
    requested: MaintenanceClock,
    outcome: AttemptOutcome,
}

impl MaintenanceAttempt<'_> {
    fn record_sample(self, sample: PopulationSample) {
        self.record_sample_at(sample, MaintenanceClock::now());
    }

    fn validated_receipt_time(
        &self,
        sample: PopulationSample,
        received: MaintenanceClock,
    ) -> Option<f64> {
        let received_at = received.wall?;
        let requested_at = self.requested.wall?;
        let PopulationSample {
            observed_at_unix_seconds: observed_at,
            oldest_at_unix_seconds: oldest,
        } = sample;
        let valid = self.requested.agrees_with(self.observer.previous_clock)
            && received.agrees_with(self.requested)
            && observed_at.is_finite()
            && observed_at > 0.0
            && observed_at >= requested_at - CLOCK_TOLERANCE_SECONDS
            && observed_at <= received_at + CLOCK_TOLERANCE_SECONDS
            && oldest
                .is_none_or(|oldest| oldest.is_finite() && oldest > 0.0 && oldest <= observed_at);
        valid.then_some(received_at)
    }

    fn record_sample_at(mut self, sample: PopulationSample, received: MaintenanceClock) {
        let Some(received_at) = self.validated_receipt_time(sample, received) else {
            self.failed_at(MaintenanceFailure::ClockInvalid, received);
            return;
        };
        let PopulationSample {
            observed_at_unix_seconds: observed_at,
            oldest_at_unix_seconds: oldest,
        } = sample;
        self.observer.previous_clock = received;
        self.observer.last_attempt_success.set(0.0);
        self.observer.database_observed.set(observed_at);
        self.observer.present.set(f64::from(oldest.is_some()));
        self.observer.oldest.set(oldest.unwrap_or(0.0));
        self.observer.clock_valid.set(1.0);
        self.observer.last_success.set(received_at);
        self.observer.last_attempt_success.set(1.0);
        self.outcome = AttemptOutcome::Succeeded;
        self.observer.observations(self.outcome).increment(1);
        if self.observer.failing {
            tracing::info!(
                target: LOG_TARGET,
                population = self.observer.population.label(),
                "postgres_maintenance_observation_resumed"
            );
        }
        self.observer.failing = false;
    }

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
            target: LOG_TARGET,
            population = self.observer.population.label(),
            outcome = self.outcome.label(),
            elapsed_seconds = self.requested.monotonic.elapsed().as_secs_f64(),
            "postgres_maintenance_observation_finished"
        );
    }
}

pub async fn in_observation_tx<F>(
    pool: &PgPool,
    read: F,
) -> Result<PopulationSample, MaintenanceFailure>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<PopulationSample, sqlx::Error>,
{
    in_tx_with(
        pool,
        TxOptions {
            isolation: Isolation::ReadCommitted,
            read_only: true,
        },
        async |tx| -> Result<PopulationSample, MaintenanceFailure> {
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

#[cfg(test)]
mod tests;
