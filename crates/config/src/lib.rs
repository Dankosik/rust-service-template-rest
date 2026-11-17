mod app;
mod health;
mod http;
mod listener_limits;
// template:begin grpc:config-module
mod grpc;
// template:end grpc:config-module
mod log;
mod observability;
mod runtime;
// template:begin messaging:config-module
mod messaging;
// template:end messaging:config-module
// template:begin cache:config-module
mod cache;
// template:end cache:config-module
// template:begin object-storage:config-module
mod object_storage;
// template:end object-storage:config-module
// template:begin authn:config-module
mod authn;
// template:end authn:config-module
// template:begin service-auth:config-service-auth-module
mod service_auth;
// template:end service-auth:config-service-auth-module
// template:begin postgres:config-module
mod postgres;
// template:end postgres:config-module
// template:begin http-idempotency:config-http-idempotency-module
mod http_idempotency;
// template:end http-idempotency:config-http-idempotency-module
// template:begin inbound-webhooks:config-inbound-webhooks-module
mod inbound_webhooks;
// template:end inbound-webhooks:config-inbound-webhooks-module
// template:begin jobs:config-jobs-module
mod jobs;
// template:end jobs:config-jobs-module
// template:begin webhooks:config-webhooks-module
mod webhooks;
// template:end webhooks:config-webhooks-module

mod cli;
mod de;
mod load;
mod process;
mod secret_policy;
#[cfg(test)]
mod test_support;
mod validate;

use serde::Deserialize;

use validate::Violations;

pub use app::{AppConfig, BuildInfo};
pub use cli::LoadOptions;
pub use health::HealthConfig;
pub use http::{HttpConfig, ShutdownTail};
// template:begin worker:config-worker-export
pub use http::WorkerHttpConfig;
pub use load::load_worker;
// template:end worker:config-worker-export
// template:begin grpc:config-export
pub use grpc::{GrpcConfig, GrpcListenerSecurity, GrpcSecurity, GrpcTlsMaterial};
// template:end grpc:config-export
// template:begin inbound-webhooks:config-inbound-webhooks-export
pub use inbound_webhooks::{
    InboundWebhookEndpointConfig, InboundWebhooksConfig, WorkerInboundWebhooksConfig,
};
// template:end inbound-webhooks:config-inbound-webhooks-export
pub use load::{Error, load};
pub use log::{LogConfig, LogFormat};
pub use observability::{
    MetricsConfig, ObservabilityConfig, OtelConfig, OtelExporterConfig, TracesSampler,
};
pub use process::process_failure;
pub use runtime::RuntimeConfig;
// template:begin messaging:config-export
pub use messaging::{ConsumerKeys, MessagingConfig, MessagingCredentials};
// template:end messaging:config-export
// template:begin cache:config-export
pub use cache::{CacheClientCertificate, CacheConfig};
// template:end cache:config-export
// template:begin object-storage:config-export
pub use object_storage::{ObjectStorageConfig, ObjectStorageCredentials, ObjectStorageProvider};
// template:end object-storage:config-export
// template:begin authn:config-export
pub use authn::{Audiences, AuthnConfig};
// template:end authn:config-export
// template:begin oidc-jwt:config-jwt-input-exports
pub use authn::{JwtAlgorithm, TokenProfile};
// template:end oidc-jwt:config-jwt-input-exports
// template:begin service-auth:config-service-auth-export
pub use service_auth::{ServiceAuthConfig, ServiceSigningKey};
// template:end service-auth:config-service-auth-export
// template:begin postgres:config-export
pub use load::load_migration;
pub use postgres::{MigrationConfig, PostgresConfig, PostgresSessionBudgets};
// template:end postgres:config-export
// template:begin http-idempotency:config-http-idempotency-export
pub use http_idempotency::HttpIdempotencyConfig;
// template:end http-idempotency:config-http-idempotency-export
// template:begin jobs:config-jobs-export
pub use jobs::{JobsConfig, JobsOperatorConfig};
pub use load::load_jobs_operator;
// template:end jobs:config-jobs-export
// template:begin webhooks:config-webhooks-export
pub use webhooks::{WebhookEndpointConfig, WebhooksConfig};
// template:end webhooks:config-webhooks-export
pub use validate::{ValidationError, ValidationErrors};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ConfigInput {
    pub(crate) app: AppConfig,
    http: HttpConfig,
    // template:begin grpc:config-input-field
    grpc: grpc::GrpcInput,
    // template:end grpc:config-input-field
    // template:begin inbound-webhooks:config-inbound-webhooks-input-field
    inbound_webhooks: InboundWebhooksConfig,
    // template:end inbound-webhooks:config-inbound-webhooks-input-field
    health: HealthConfig,
    log: LogConfig,
    runtime: RuntimeConfig,
    observability: ObservabilityConfig,
    // template:begin messaging:config-input-field
    messaging: messaging::MessagingInput,
    // template:end messaging:config-input-field
    // template:begin cache:config-input-field
    cache: cache::CacheInput,
    // template:end cache:config-input-field
    // template:begin object-storage:config-input-field
    object_storage: object_storage::ObjectStorageInput,
    // template:end object-storage:config-input-field
    // template:begin authn:config-input-field
    authn: authn::AuthnInput,
    // template:end authn:config-input-field
    // template:begin service-auth:config-service-auth-input-field
    service_auth: service_auth::ServiceAuthInput,
    // template:end service-auth:config-service-auth-input-field
    // template:begin postgres:config-input-field
    postgres: postgres::PostgresInput,
    // template:end postgres:config-input-field
    // template:begin http-idempotency:config-http-idempotency-input-field
    http_idempotency: HttpIdempotencyConfig,
    // template:end http-idempotency:config-http-idempotency-input-field
    // template:begin jobs:config-jobs-input-field
    jobs: serde::de::IgnoredAny,
    // template:end jobs:config-jobs-input-field
    // template:begin webhooks:config-webhooks-input-field
    webhooks: serde::de::IgnoredAny,
    // template:end webhooks:config-webhooks-input-field
}

impl ConfigInput {
    pub(crate) fn admit(
        self,
        shutdown_tail: Option<ShutdownTail>,
    ) -> Result<Config, ValidationErrors> {
        let mut violations = Violations::default();
        let v = &mut violations;
        self.app.validate(v);
        #[allow(
            unused_variables,
            reason = "only the gRPC and cache profiles compare against HTTP timings"
        )]
        let http_timings = self.http.validate(shutdown_tail, v);
        // template:begin grpc:config-admit
        let grpc = self.grpc.admit(http_timings.drain_budget, v);
        // template:end grpc:config-admit
        // template:begin inbound-webhooks:config-inbound-webhooks-validate
        self.inbound_webhooks
            .validate_service(self.postgres.enabled, v);
        // template:end inbound-webhooks:config-inbound-webhooks-validate
        self.health.validate(v);
        self.log.validate(v);
        self.runtime.validate(v);
        self.observability.validate(v);
        // template:begin messaging:config-admit
        let messaging = self.messaging.admit(&self.app.env, v);
        // template:end messaging:config-admit
        // template:begin cache:config-admit
        let cache = self
            .cache
            .admit(&self.app.env, http_timings.request_timeout, v);
        // template:end cache:config-admit
        // template:begin object-storage:config-admit
        let object_storage = self.object_storage.admit(&self.app.env, v);
        // template:end object-storage:config-admit
        // template:begin authn:config-admit
        let authn = self.authn.admit(v);
        // template:end authn:config-admit
        // template:begin service-auth:config-service-auth-admit
        let grpc_enabled = !matches!(grpc, Ok(None));
        let service_auth = self.service_auth.admit(&self.app.env, grpc_enabled, v);
        // template:end service-auth:config-service-auth-admit
        // template:begin postgres:config-admit
        let postgres = self.postgres.admit(v);
        // template:end postgres:config-admit
        // template:begin http-idempotency:config-http-idempotency-validate
        self.http_idempotency.validate(v);
        // template:end http-idempotency:config-http-idempotency-validate
        #[allow(
            clippy::redundant_closure_call,
            reason = "the closure scopes `?` on admitted sections; without those profiles it has none"
        )]
        let admitted: Result<Config, validate::Rejected> = (|| {
            Ok(Config {
                app: self.app,
                http: self.http,
                // template:begin grpc:config-admitted-field
                grpc: grpc?,
                // template:end grpc:config-admitted-field
                // template:begin inbound-webhooks:config-inbound-webhooks-admitted-field
                inbound_webhooks: self.inbound_webhooks,
                // template:end inbound-webhooks:config-inbound-webhooks-admitted-field
                health: self.health,
                log: self.log,
                runtime: self.runtime,
                observability: self.observability,
                // template:begin messaging:config-admitted-field
                messaging: messaging?,
                // template:end messaging:config-admitted-field
                // template:begin cache:config-admitted-field
                cache: cache?,
                // template:end cache:config-admitted-field
                // template:begin object-storage:config-admitted-field
                object_storage: object_storage?,
                // template:end object-storage:config-admitted-field
                // template:begin authn:config-admitted-field
                authn: authn?,
                // template:end authn:config-admitted-field
                // template:begin service-auth:config-service-auth-admitted-field
                service_auth: service_auth?,
                // template:end service-auth:config-service-auth-admitted-field
                // template:begin postgres:config-admitted-field
                postgres: postgres?,
                // template:end postgres:config-admitted-field
                // template:begin http-idempotency:config-http-idempotency-admitted-field
                http_idempotency: self.http_idempotency,
                // template:end http-idempotency:config-http-idempotency-admitted-field
            })
        })();
        violations.finish(admitted)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub app: AppConfig,
    pub http: HttpConfig,
    // template:begin grpc:config-field
    pub grpc: Option<GrpcConfig>,
    // template:end grpc:config-field
    // template:begin inbound-webhooks:config-inbound-webhooks-field
    pub inbound_webhooks: InboundWebhooksConfig,
    // template:end inbound-webhooks:config-inbound-webhooks-field
    pub health: HealthConfig,
    pub log: LogConfig,
    pub runtime: RuntimeConfig,
    pub observability: ObservabilityConfig,
    // template:begin messaging:config-field
    pub messaging: MessagingConfig,
    // template:end messaging:config-field
    // template:begin cache:config-field
    pub cache: CacheConfig,
    // template:end cache:config-field
    // template:begin object-storage:config-field
    pub object_storage: Option<ObjectStorageConfig>,
    // template:end object-storage:config-field
    // template:begin authn:config-field
    pub authn: AuthnConfig,
    // template:end authn:config-field
    // template:begin service-auth:config-service-auth-field
    pub service_auth: ServiceAuthConfig,
    // template:end service-auth:config-service-auth-field
    // template:begin postgres:config-field
    pub postgres: Option<PostgresConfig>,
    // template:end postgres:config-field
    // template:begin http-idempotency:config-http-idempotency-field
    pub http_idempotency: HttpIdempotencyConfig,
    // template:end http-idempotency:config-http-idempotency-field
}

#[allow(
    unused_macros,
    unused_macro_rules,
    reason = "only the worker profile expands the worker configuration types"
)]
macro_rules! declare_worker_config {
    () => {
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
            http: WorkerHttpConfig,
            // template:begin postgres:worker-input-postgres-field
            postgres: postgres::PostgresInput,
            // template:end postgres:worker-input-postgres-field
            // template:begin jobs:worker-input-jobs-field
            jobs: JobsConfig,
            // template:end jobs:worker-input-jobs-field
            // template:begin messaging:worker-input-messaging-field
            messaging: messaging::MessagingInput,
            // template:end messaging:worker-input-messaging-field
            // template:begin webhooks:worker-input-webhooks-field
            webhooks: WebhooksConfig,
            // template:end webhooks:worker-input-webhooks-field
            // template:begin inbound-webhooks:worker-input-inbound-webhooks-field
            inbound_webhooks: WorkerInboundWebhooksConfig,
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
                self.http.validate(shutdown_tail, v);
                // template:begin inbound-webhooks:worker-inbound-webhooks-validate
                self.inbound_webhooks.validate(self.postgres.enabled, v);
                // template:end inbound-webhooks:worker-inbound-webhooks-validate
                self.health.validate(v);
                self.log.validate(v);
                self.runtime.validate(v);
                self.observability.validate(v);
                // template:begin messaging:worker-messaging-admit
                let messaging = self.messaging.admit(&self.app.env, v);
                // template:end messaging:worker-messaging-admit
                // template:begin postgres:worker-postgres-admit
                let postgres = self.postgres.admit(v);
                // template:end postgres:worker-postgres-admit
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
                let admitted: Result<WorkerConfig, validate::Rejected> = (|| {
                    Ok(WorkerConfig {
                        app: self.app,
                        log: self.log,
                        runtime: self.runtime,
                        health: self.health,
                        observability: self.observability,
                        http: self.http,
                        // template:begin postgres:worker-postgres-admitted-field
                        postgres: postgres?,
                        // template:end postgres:worker-postgres-admitted-field
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
                        inbound_webhooks: self.inbound_webhooks,
                        // template:end inbound-webhooks:worker-inbound-webhooks-admitted-field
                    })
                })();
                violations.finish(admitted)
            }
        }

        #[derive(Clone, Debug, Default)]
        pub struct WorkerConfig {
            pub app: AppConfig,
            pub log: LogConfig,
            pub runtime: RuntimeConfig,
            pub health: HealthConfig,
            pub observability: ObservabilityConfig,
            pub http: WorkerHttpConfig,
            // template:begin postgres:worker-config-postgres-field
            pub postgres: Option<PostgresConfig>,
            // template:end postgres:worker-config-postgres-field
            // template:begin jobs:worker-config-jobs-field
            pub jobs: JobsConfig,
            // template:end jobs:worker-config-jobs-field
            // template:begin messaging:worker-config-messaging-field
            pub messaging: MessagingConfig,
            // template:end messaging:worker-config-messaging-field
            // template:begin webhooks:worker-config-webhooks-field
            pub webhooks: WebhooksConfig,
            // template:end webhooks:worker-config-webhooks-field
            // template:begin inbound-webhooks:worker-config-inbound-webhooks-field
            pub inbound_webhooks: WorkerInboundWebhooksConfig,
            // template:end inbound-webhooks:worker-config-inbound-webhooks-field
        }
    };
}

// template:begin worker:worker-config
declare_worker_config!();
// template:end worker:worker-config
