use std::num::NonZeroU32;

use serde::Deserialize;

use crate::postgres::{PostgresConfig, PostgresInput};
use crate::validate::{ValidationError, ValidationErrors, Violations, int_range};

const WORKER_MANAGEMENT_CONNECTIONS: u64 = 2;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct JobsOperatorInput {
    postgres: PostgresInput,
    jobs: serde::de::IgnoredAny,
    // template:begin webhooks:jobs-operator-input-webhooks
    webhooks: serde::de::IgnoredAny,
    // template:end webhooks:jobs-operator-input-webhooks
    // template:begin inbound-webhooks:jobs-operator-input-inbound-webhooks
    inbound_webhooks: serde::de::IgnoredAny,
    // template:end inbound-webhooks:jobs-operator-input-inbound-webhooks
    // template:begin messaging:jobs-operator-input-messaging
    messaging: serde::de::IgnoredAny,
    // template:end messaging:jobs-operator-input-messaging
    // template:begin http-idempotency:jobs-operator-input-http-idempotency
    http_idempotency: serde::de::IgnoredAny,
    // template:end http-idempotency:jobs-operator-input-http-idempotency
    // template:begin service-auth:jobs-operator-input-service-auth
    service_auth: serde::de::IgnoredAny,
    // template:end service-auth:jobs-operator-input-service-auth
    // template:begin authn:jobs-operator-input-authn
    authn: serde::de::IgnoredAny,
    // template:end authn:jobs-operator-input-authn
    // template:begin object-storage:jobs-operator-input-object-storage
    object_storage: serde::de::IgnoredAny,
    // template:end object-storage:jobs-operator-input-object-storage
    // template:begin cache:jobs-operator-input-cache
    cache: serde::de::IgnoredAny,
    // template:end cache:jobs-operator-input-cache
    // template:begin grpc:jobs-operator-input-grpc
    grpc: serde::de::IgnoredAny,
    // template:end grpc:jobs-operator-input-grpc
    observability: serde::de::IgnoredAny,
    runtime: serde::de::IgnoredAny,
    log: serde::de::IgnoredAny,
    health: serde::de::IgnoredAny,
    http: serde::de::IgnoredAny,
    app: serde::de::IgnoredAny,
}

#[derive(Clone, Debug, Default)]
pub struct JobsOperatorConfig {
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
    pub fn validate_pool_capacity(
        &self,
        postgres_max_connections: NonZeroU32,
    ) -> Result<(), ValidationError> {
        let required = self.ordinary_pool_requirement();
        if u64::from(postgres_max_connections.get()) < required {
            return Err(ValidationError::new(
                "postgres.max_connections",
                format!(
                    "must be at least jobs.max_workers + {WORKER_MANAGEMENT_CONNECTIONS} ({required}) for the jobs worker, got {postgres_max_connections}"
                ),
            ));
        }
        Ok(())
    }

    fn ordinary_pool_requirement(&self) -> u64 {
        u64::from(self.max_workers.get()) + WORKER_MANAGEMENT_CONNECTIONS
    }

    // template:begin outbox:jobs-outbox-capacity
    pub fn validate_pool_capacity_with_outbox(
        &self,
        postgres_max_connections: NonZeroU32,
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
        if u64::from(postgres_max_connections.get()) < required {
            return Err(ValidationError::new(
                "postgres.max_connections",
                format!(
                    "must be at least {formula} ({required}) for the outbox worker, got {postgres_max_connections}"
                ),
            ));
        }
        Ok(())
    }
    // template:end outbox:jobs-outbox-capacity

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
