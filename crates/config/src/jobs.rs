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

/// The only section jobs operator commands read. Unrelated sections are
/// ignored, while the shared loader still enforces namespace and file rules.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct JobsOperatorConfig {
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
        let required = u64::from(self.max_workers.get()) + 2;
        if u64::from(postgres.max_connections.get()) < required {
            return Err(ValidationError::new(
                "postgres.max_connections",
                format!("must be at least jobs.max_workers + 2 ({required}) for the jobs worker"),
            ));
        }
        Ok(())
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
        let (required, mode) = if ordinary_jobs {
            (
                u64::from(self.max_workers.get()) + 5,
                "jobs.max_workers + 5",
            )
        } else {
            (3, "3")
        };
        if u64::from(postgres.max_connections.get()) < required {
            return Err(ValidationError::new(
                "postgres.max_connections",
                format!("must be at least {mode} ({required}) for the outbox worker"),
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
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    fn parse(toml: &str) -> Result<JobsConfig, toml::de::Error> {
        toml::from_str(toml)
    }

    fn workers(max_workers: u32) -> JobsConfig {
        JobsConfig {
            max_workers: NonZeroU32::new(max_workers).unwrap(),
        }
    }

    fn postgres(max_connections: u32) -> PostgresConfig {
        PostgresConfig {
            max_connections: NonZeroU32::new(max_connections).unwrap(),
            ..PostgresConfig::default()
        }
    }

    #[test]
    fn default_is_one_and_validates() {
        let config = JobsConfig::default();
        assert_eq!(config.max_workers, NonZeroU32::MIN);
        config.validate().unwrap();
    }

    #[rstest::rstest]
    #[case::just_above_maximum(501, false)]
    #[case::minimum(1, true)]
    #[case::maximum(500, true)]
    fn validate_bounds(#[case] max_workers: u32, #[case] accepted: bool) {
        let config = workers(max_workers);
        match config.validate() {
            Ok(()) => assert!(accepted),
            Err(err) => {
                assert!(!accepted);
                assert_eq!(err.key, "jobs.max_workers");
            }
        }
    }

    #[test]
    fn zero_max_workers_fails_to_deserialize() {
        let err = parse("max_workers = 0").unwrap_err();
        assert!(err.to_string().contains("max_workers"), "{err}");
    }

    #[test]
    fn toml_sets_max_workers() {
        let config = parse("max_workers = 8").unwrap();
        assert_eq!(config.max_workers.get(), 8);
    }

    #[test]
    fn unknown_keys_in_the_section_fail() {
        assert!(parse("max_workers = 8\nbogus = 1").is_err());
    }

    #[test]
    fn one_worker_needs_three_connections() {
        let err = workers(1).validate_pool_capacity(&postgres(2)).unwrap_err();
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
        workers(1).validate_pool_capacity(&postgres(3)).unwrap();
    }

    #[test]
    fn eight_workers_need_ten_connections() {
        let jobs = workers(8);
        let err = jobs.validate_pool_capacity(&postgres(9)).unwrap_err();
        assert_eq!(err.key, "postgres.max_connections");
        assert!(err.message.contains("(10)"), "{err}");
        jobs.validate_pool_capacity(&postgres(10)).unwrap();
    }

    #[test]
    fn defaults_satisfy_the_pool_bound() {
        JobsConfig::default()
            .validate_pool_capacity(&PostgresConfig::default())
            .unwrap();
    }

    #[test]
    fn five_hundred_workers_refuse_five_hundred_connections() {
        let err = workers(500)
            .validate_pool_capacity(&postgres(500))
            .unwrap_err();
        assert_eq!(err.key, "postgres.max_connections");
    }

    // template:begin outbox:jobs-outbox-capacity-tests
    #[rstest::rstest]
    #[case::outbox_only(500, false, 3)]
    #[case::combined_one(1, true, 6)]
    #[case::combined_eight(8, true, 13)]
    fn publisher_capacity_is_reserved_from_ordinary_slots(
        #[case] max_workers: u32,
        #[case] ordinary_jobs: bool,
        #[case] required: u32,
    ) {
        let jobs = workers(max_workers);
        let error = jobs
            .validate_pool_capacity_with_outbox(&postgres(required - 1), ordinary_jobs)
            .unwrap_err();
        assert_eq!(error.key, "postgres.max_connections");
        jobs.validate_pool_capacity_with_outbox(&postgres(required), ordinary_jobs)
            .unwrap();
    }
    // template:end outbox:jobs-outbox-capacity-tests

    #[test]
    fn jobs_max_workers_is_not_secret_like() {
        assert!(!crate::secret_policy::is_secret_like_key(
            "jobs.max_workers"
        ));
    }
}
