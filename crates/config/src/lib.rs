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
pub mod log;
pub mod observability;
// template:begin authn:config-module
pub mod authn;
// template:end authn:config-module
// template:begin postgres:config-module
pub mod postgres;
// template:end postgres:config-module

mod cli;
mod load;
mod secret_policy;
mod validate;

use serde::Deserialize;

pub use app::{AppConfig, BuildInfo};
pub use cli::{FromArgs, LoadOptions, process_failure};
pub use health::HealthConfig;
pub use http::HttpConfig;
pub use load::{ENV_PREFIX, Error, MAX_FILE_BYTES, load};
pub use log::{LogConfig, LogFormat};
pub use observability::{
    MetricsConfig, ObservabilityConfig, OtelConfig, OtelExporterConfig, TracesSampler,
};
// template:begin authn:config-export
pub use authn::{AuthnConfig, AuthnMode};
// template:end authn:config-export
// template:begin oidc-jwt:config-token-profile-export
pub use authn::TokenProfile;
// template:end oidc-jwt:config-token-profile-export
// template:begin postgres:config-export
pub use postgres::PostgresConfig;
// template:end postgres:config-export
pub use secret_policy::is_secret_like_key;
pub use validate::ValidationError;

/// The immutable runtime snapshot built once during startup.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub app: AppConfig,
    pub http: HttpConfig,
    pub health: HealthConfig,
    pub log: LogConfig,
    pub observability: ObservabilityConfig,
    // template:begin authn:config-field
    pub authn: AuthnConfig,
    // template:end authn:config-field
    // template:begin postgres:config-field
    pub postgres: PostgresConfig,
    // template:end postgres:config-field
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
        self.health.validate()?;
        self.log.validate()?;
        self.observability.validate()?;
        // template:begin authn:config-validate
        self.authn.validate()?;
        // template:end authn:config-validate
        // template:begin postgres:config-validate
        self.postgres.validate()?;
        // template:end postgres:config-validate
        Ok(())
    }
}
