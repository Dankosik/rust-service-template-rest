//! Typed, admitted, immutable runtime configuration.
//!
//! Precedence, last wins: code defaults → `--config` file → `--config-overlay`
//! files in order → `APP__SECTION__KEY` variables, read from the files of
//! `--secrets-dir` and then from the environment. A later source may override
//! individual keys in an earlier table. Replacing the table with a value
//! fails startup, since it could hide unknown keys before decoding. Secret-like
//! keys may carry a value only through a variable.
//!
//! Loading has two steps. Decoding fills the operator's input types and
//! rejects unknown keys; it stops at its first failure, as `serde` does.
//! Admission then checks every rule, reports all violations together, and
//! builds [`Config`], whose types hold only admitted states: a disabled
//! listener is `None`, an enabled one carries its address and security. A
//! rule spanning two sections lives in the one that depends on the other.
//! Rules that need process structure, such as the drain-plus-teardown tail
//! against the grace period, or a binary's role, stay in the composition root.
//!
//! The migration and jobs operator loaders apply the shared input rules, then
//! ignore sections outside their own snapshots when decoding.
//!
//! The loader is [`config`](https://docs.rs/config) with `serde`; the
//! variable-name and file pre-scans and the secrets directory in [`load()`]
//! add what the crate does not.

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
// template:begin client-integrations:config-module
mod integrations;
// template:end client-integrations:config-module
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
pub use http::HttpConfig;
// template:begin grpc:config-export
pub use grpc::{GrpcConfig, GrpcListenerSecurity, GrpcSecurity, GrpcTlsMaterial};
// template:end grpc:config-export
// template:begin outbound-auth:config-export
pub use integrations::{OAuthAlgorithm, OAuthConfig, Scopes};
// template:end outbound-auth:config-export
// template:begin client-integrations:config-integration-export
pub use integrations::IntegrationConfig;
// template:end client-integrations:config-integration-export
// template:begin grpc:config-integration-grpc-export
pub use integrations::GrpcClientConfig;
// template:end grpc:config-integration-grpc-export
// template:begin inbound-webhooks:config-inbound-webhooks-export
pub use inbound_webhooks::{InboundWebhookEndpointConfig, InboundWebhooksConfig};
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

/// What the operator supplied, decoded but not yet admitted.
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
    // template:begin client-integrations:config-input-field
    integrations: std::collections::BTreeMap<String, IntegrationConfig>,
    // template:end client-integrations:config-input-field
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
    jobs: JobsConfig,
    // template:end jobs:config-jobs-input-field
    // template:begin webhooks:config-webhooks-input-field
    webhooks: WebhooksConfig,
    // template:end webhooks:config-webhooks-input-field
}

impl ConfigInput {
    /// Check every rule and build the snapshot, or report every violation.
    pub(crate) fn admit(self) -> Result<Config, ValidationErrors> {
        let mut violations = Violations::default();
        let v = &mut violations;
        self.app.validate(v);
        #[allow(
            unused_variables,
            reason = "only the gRPC and cache profiles compare against HTTP timings"
        )]
        let http_timings = self.http.validate(v);
        // template:begin grpc:config-admit
        let grpc = self.grpc.admit(http_timings.drain_budget, v);
        // template:end grpc:config-admit
        // template:begin inbound-webhooks:config-inbound-webhooks-validate
        self.inbound_webhooks.validate(self.postgres.enabled, v);
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
        // template:begin client-integrations:config-validate
        integrations::validate_integrations(&self.integrations, v);
        // template:end client-integrations:config-validate
        // template:begin authn:config-admit
        let authn = self.authn.admit(v);
        // template:end authn:config-admit
        // template:begin service-auth:config-service-auth-admit
        // Only an enabled listener is admitted or rejected; a disabled one is `Ok(None)`.
        let grpc_enabled = !matches!(grpc, Ok(None));
        let service_auth = self.service_auth.admit(&self.app.env, grpc_enabled, v);
        // template:end service-auth:config-service-auth-admit
        // template:begin postgres:config-admit
        let postgres = self.postgres.admit(v);
        // template:end postgres:config-admit
        // template:begin http-idempotency:config-http-idempotency-validate
        self.http_idempotency.validate(v);
        // template:end http-idempotency:config-http-idempotency-validate
        // template:begin jobs:config-jobs-validate
        self.jobs.validate(v);
        // template:end jobs:config-jobs-validate
        // template:begin webhooks:config-webhooks-validate
        self.webhooks.validate(v);
        // template:end webhooks:config-webhooks-validate
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
                // template:begin client-integrations:config-admitted-field
                integrations: self.integrations,
                // template:end client-integrations:config-admitted-field
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
                // template:begin jobs:config-jobs-admitted-field
                jobs: self.jobs,
                // template:end jobs:config-jobs-admitted-field
                // template:begin webhooks:config-webhooks-admitted-field
                webhooks: self.webhooks,
                // template:end webhooks:config-webhooks-admitted-field
            })
        })();
        violations.finish(admitted)
    }
}

/// The admitted snapshot [`load()`] returns.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub app: AppConfig,
    pub http: HttpConfig,
    // template:begin grpc:config-field
    /// The native gRPC listener; `None` when `grpc.enabled` is false.
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
    /// Object storage; `None` when `object_storage.provider` is `none`.
    pub object_storage: Option<ObjectStorageConfig>,
    // template:end object-storage:config-field
    // template:begin client-integrations:config-field
    pub integrations: std::collections::BTreeMap<String, IntegrationConfig>,
    // template:end client-integrations:config-field
    // template:begin authn:config-field
    pub authn: AuthnConfig,
    // template:end authn:config-field
    // template:begin service-auth:config-service-auth-field
    pub service_auth: ServiceAuthConfig,
    // template:end service-auth:config-service-auth-field
    // template:begin postgres:config-field
    /// PostgreSQL; `None` when `postgres.enabled` is false.
    pub postgres: Option<PostgresConfig>,
    // template:end postgres:config-field
    // template:begin http-idempotency:config-http-idempotency-field
    pub http_idempotency: HttpIdempotencyConfig,
    // template:end http-idempotency:config-http-idempotency-field
    // template:begin jobs:config-jobs-field
    pub jobs: JobsConfig,
    // template:end jobs:config-jobs-field
    // template:begin webhooks:config-webhooks-field
    pub webhooks: WebhooksConfig,
    // template:end webhooks:config-webhooks-field
}
