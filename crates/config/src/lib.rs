//! Typed, validated, immutable runtime configuration.
//!
//! Precedence, last wins: code defaults → `--config` file → `--config-overlay`
//! files in order → `APP__SECTION__KEY` environment variables. Unknown keys
//! anywhere fail startup. Secret-like keys may carry a value only through the
//! environment. Each section owns its type, defaults, and validation in one
//! file. [`Config::validate`] runs those section validators; intra-HTTP key
//! relations stay in [`http::HttpConfig::validate`]. Rules that need process
//! structure, such as the drain-plus-teardown tail against the grace period,
//! stay in the composition root.
//!
//! The loader is [`config`](https://docs.rs/config) with `serde`; see
//! `docs/configuration-source-policy.md` for why, and for what the two
//! pre-scans in [`load`] add that the crate does not.

pub mod app;
pub mod health;
pub mod http;
// template:begin grpc:config-module
pub mod grpc;
// template:end grpc:config-module
pub mod log;
pub mod observability;
// template:begin messaging:config-module
pub mod messaging;
// template:end messaging:config-module
// template:begin client-integrations:config-module
pub mod integrations;
// template:end client-integrations:config-module
// template:begin authn:config-module
pub mod authn;
// template:end authn:config-module
// template:begin postgres:config-module
pub mod postgres;
// template:end postgres:config-module
// template:begin http-idempotency:config-http-idempotency-module
pub mod http_idempotency;
// template:end http-idempotency:config-http-idempotency-module
// template:begin inbound-webhooks:config-inbound-webhooks-module
pub mod inbound_webhooks;
// template:end inbound-webhooks:config-inbound-webhooks-module
// template:begin jobs:config-jobs-module
pub mod jobs;
// template:end jobs:config-jobs-module
// template:begin webhooks:config-webhooks-module
pub mod webhooks;
// template:end webhooks:config-webhooks-module

mod cli;
mod load;
mod secret_policy;
mod validate;

use serde::Deserialize;

pub use app::{AppConfig, BuildInfo};
pub use cli::{FromArgs, LoadOptions, process_failure};
pub use health::HealthConfig;
pub use http::HttpConfig;
// template:begin grpc:config-export
pub use grpc::{GrpcConfig, GrpcSecurity};
// template:end grpc:config-export
// template:begin outbound-auth:config-export
pub use integrations::{OAuthConfig, Scopes};
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
pub use load::{ENV_PREFIX, Error, MAX_FILE_BYTES, load};
pub use log::{LogConfig, LogFormat};
pub use observability::{
    MetricsConfig, ObservabilityConfig, OtelConfig, OtelExporterConfig, TracesSampler,
};
// template:begin messaging:config-export
pub use messaging::MessagingConfig;
// template:end messaging:config-export
// template:begin authn:config-export
pub use authn::{Audiences, AuthnConfig};
// template:end authn:config-export
// template:begin oidc-jwt:config-jwt-input-exports
pub use authn::{JwtAlgorithm, TokenProfile};
// template:end oidc-jwt:config-jwt-input-exports
// template:begin postgres:config-export
pub use postgres::PostgresConfig;
// template:end postgres:config-export
// template:begin http-idempotency:config-http-idempotency-export
pub use http_idempotency::HttpIdempotencyConfig;
// template:end http-idempotency:config-http-idempotency-export
// template:begin jobs:config-jobs-export
pub use jobs::JobsConfig;
// template:end jobs:config-jobs-export
// template:begin webhooks:config-webhooks-export
pub use webhooks::{WebhookEndpointConfig, WebhooksConfig};
// template:end webhooks:config-webhooks-export
pub use secret_policy::is_secret_like_key;
pub use validate::ValidationError;

/// The immutable runtime snapshot built once during startup.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub app: AppConfig,
    pub http: HttpConfig,
    // template:begin grpc:config-field
    pub grpc: GrpcConfig,
    // template:end grpc:config-field
    // template:begin inbound-webhooks:config-inbound-webhooks-field
    pub inbound_webhooks: InboundWebhooksConfig,
    // template:end inbound-webhooks:config-inbound-webhooks-field
    pub health: HealthConfig,
    pub log: LogConfig,
    pub observability: ObservabilityConfig,
    // template:begin messaging:config-field
    pub messaging: MessagingConfig,
    // template:end messaging:config-field
    // template:begin client-integrations:config-field
    #[serde(default, deserialize_with = "integrations::deserialize_integrations")]
    pub integrations: std::collections::BTreeMap<String, IntegrationConfig>,
    // template:end client-integrations:config-field
    // template:begin authn:config-field
    pub authn: AuthnConfig,
    // template:end authn:config-field
    // template:begin postgres:config-field
    pub postgres: PostgresConfig,
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

impl Config {
    /// Validate every section. Cross-key rules that live on a section type
    /// run there; process-structure ceilings are not encoded here.
    ///
    /// # Errors
    ///
    /// Returns the first violated rule, naming the configuration key.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.app.validate()?;
        self.http.validate()?;
        // template:begin grpc:config-validate
        self.grpc.validate()?;
        // template:end grpc:config-validate
        // template:begin inbound-webhooks:config-inbound-webhooks-validate
        self.inbound_webhooks.validate()?;
        // template:end inbound-webhooks:config-inbound-webhooks-validate
        self.health.validate()?;
        self.log.validate()?;
        self.observability.validate()?;
        // template:begin messaging:config-validate
        self.messaging.validate(&self.app.env)?;
        // template:end messaging:config-validate
        // template:begin client-integrations:config-validate
        integrations::validate_integrations(&self.integrations)?;
        // template:end client-integrations:config-validate
        // template:begin authn:config-validate
        self.authn.validate()?;
        // template:end authn:config-validate
        // template:begin postgres:config-validate
        self.postgres.validate()?;
        // template:end postgres:config-validate
        // template:begin http-idempotency:config-http-idempotency-validate
        self.http_idempotency.validate()?;
        // template:end http-idempotency:config-http-idempotency-validate
        // template:begin jobs:config-jobs-validate
        self.jobs.validate()?;
        // template:end jobs:config-jobs-validate
        // template:begin webhooks:config-webhooks-validate
        self.webhooks.validate()?;
        // template:end webhooks:config-webhooks-validate
        Ok(())
    }
}
