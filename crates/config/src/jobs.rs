//! Jobs worker capacity and its PostgreSQL-only operator snapshot.
//!
//! Every binary validates it with the rest of the snapshot; only the
//! `jobs-worker` binary uses it. The pool bound it implies is checked by
//! the worker alone through [`JobsConfig::validate_pool_capacity`], as
//! `http_idempotency.retention` is checked only where it is served.

use std::num::NonZeroU32;

use serde::Deserialize;

use crate::postgres::PostgresConfig;
use crate::validate::{ValidationError, int_range};

const WORKER_MANAGEMENT_CONNECTIONS: u64 = 2;

/// The only section jobs operator commands read. Unrelated sections are
/// ignored, while the shared loader still enforces namespace and file rules.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct JobsOperatorConfig {
    /// The `postgres` section, the only section the jobs operator commands decode.
    pub postgres: PostgresConfig,
}

impl JobsOperatorConfig {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        self.postgres.validate()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct JobsConfig {
    /// The most attempts one worker process runs at once
    /// (`APP__JOBS__MAX_WORKERS`), between 1 and 500.
    /// `postgres.max_connections` bounds it further per deployment.
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
    /// readiness probe. Called only by the worker.
    ///
    /// # Errors
    ///
    /// Returns `postgres.max_connections` when the pool is below
    /// `jobs.max_workers` plus 2.
    pub fn validate_pool_capacity(&self, postgres: &PostgresConfig) -> Result<(), ValidationError> {
        let required = self.ordinary_pool_requirement();
        if u64::from(postgres.max_connections.get()) < required {
            return Err(ValidationError::new(
                "postgres.max_connections",
                format!(
                    "must be at least jobs.max_workers + {WORKER_MANAGEMENT_CONNECTIONS} ({required}) for the jobs worker"
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
    ///
    /// # Errors
    ///
    /// Returns `postgres.max_connections` below three for publication alone,
    /// or below `jobs.max_workers + 5` when ordinary work is also registered.
    pub fn validate_pool_capacity_with_outbox(
        &self,
        postgres: &PostgresConfig,
        ordinary_jobs: bool,
    ) -> Result<(), ValidationError> {
        /// One publication slot plus two management connections.
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
                format!("must be at least {formula} ({required}) for the outbox worker"),
            ));
        }
        Ok(())
    }
    // template:end outbox:jobs-outbox-capacity

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        int_range(
            "jobs.max_workers",
            u64::from(self.max_workers.get()),
            1,
            500,
        )
    }
}

#[cfg(test)]
mod tests;
