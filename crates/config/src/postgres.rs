//! PostgreSQL profile switch, connection source, and pool capacity.
//!
//! The profile is inert until `enabled` is set. The DSN is the only
//! connection source and, being secret-like, arrives through the environment
//! only; its admission rules (URL form, explicit `sslmode`, no libpq side
//! channels) live in `infra-postgres`, which is the crate that knows what
//! the driver would otherwise read. Timeouts are template constants there
//! as well; the pool size is the one capacity value without a universal
//! safe answer, so it is the one an operator sets.

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::validate::{ValidationError, int_range};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PostgresConfig {
    /// Whether the service opens a pool at startup and reports it in
    /// readiness.
    pub enabled: bool,
    /// `postgres://user:password@host:port/database?sslmode=<mode>`.
    /// Environment only (`APP__POSTGRES__DSN`).
    pub dsn: SecretString,
    /// Upper bound on pooled connections. Size it from the database's
    /// `max_connections` divided across every instance and job that shares
    /// the database, not from the service's concurrency.
    pub max_connections: u32,
}

impl Default for PostgresConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dsn: SecretString::from(String::new()),
            max_connections: 4,
        }
    }
}

impl PostgresConfig {
    /// Whether a DSN value is present. Its shape is validated by the
    /// persistence crate, which owns the admission rules.
    #[must_use]
    pub fn has_dsn(&self) -> bool {
        !self.dsn.expose_secret().trim().is_empty()
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if self.enabled && !self.has_dsn() {
            return Err(ValidationError::new(
                "postgres.dsn",
                "is required when postgres.enabled = true",
            ));
        }
        int_range(
            "postgres.max_connections",
            u64::from(self.max_connections),
            1,
            500,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_inert() {
        let config = PostgresConfig::default();
        assert!(!config.enabled);
        assert!(!config.has_dsn());
        assert_eq!(config.max_connections, 4);
        config.validate().unwrap();
    }

    #[test]
    fn enabled_requires_a_dsn() {
        let config = PostgresConfig {
            enabled: true,
            ..PostgresConfig::default()
        };
        let err = config.validate().unwrap_err();
        assert_eq!(err.key, "postgres.dsn");
    }

    #[test]
    fn a_disabled_profile_may_keep_its_dsn_placeholder() {
        let config = PostgresConfig {
            enabled: false,
            dsn: SecretString::from("postgres://u:p@h:5432/d?sslmode=require".to_owned()),
            ..PostgresConfig::default()
        };
        config.validate().unwrap();
    }

    #[test]
    fn pool_size_is_bounded() {
        for max_connections in [0, 501] {
            let config = PostgresConfig {
                max_connections,
                ..PostgresConfig::default()
            };
            let err = config.validate().unwrap_err();
            assert_eq!(err.key, "postgres.max_connections");
        }
    }

    #[test]
    fn debug_output_redacts_the_dsn() {
        let config = PostgresConfig {
            dsn: SecretString::from("postgres://u:hunter2@h:5432/d?sslmode=require".to_owned()),
            ..PostgresConfig::default()
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
    }
}
