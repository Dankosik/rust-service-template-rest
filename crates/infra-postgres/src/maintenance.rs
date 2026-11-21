use std::time::Duration;

use tokio_util::sync::CancellationToken;

mod cleanup;
mod population;

pub(crate) use cleanup::{CLEANUP_DURATION_BUCKETS, CLEANUP_DURATION_METRIC};
pub use cleanup::{
    CleanupBatchOptions, CleanupError, CleanupName, CleanupPass, CleanupPassResult, PassEnd,
};
pub use population::{
    MAINTENANCE_OBSERVATION_INTERVAL, MaintenanceFailure, MaintenanceObserver,
    MaintenancePopulation, PopulationName, PopulationSample, in_observation_tx,
};

const LOG_TARGET: &str = module_path!();

const CLEANUP_INTERVAL: Duration = Duration::from_mins(1);

const MAX_LABEL_LEN: usize = 64;

const fn is_valid_label(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_LABEL_LEN || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    let mut index = 1;
    while index < bytes.len() {
        let byte = bytes[index];
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_') {
            return false;
        }
        index += 1;
    }
    true
}

#[derive(Debug)]
pub struct CleanupSchedule {
    interval: tokio::time::Interval,
}

impl CleanupSchedule {
    #[must_use]
    pub fn new(cleanup: CleanupName) -> Self {
        tracing::info!(cleanup = cleanup.as_str(), "postgres_cleanup_scheduled");
        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self { interval }
    }

    pub async fn next(&mut self) {
        self.interval.tick().await;
    }
}

pub async fn run_scheduled_cleanup<E>(
    cleanup: CleanupName,
    runs_metric: Option<&'static str>,
    cancel: &CancellationToken,
    mut pass: impl AsyncFnMut() -> Result<PassEnd, E>,
    mut sample: impl AsyncFnMut() -> Result<PopulationSample, MaintenanceFailure>,
) {
    if cancel.is_cancelled() {
        return;
    }
    let mut observer = MaintenanceObserver::start(MaintenancePopulation::Cleanup(cleanup));
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
            if let Some(metric) = runs_metric {
                metrics::counter!(metric, "outcome" => outcome).increment(1);
            }
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
