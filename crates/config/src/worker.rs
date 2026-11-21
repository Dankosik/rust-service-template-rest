use serde::Deserialize;

use crate::validate::admit_with;
use crate::{
    AppConfig, BuildInfo, HealthConfig, LogConfig, ObservabilityConfig, RuntimeConfig,
    ShutdownTail, ValidationErrors, WorkerHttpConfig,
};

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct WorkerInput {
    app: crate::app::AppInput,
    log: LogConfig,
    runtime: RuntimeConfig,
    health: HealthConfig,
    observability: crate::observability::ObservabilityInput,
    http: crate::http::WorkerHttpInput,
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
    webhooks: crate::webhooks::WebhooksInput,
    // template:end webhooks:worker-input-webhooks-field
    // template:begin inbound-webhooks:worker-input-inbound-webhooks-field
    inbound_webhooks: crate::inbound_webhooks::WorkerInboundWebhooksInput,
    // template:end inbound-webhooks:worker-input-inbound-webhooks-field
}

impl WorkerInput {
    pub(crate) fn admit(
        self,
        build: BuildInfo,
        shutdown_tail: Option<ShutdownTail>,
    ) -> Result<WorkerConfig, ValidationErrors> {
        admit_with(|v| {
            let app = self.app.admit(build, v);
            let http = self.http.admit(shutdown_tail, v);
            // template:begin inbound-webhooks:worker-inbound-webhooks-validate
            let inbound_webhook_endpoints = self.inbound_webhooks.admit(self.postgres.enabled, v);
            // template:end inbound-webhooks:worker-inbound-webhooks-validate
            self.health.validate(v);
            self.log.validate(v);
            self.runtime.validate(v);
            let observability = self.observability.admit(v);
            // template:begin messaging:worker-messaging-admit
            let messaging = self.messaging.admit(app.is_local_development(), v);
            // template:end messaging:worker-messaging-admit
            // template:begin jobs:worker-postgres-admit
            let postgres = self.postgres.admit(v);
            // template:end jobs:worker-postgres-admit
            // template:begin jobs:worker-jobs-validate
            self.jobs.validate(v);
            // template:end jobs:worker-jobs-validate
            // template:begin webhooks:worker-webhooks-validate
            let webhooks = self.webhooks.admit(v);
            // template:end webhooks:worker-webhooks-validate
            Ok(WorkerConfig {
                app,
                log: self.log,
                runtime: self.runtime,
                health: self.health,
                observability,
                http: http?,
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
                webhooks,
                // template:end webhooks:worker-webhooks-admitted-field
                // template:begin inbound-webhooks:worker-inbound-webhooks-admitted-field
                inbound_webhook_endpoints,
                // template:end inbound-webhooks:worker-inbound-webhooks-admitted-field
            })
        })
    }
}

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    app: AppConfig,
    log: LogConfig,
    runtime: RuntimeConfig,
    health: HealthConfig,
    observability: ObservabilityConfig,
    http: WorkerHttpConfig,
    // template:begin jobs:worker-config-postgres-field
    postgres: Option<crate::PostgresConfig>,
    // template:end jobs:worker-config-postgres-field
    // template:begin jobs:worker-config-jobs-field
    jobs: crate::JobsConfig,
    // template:end jobs:worker-config-jobs-field
    // template:begin messaging:worker-config-messaging-field
    messaging: crate::MessagingConfig,
    // template:end messaging:worker-config-messaging-field
    // template:begin webhooks:worker-config-webhooks-field
    webhooks: crate::WebhooksConfig,
    // template:end webhooks:worker-config-webhooks-field
    // template:begin inbound-webhooks:worker-config-inbound-webhooks-field
    inbound_webhook_endpoints: Vec<String>,
    // template:end inbound-webhooks:worker-config-inbound-webhooks-field
}

impl WorkerConfig {
    #[must_use]
    pub const fn app(&self) -> &AppConfig {
        &self.app
    }

    #[must_use]
    pub const fn log(&self) -> &LogConfig {
        &self.log
    }

    #[must_use]
    pub const fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }

    #[must_use]
    pub const fn health(&self) -> &HealthConfig {
        &self.health
    }

    #[must_use]
    pub const fn observability(&self) -> &ObservabilityConfig {
        &self.observability
    }

    #[must_use]
    pub const fn http(&self) -> &WorkerHttpConfig {
        &self.http
    }

    // template:begin jobs:worker-config-postgres-accessor
    #[must_use]
    pub const fn postgres(&self) -> Option<&crate::PostgresConfig> {
        self.postgres.as_ref()
    }
    // template:end jobs:worker-config-postgres-accessor

    // template:begin jobs:worker-config-jobs-accessor
    #[must_use]
    pub const fn jobs(&self) -> &crate::JobsConfig {
        &self.jobs
    }
    // template:end jobs:worker-config-jobs-accessor

    // template:begin messaging:worker-config-messaging-accessor
    #[must_use]
    pub const fn messaging(&self) -> &crate::MessagingConfig {
        &self.messaging
    }
    // template:end messaging:worker-config-messaging-accessor

    // template:begin webhooks:worker-config-webhooks-accessor
    #[must_use]
    pub const fn webhooks(&self) -> &crate::WebhooksConfig {
        &self.webhooks
    }
    // template:end webhooks:worker-config-webhooks-accessor

    // template:begin inbound-webhooks:worker-config-inbound-webhooks-accessor
    #[must_use]
    pub fn inbound_webhook_endpoints(&self) -> &[String] {
        &self.inbound_webhook_endpoints
    }
    // template:end inbound-webhooks:worker-config-inbound-webhooks-accessor

    #[must_use]
    pub fn finite_listener_connection_caps(&self) -> u64 {
        crate::finite_http_connection_caps(self.http.listener(), &self.observability.metrics)
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::load_worker_env;

    #[rstest::rstest]
    #[case::health_only("4", "", 4)]
    #[case::with_diagnostics("4", ":9090", 8)]
    #[case::uncapped("0", ":9090", 0)]
    fn worker_counts_finite_caps_of_enabled_listeners(
        #[case] http_max_connections: &str,
        #[case] diagnostics_addr: &str,
        #[case] expected: u64,
    ) {
        let cfg = load_worker_env(&[
            ("APP__HTTP__MAX_CONNECTIONS", http_max_connections),
            ("APP__OBSERVABILITY__METRICS__ADDR", diagnostics_addr),
        ]);
        assert_eq!(cfg.finite_listener_connection_caps(), expected);
    }
}
