//! Jobs worker capacity and its PostgreSQL-only operator snapshot.
//!
//! The full snapshot validates worker settings. The worker checks the
//! implied pool bound through [`JobsConfig::validate_pool_capacity`].

use std::num::NonZeroU32;

use serde::Deserialize;

use crate::postgres::{PostgresConfig, PostgresInput};
use crate::validate::{ValidationError, ValidationErrors, Violations, int_range};

const WORKER_MANAGEMENT_CONNECTIONS: u64 = 2;

/// What jobs operator commands decoded; the only section they read. Unrelated
/// sections are ignored, while the shared loader still enforces namespace and
/// file rules.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct JobsOperatorInput {
    postgres: PostgresInput,
}

/// The only section jobs operator commands read, admitted.
#[derive(Clone, Debug, Default)]
pub struct JobsOperatorConfig {
    /// `None` when `postgres.enabled` is false.
    pub postgres: Option<PostgresConfig>,
}

impl JobsOperatorInput {
    pub(crate) fn admit(self) -> Result<JobsOperatorConfig, ValidationErrors> {
        let mut violations = Violations::default();
        let postgres = self.postgres.admit(&mut violations);
        let admitted = postgres.map(|postgres| JobsOperatorConfig { postgres });
        violations.finish(admitted)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct JobsConfig {
    /// The most attempts one worker process runs at once
    /// (`APP__JOBS__MAX_WORKERS`), between 1 and 500.
    /// `postgres.max_connections` bounds it further per deployment.
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    pub max_workers: NonZeroU32,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            max_workers: NonZeroU32::MIN,
        }
    }
}

impl JobsConfig {
    /// One connection per concurrent attempt plus at most two for the
    /// worker's claiming, outcome recording, maintenance, and
    /// readiness probe.
    pub fn validate_pool_capacity(&self, postgres: &PostgresConfig) -> Result<(), ValidationError> {
        let required = self.ordinary_pool_requirement();
        if u64::from(postgres.max_connections.get()) < required {
            return Err(ValidationError::new(
                "postgres.max_connections",
                format!(
                    "must be at least jobs.max_workers + {WORKER_MANAGEMENT_CONNECTIONS} ({required}) for the jobs worker, got {}",
                    postgres.max_connections
                ),
            ));
        }
        Ok(())
    }

    fn ordinary_pool_requirement(&self) -> u64 {
        u64::from(self.max_workers.get()) + WORKER_MANAGEMENT_CONNECTIONS
    }

    // template:begin outbox:jobs-outbox-capacity
    /// Admit the shared pool for one reserved publisher plus any ordinary engine.
    /// The publisher has one slot and two management connections; ordinary work
    /// retains its existing `max_workers + 2` allowance.
    pub fn validate_pool_capacity_with_outbox(
        &self,
        postgres: &PostgresConfig,
        ordinary_jobs: bool,
    ) -> Result<(), ValidationError> {
        const PUBLISHER_CONNECTIONS: u64 = 3;
        let (required, formula) = if ordinary_jobs {
            let reserve = WORKER_MANAGEMENT_CONNECTIONS + PUBLISHER_CONNECTIONS;
            (
                self.ordinary_pool_requirement() + PUBLISHER_CONNECTIONS,
                format!("jobs.max_workers + {reserve}"),
            )
        } else {
            (PUBLISHER_CONNECTIONS, PUBLISHER_CONNECTIONS.to_string())
        };
        if u64::from(postgres.max_connections.get()) < required {
            return Err(ValidationError::new(
                "postgres.max_connections",
                format!(
                    "must be at least {formula} ({required}) for the outbox worker, got {}",
                    postgres.max_connections
                ),
            ));
        }
        Ok(())
    }
    // template:end outbox:jobs-outbox-capacity

    pub(crate) fn validate(&self, violations: &mut Violations) {
        violations.check(int_range(
            "jobs.max_workers",
            u64::from(self.max_workers.get()),
            1,
            500,
        ));
    }
}

#[cfg(test)]
mod tests;
