use std::num::NonZeroU32;

use serde::Deserialize;

use crate::postgres::{OperatorPostgresConfig, OperatorPostgresInput};
use crate::validate::{ValidationErrors, Violations, int_range};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct JobsOperatorInput {
    postgres: OperatorPostgresInput,
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

#[derive(Clone, Debug)]
pub struct JobsOperatorConfig {
    postgres: Option<OperatorPostgresConfig>,
}

impl JobsOperatorConfig {
    pub fn postgres(&self) -> Option<&OperatorPostgresConfig> {
        self.postgres.as_ref()
    }
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
