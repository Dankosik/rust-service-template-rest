//! Readiness refresh cadence.
//!
//! Probes are evaluated on an interval by a background task, never per
//! request, so a probe route can never consume the dependency capacity it
//! reports on.

use std::time::Duration;

use serde::Deserialize;

use crate::validate::{ValidationError, duration_range, int_range};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct HealthConfig {
    /// How often readiness is re-evaluated.
    #[serde(with = "humantime_serde")]
    pub refresh_interval: Duration,
    /// Consecutive failed evaluations before readiness flips off. One slow
    /// round-trip must not evict an instance that is still serving.
    pub failure_threshold: u32,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(2),
            failure_threshold: 3,
        }
    }
}

impl HealthConfig {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        duration_range(
            "health.refresh_interval",
            self.refresh_interval,
            Duration::from_millis(100),
            Duration::from_secs(300),
        )?;
        int_range(
            "health.failure_threshold",
            u64::from(self.failure_threshold),
            1,
            100,
        )?;
        Ok(())
    }
}
