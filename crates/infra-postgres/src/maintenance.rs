//! Maintenance observation for the tables a profile cleans up.
//!
//! Two signals per retained table owner: how old its oldest eligible row is
//! ([`MaintenanceObserver`]), and what each cleanup pass confirmed it removed
//! ([`CleanupPass`]). `env/monitoring/postgres-maintenance.rules.yml` holds
//! the alert examples built on them.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

mod cleanup;
mod population;

pub(crate) use cleanup::{CLEANUP_DURATION_BUCKETS, CLEANUP_DURATION_METRIC};
pub use cleanup::{Cleanup, CleanupError, CleanupPass, PassEnd};
pub use population::{
    MAINTENANCE_OBSERVATION_INTERVAL, MaintenanceFailure, MaintenanceObserver,
    MaintenancePopulation, PopulationSample, in_observation_tx,
};

// Keep the target used by maintenance log filters when a call site moves.
const LOG_TARGET: &str = module_path!();

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

#[cfg(test)]
mod tests;
