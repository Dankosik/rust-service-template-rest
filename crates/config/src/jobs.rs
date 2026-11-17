use std::num::NonZeroU32;

use serde::Deserialize;

use crate::load::{Error, JobsOperatorLoadError};
use crate::postgres::{OperatorPostgresConfig, OperatorPostgresInput};
use crate::validate::{Violations, int_range};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct JobsOperatorInput {
    postgres: OperatorPostgresInput,
}

#[derive(Clone, Debug)]
pub struct JobsOperatorConfig {
    postgres: OperatorPostgresConfig,
}

impl JobsOperatorConfig {
    #[must_use]
    pub const fn postgres(&self) -> &OperatorPostgresConfig {
        &self.postgres
    }
}

impl JobsOperatorInput {
    pub(crate) fn admit(self) -> Result<JobsOperatorConfig, JobsOperatorLoadError> {
        let mut violations = Violations::default();
        let postgres = self.postgres.admit(&mut violations);
        let postgres = violations
            .finish(postgres)
            .map_err(Error::from)?
            .ok_or(JobsOperatorLoadError::PostgresDisabled)?;
        Ok(JobsOperatorConfig { postgres })
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
    pub(crate) fn validate(&self, violations: &mut Violations) {
        violations.check(int_range(
            "jobs.max_workers",
            self.max_workers.get(),
            1..=500,
        ));
    }
}

#[cfg(test)]
mod tests;
