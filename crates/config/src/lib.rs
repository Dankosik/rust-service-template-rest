//! Typed, validated, immutable runtime configuration.
//!
//! Precedence, last wins: code defaults → `--config` file → `--config-overlay`
//! files in order → `APP__SECTION__KEY` environment variables. Unknown keys
//! anywhere fail startup. Secret-like keys may carry a value only through the
//! environment. Each section owns its type, defaults, and validation in one
//! file; cross-section rules live in [`Config::validate`].
//!
//! The loader is [`config`](https://docs.rs/config) with `serde`; see
//! `docs/configuration-source-policy.md` for why, and for what the two
//! pre-scans in [`load`] add that the crate does not.

pub mod app;
pub mod health;
pub mod http;
pub mod log;
pub mod observability;
pub mod postgres;

mod cli;
mod load;
mod secret_policy;
mod validate;

use serde::Deserialize;

pub use app::{AppConfig, BuildInfo};
pub use cli::LoadOptions;
pub use health::HealthConfig;
pub use http::HttpConfig;
pub use load::{ENV_PREFIX, Error, MAX_FILE_BYTES, load};
pub use log::{LogConfig, LogFormat};
pub use observability::{
    MetricsConfig, ObservabilityConfig, OtelConfig, OtelExporterConfig, TracesSampler,
};
pub use postgres::PostgresConfig;
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
    pub postgres: PostgresConfig,
}

impl Config {
    /// Validate every section, then the rules that span sections.
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
        self.postgres.validate()?;
        Ok(())
    }
}
