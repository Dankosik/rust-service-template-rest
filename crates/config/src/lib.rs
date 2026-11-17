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
#[cfg(feature = "test-support")]
pub mod fixtures;
mod load;
mod secret_policy;
#[cfg(test)]
mod test_support;
mod validate;

use serde::Deserialize;

use validate::Violations;

pub use app::{AppConfig, BuildInfo};
pub use cli::LoadOptions;
pub use health::HealthConfig;
pub use http::{HttpConfig, HttpListenerConfig, ShutdownTail};
// template:begin worker:config-worker-export
pub use http::WorkerHttpConfig;
pub use load::load_worker;
// template:end worker:config-worker-export
// template:begin grpc:config-export
pub use grpc::{GrpcConfig, GrpcListenerSecurity, GrpcSecurity, GrpcTlsMaterial};
// template:end grpc:config-export
// template:begin inbound-webhooks:config-inbound-webhooks-export
pub use inbound_webhooks::{InboundWebhookEndpointConfig, InboundWebhookKey};
// template:end inbound-webhooks:config-inbound-webhooks-export
pub use load::{Error, load};
pub use log::{LogConfig, LogFormat};
pub use observability::{
    MetricsConfig, ObservabilityConfig, OtelConfig, OtelExporterConfig, TracesSampler,
};
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
pub use load::{MigrationLoadError, load_migration};
pub use postgres::{
    MigrationConfig, MigrationPostgresConfig, PostgresConfig, PostgresSessionBudgets,
};
// template:end postgres:config-export
// template:begin http-idempotency:config-http-idempotency-export
pub use http_idempotency::HttpIdempotencyConfig;
// template:end http-idempotency:config-http-idempotency-export
// template:begin jobs:config-jobs-export
pub use jobs::{JobsConfig, JobsOperatorConfig};
pub use load::{JobsOperatorLoadError, load_jobs_operator};
pub use postgres::OperatorPostgresConfig;
// template:end jobs:config-jobs-export
// template:begin webhooks:config-webhooks-export
pub use webhooks::{WebhookEndpointConfig, WebhooksConfig};
// template:end webhooks:config-webhooks-export
pub use validate::{ValidationError, ValidationErrors};

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct ConfigInput {
    pub(crate) app: AppConfig,
    http: http::HttpInput,
    // template:begin grpc:config-input-field
    grpc: grpc::GrpcInput,
    // template:end grpc:config-input-field
    // template:begin inbound-webhooks:config-inbound-webhooks-input-field
    inbound_webhooks: inbound_webhooks::InboundWebhooksInput,
    // template:end inbound-webhooks:config-inbound-webhooks-input-field
    health: HealthConfig,
    log: LogConfig,
    runtime: RuntimeConfig,
    observability: ObservabilityConfig,
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
        let (http, http_timings) = self.http.admit(shutdown_tail, v);
        // template:begin grpc:config-admit
        let grpc = self.grpc.admit(http_timings.drain_budget, v);
        // template:end grpc:config-admit
        // template:begin inbound-webhooks:config-inbound-webhooks-validate
        let inbound_webhooks = self.inbound_webhooks.admit(self.postgres.enabled, v);
        // template:end inbound-webhooks:config-inbound-webhooks-validate
        self.health.validate(v);
        self.log.validate(v);
        self.runtime.validate(v);
        self.observability.validate(v);
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
            Ok(Config(ServiceSections {
                app: self.app,
                http,
                // template:begin grpc:config-admitted-field
                grpc: grpc?,
                // template:end grpc:config-admitted-field
                // template:begin inbound-webhooks:config-inbound-webhooks-admitted-field
                inbound_webhooks,
                // template:end inbound-webhooks:config-inbound-webhooks-admitted-field
                health: self.health,
                log: self.log,
                runtime: self.runtime,
                observability: self.observability,
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
            }))
        })();
        violations.finish(admitted)
    }
}

#[derive(Clone, Debug)]
pub struct Config(ServiceSections);

impl std::ops::Deref for Config {
    type Target = ServiceSections;

    fn deref(&self) -> &ServiceSections {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct ServiceSections {
    pub app: AppConfig,
    pub http: HttpConfig,
    // template:begin grpc:config-field
    pub grpc: Option<GrpcConfig>,
    // template:end grpc:config-field
    // template:begin inbound-webhooks:config-inbound-webhooks-field
    pub inbound_webhooks: std::collections::BTreeMap<String, InboundWebhookEndpointConfig>,
    // template:end inbound-webhooks:config-inbound-webhooks-field
    pub health: HealthConfig,
    pub log: LogConfig,
    pub runtime: RuntimeConfig,
    pub observability: ObservabilityConfig,
    // template:begin cache:config-field
    pub cache: Option<CacheConfig>,
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

// template:begin worker:worker-config
mod worker;
use worker::WorkerInput;
pub use worker::{WorkerConfig, WorkerSections};
// template:end worker:worker-config
