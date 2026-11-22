use std::num::NonZeroU32;

use serde::Deserialize;

use crate::load::{Error, JobsOperatorLoadError};
use crate::postgres::{OperatorPostgresConfig, OperatorPostgresInput};
use crate::validate::{Violations, admit_with, int_range};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct JobsOperatorInput {
    app: OperatorAppInput,
    postgres: OperatorPostgresInput,
}

type OperatorAppInput = crate::app::AppFields<crate::de::Skip, crate::de::Skip>;

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
        let postgres = admit_with(|v| {
            let local_development = self.app.admit_insecure_development(v);
            self.postgres.admit(local_development, v)
        })
        .map_err(Error::from)?
        .ok_or(JobsOperatorLoadError::PostgresDisabled)?;
        Ok(JobsOperatorConfig { postgres })
    }
}

#[serde_with::serde_as]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, educe::Educe)]
#[educe(Default)]
#[serde(deny_unknown_fields, default)]
pub struct JobsConfig {
    /// The most attempts one worker process runs at once
    /// (`APP__JOBS__MAX_WORKERS`), between 1 and 500.
    /// `postgres.max_connections` bounds it further per deployment.
    #[serde_as(as = "crate::de::StrictCount")]
    #[educe(Default(expression = NonZeroU32::MIN))]
    pub max_workers: NonZeroU32,
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
