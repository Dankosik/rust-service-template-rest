use std::time::Duration;

use tokio_util::sync::CancellationToken;

mod cleanup;
mod population;

pub(crate) use cleanup::{CLEANUP_DURATION_BUCKETS, CLEANUP_DURATION_METRIC};
pub use cleanup::{
    Cleanup, CleanupBatchOptions, CleanupError, CleanupPass, CleanupPassResult, PassEnd,
};
pub use population::{
    MAINTENANCE_OBSERVATION_INTERVAL, MaintenanceFailure, MaintenanceObserver,
    MaintenancePopulation, PopulationSample, in_observation_tx,
};

const LOG_TARGET: &str = module_path!();

const CLEANUP_INTERVAL: Duration = Duration::from_mins(1);

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

    pub async fn next(&mut self) {
        self.interval.tick().await;
    }
}

pub async fn run_scheduled_cleanup<E, P, S>(
    cleanup: Cleanup,
    runs_metric: &'static str,
    cancel: &CancellationToken,
    mut pass: impl FnMut() -> P,
    mut sample: impl FnMut() -> S,
) where
    P: Future<Output = Result<PassEnd, E>>,
    S: Future<Output = Result<PopulationSample, MaintenanceFailure>>,
{
    if cancel.is_cancelled() {
        return;
    }
    let mut observer = MaintenanceObserver::start(MaintenancePopulation::cleanup(cleanup));
    let passes = async {
        let mut schedule = CleanupSchedule::new(cleanup);
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
