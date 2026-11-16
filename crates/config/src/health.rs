use std::num::NonZeroU32;
use std::time::Duration;

use serde::Deserialize;

use crate::validate::{Violations, duration_range, int_range};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct HealthConfig {
    /// How often readiness is re-checked. Together with [`Self::probe_budget`]
    /// this sizes the staleness bound on the cached verdict, which
    /// `health::RefreshPolicy::stale_after` owns.
    #[serde(with = "humantime_serde")]
    pub refresh_interval: Duration,
    /// One deadline for a background check; every probe runs under it at
    /// the same time, and the verdict names the first probe that failed or
    /// ran out of it. Also feeds the staleness bound described on
    /// [`Self::refresh_interval`].
    #[serde(with = "humantime_serde")]
    pub probe_budget: Duration,
    /// Failed checks in a row before a ready verdict is withdrawn. Some
    /// readers, such as a gRPC health `Watch` client, drop the backend on the
    /// first unready answer with no threshold of their own, so one slow
    /// round-trip must not evict an instance that is still serving.
    /// Admission and a never-ready instance report the first failure at once.
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    pub failure_threshold: NonZeroU32,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(2),
            probe_budget: Duration::from_secs(4),
            failure_threshold: NonZeroU32::MIN.saturating_add(2),
        }
    }
}

impl HealthConfig {
    pub(crate) fn validate(&self, v: &mut Violations) {
        v.check(duration_range(
            "health.refresh_interval",
            self.refresh_interval,
            Duration::from_millis(100)..=Duration::from_mins(5),
        ));
        v.check(duration_range(
            "health.probe_budget",
            self.probe_budget,
            Duration::from_millis(100)..=Duration::from_secs(30),
        ));
        v.check(int_range(
            "health.failure_threshold",
            self.failure_threshold.get(),
            1..=100,
        ));
    }
}
