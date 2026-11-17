use serde::Deserialize;

use crate::validate::{Rejected, Violations};
use crate::{
    AppConfig, HealthConfig, LogConfig, ObservabilityConfig, RuntimeConfig, ShutdownTail,
    ValidationErrors, WorkerHttpConfig,
};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[allow(
    dead_code,
    reason = "retained service-only sibling sections are decoded as IgnoredAny and never read by the worker"
)]
pub(crate) struct WorkerInput {
    pub(crate) app: AppConfig,
    log: LogConfig,
    runtime: RuntimeConfig,
    health: HealthConfig,
    observability: ObservabilityConfig,
    http: crate::http::WorkerHttpInput,
    // template:begin jobs:worker-skip-service-postgres
    #[serde(skip)]
    // template:end jobs:worker-skip-service-postgres
    // template:begin postgres:worker-input-service-postgres
    #[serde(rename = "postgres")]
    service_postgres: serde::de::IgnoredAny,
    // template:end postgres:worker-input-service-postgres
    // template:begin jobs:worker-input-postgres-field
    postgres: crate::postgres::PostgresInput,
    // template:end jobs:worker-input-postgres-field
    // template:begin jobs:worker-input-jobs-field
    jobs: crate::JobsConfig,
    // template:end jobs:worker-input-jobs-field
    // template:begin messaging:worker-input-messaging-field
    messaging: crate::messaging::MessagingInput,
    // template:end messaging:worker-input-messaging-field
    // template:begin webhooks:worker-input-webhooks-field
    webhooks: crate::WebhooksConfig,
    // template:end webhooks:worker-input-webhooks-field
    // template:begin inbound-webhooks:worker-input-inbound-webhooks-field
    inbound_webhooks: crate::inbound_webhooks::WorkerInboundWebhooksInput,
    // template:end inbound-webhooks:worker-input-inbound-webhooks-field
    // template:begin authn:worker-input-authn-field
    authn: serde::de::IgnoredAny,
    // template:end authn:worker-input-authn-field
    // template:begin cache:worker-input-cache-field
    cache: serde::de::IgnoredAny,
    // template:end cache:worker-input-cache-field
    // template:begin grpc:worker-input-grpc-field
    grpc: serde::de::IgnoredAny,
    // template:end grpc:worker-input-grpc-field
    // template:begin http-idempotency:worker-input-http-idempotency-field
    http_idempotency: serde::de::IgnoredAny,
    // template:end http-idempotency:worker-input-http-idempotency-field
    // template:begin object-storage:worker-input-object-storage-field
    object_storage: serde::de::IgnoredAny,
    // template:end object-storage:worker-input-object-storage-field
    // template:begin service-auth:worker-input-service-auth-field
    service_auth: serde::de::IgnoredAny,
    // template:end service-auth:worker-input-service-auth-field
}

impl WorkerInput {
    pub(crate) fn admit(
        self,
        shutdown_tail: Option<ShutdownTail>,
    ) -> Result<WorkerConfig, ValidationErrors> {
        let mut violations = Violations::default();
        let v = &mut violations;
        self.app.validate(v);
        let http = self.http.admit(shutdown_tail, v);
        // template:begin inbound-webhooks:worker-inbound-webhooks-validate
        let inbound_webhook_endpoints = self.inbound_webhooks.admit(self.postgres.enabled, v);
        // template:end inbound-webhooks:worker-inbound-webhooks-validate
        self.health.validate(v);
        self.log.validate(v);
        self.runtime.validate(v);
        self.observability.validate(v);
        // template:begin messaging:worker-messaging-admit
        let messaging = self.messaging.admit(&self.app.env, v);
        // template:end messaging:worker-messaging-admit
        // template:begin jobs:worker-postgres-admit
        let postgres = self.postgres.admit(v);
        // template:end jobs:worker-postgres-admit
        // template:begin jobs:worker-jobs-validate
        self.jobs.validate(v);
        // template:end jobs:worker-jobs-validate
        // template:begin webhooks:worker-webhooks-validate
        self.webhooks.validate(v);
        // template:end webhooks:worker-webhooks-validate
        #[allow(
            clippy::redundant_closure_call,
            reason = "the closure scopes `?` on admitted sections; jobs-only or messaging-only subsets vary"
        )]
        let admitted: Result<WorkerConfig, Rejected> = (|| {
            Ok(WorkerConfig(WorkerSections {
                app: self.app,
                log: self.log,
                runtime: self.runtime,
                health: self.health,
                observability: self.observability,
                http,
                // template:begin jobs:worker-postgres-admitted-field
                postgres: postgres?,
                // template:end jobs:worker-postgres-admitted-field
                // template:begin jobs:worker-jobs-admitted-field
                jobs: self.jobs,
                // template:end jobs:worker-jobs-admitted-field
                // template:begin messaging:worker-messaging-admitted-field
                messaging: messaging?,
                // template:end messaging:worker-messaging-admitted-field
                // template:begin webhooks:worker-webhooks-admitted-field
                webhooks: self.webhooks,
                // template:end webhooks:worker-webhooks-admitted-field
                // template:begin inbound-webhooks:worker-inbound-webhooks-admitted-field
                inbound_webhook_endpoints,
                // template:end inbound-webhooks:worker-inbound-webhooks-admitted-field
            }))
        })();
        violations.finish(admitted)
    }
}

#[derive(Clone, Debug)]
pub struct WorkerConfig(WorkerSections);

impl std::ops::Deref for WorkerConfig {
    type Target = WorkerSections;

    fn deref(&self) -> &WorkerSections {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct WorkerSections {
    pub app: AppConfig,
    pub log: LogConfig,
    pub runtime: RuntimeConfig,
    pub health: HealthConfig,
    pub observability: ObservabilityConfig,
    pub http: WorkerHttpConfig,
    // template:begin jobs:worker-config-postgres-field
    pub postgres: Option<crate::PostgresConfig>,
    // template:end jobs:worker-config-postgres-field
    // template:begin jobs:worker-config-jobs-field
    pub jobs: crate::JobsConfig,
    // template:end jobs:worker-config-jobs-field
    // template:begin messaging:worker-config-messaging-field
    pub messaging: crate::MessagingConfig,
    // template:end messaging:worker-config-messaging-field
    // template:begin webhooks:worker-config-webhooks-field
    pub webhooks: crate::WebhooksConfig,
    // template:end webhooks:worker-config-webhooks-field
    // template:begin inbound-webhooks:worker-config-inbound-webhooks-field
    pub inbound_webhook_endpoints: std::collections::BTreeSet<String>,
    // template:end inbound-webhooks:worker-config-inbound-webhooks-field
}
