//! Jobs worker capacity: the one key the optional jobs pack adds.
//!
//! Every binary validates it with the rest of the snapshot; only the
//! `jobs-worker` binary uses it. The pool bound it implies is checked by
//! the worker alone through [`JobsConfig::required_connections`], as
//! `http_idempotency.retention` is checked only where it is served.

use std::num::NonZeroU32;

use serde::Deserialize;

use crate::postgres::PostgresConfig;
use crate::validate::{ValidationError, int_range};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct JobsConfig {
    /// The most attempts one worker process runs at once
    /// (`APP__JOBS__MAX_WORKERS`), between 1 and 500.
    /// `postgres.max_connections` bounds it further per deployment.
    pub max_workers: u32,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self { max_workers: 1 }
    }
}

impl JobsConfig {
    /// The typed form the engine takes, like
    /// [`PostgresConfig::pool_max_connections`].
    ///
    /// # Errors
    ///
    /// Returns `jobs.max_workers` when the value is 0.
    pub fn max_workers(&self) -> Result<NonZeroU32, ValidationError> {
        NonZeroU32::new(self.max_workers)
            .ok_or_else(|| ValidationError::new("jobs.max_workers", "must be greater than 0"))
    }

    /// One connection per concurrent attempt plus at most two for the
    /// worker's claiming, claim upkeep, outcome recording, maintenance, and
    /// readiness probe. Called only by the worker.
    ///
    /// # Errors
    ///
    /// Returns `postgres.max_connections` when the pool is below
    /// `jobs.max_workers` plus 2.
    pub fn required_connections(&self, postgres: &PostgresConfig) -> Result<(), ValidationError> {
        let required = u64::from(self.max_workers) + 2;
        if u64::from(postgres.max_connections) < required {
            return Err(ValidationError::new(
                "postgres.max_connections",
                format!("must be at least jobs.max_workers + 2 ({required}) for the jobs worker"),
            ));
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        int_range("jobs.max_workers", u64::from(self.max_workers), 1, 500)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Result<JobsConfig, toml::de::Error> {
        toml::from_str(toml)
    }

    fn postgres(max_connections: u32) -> PostgresConfig {
        PostgresConfig {
            max_connections,
            ..PostgresConfig::default()
        }
    }

    #[test]
    fn default_is_one_and_validates() {
        let config = JobsConfig::default();
        assert_eq!(config.max_workers, 1);
        config.validate().unwrap();
        assert_eq!(config.max_workers().unwrap().get(), 1);
    }

    #[rstest::rstest]
    #[case::zero(0, false)]
    #[case::just_above_maximum(501, false)]
    #[case::minimum(1, true)]
    #[case::maximum(500, true)]
    fn validate_bounds(#[case] max_workers: u32, #[case] accepted: bool) {
        let config = JobsConfig { max_workers };
        match config.validate() {
            Ok(()) => assert!(accepted),
            Err(err) => {
                assert!(!accepted);
                assert_eq!(err.key, "jobs.max_workers");
            }
        }
    }

    #[test]
    fn max_workers_rejects_zero() {
        let err = JobsConfig { max_workers: 0 }.max_workers().unwrap_err();
        assert_eq!(err.key, "jobs.max_workers");
        assert_eq!(err.message, "must be greater than 0");
    }

    #[test]
    fn toml_sets_max_workers() {
        let config = parse("max_workers = 8").unwrap();
        assert_eq!(config.max_workers, 8);
    }

    #[test]
    fn unknown_keys_in_the_section_fail() {
        assert!(parse("max_workers = 8\nbogus = 1").is_err());
    }

    #[test]
    fn one_worker_needs_three_connections() {
        let err = JobsConfig { max_workers: 1 }
            .required_connections(&postgres(2))
            .unwrap_err();
        assert_eq!(err.key, "postgres.max_connections");
        assert_eq!(
            err.message,
            "must be at least jobs.max_workers + 2 (3) for the jobs worker"
        );
        assert_eq!(
            err.to_string(),
            "postgres.max_connections: must be at least jobs.max_workers + 2 (3) for the jobs worker"
        );
    }

    #[test]
    fn one_worker_accepts_three_connections() {
        JobsConfig { max_workers: 1 }
            .required_connections(&postgres(3))
            .unwrap();
    }

    #[test]
    fn eight_workers_need_ten_connections() {
        let jobs = JobsConfig { max_workers: 8 };
        let err = jobs.required_connections(&postgres(9)).unwrap_err();
        assert_eq!(err.key, "postgres.max_connections");
        assert!(err.message.contains("(10)"), "{err}");
        jobs.required_connections(&postgres(10)).unwrap();
    }

    #[test]
    fn defaults_satisfy_the_pool_bound() {
        JobsConfig::default()
            .required_connections(&PostgresConfig::default())
            .unwrap();
    }

    #[test]
    fn five_hundred_workers_refuse_five_hundred_connections() {
        let err = JobsConfig { max_workers: 500 }
            .required_connections(&postgres(500))
            .unwrap_err();
        assert_eq!(err.key, "postgres.max_connections");
    }

    #[test]
    fn jobs_max_workers_is_not_secret_like() {
        assert!(!crate::secret_policy::is_secret_like_key(
            "jobs.max_workers"
        ));
    }
}
