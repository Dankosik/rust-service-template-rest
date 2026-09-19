//! PostgreSQL profile switch, connection source, and pool capacity.
//!
//! The profile is inert until `enabled` is set. The DSN is the only
//! connection source and, being secret-like, arrives through the environment
//! only; its admission rules (URL form, explicit `sslmode`, no libpq side
//! channels) live in `infra-postgres`, which is the crate that knows what
//! the driver would otherwise read. Timeouts are template constants there
//! as well; the pool size is the one capacity value without a universal
//! safe answer, so it is the one an operator sets.

use std::num::NonZeroU32;

use secrecy::SecretString;
use serde::Deserialize;

use crate::secret_policy::occupied_secret;
use crate::validate::{ValidationError, int_range};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PostgresConfig {
    /// Whether the service opens a pool at startup and reports it in
    /// readiness.
    pub enabled: bool,
    /// `postgres://user:password@host:port/database?sslmode=<mode>`.
    /// Environment only (`APP__POSTGRES__DSN`). Missing, empty, or
    /// whitespace-only is absent (`None`); `enabled` is a separate axis.
    #[serde(default, deserialize_with = "occupied_secret")]
    pub dsn: Option<SecretString>,
    /// Upper bound on pooled connections. Size it from the database's
    /// `max_connections` divided across every instance and job that shares
    /// the database, not from the service's concurrency.
    pub max_connections: u32,
}

impl Default for PostgresConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dsn: None,
            max_connections: 4,
        }
    }
}

impl PostgresConfig {
    /// Whether a DSN value is present. Its shape is validated by the
    /// persistence crate, which owns the admission rules.
    #[must_use]
    pub fn has_dsn(&self) -> bool {
        self.dsn.is_some()
    }

    /// The occupied DSN, or the same validation error `enabled` uses.
    ///
    /// # Errors
    ///
    /// Returns `postgres.dsn` when the value is absent.
    pub fn required_dsn(&self) -> Result<&SecretString, ValidationError> {
        self.dsn.as_ref().ok_or_else(|| {
            ValidationError::new("postgres.dsn", "is required when postgres.enabled = true")
        })
    }

    /// Pool size for the adapter. [`Self::validate`] already rejects 0;
    /// this is the typed form `connect` takes.
    ///
    /// # Errors
    ///
    /// Returns the same key as validation when the size is 0.
    pub fn pool_max_connections(&self) -> Result<NonZeroU32, ValidationError> {
        NonZeroU32::new(self.max_connections).ok_or_else(|| {
            ValidationError::new("postgres.max_connections", "must be greater than 0")
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if self.enabled {
            self.required_dsn()?;
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
        assert!(PostgresConfig::default().required_dsn().is_err());
    }

    #[test]
    fn a_disabled_profile_may_keep_its_dsn_placeholder() {
        let config = PostgresConfig {
            enabled: false,
            dsn: Some(SecretString::from(
                "postgres://u:p@h:5432/d?sslmode=require".to_owned(),
            )),
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
            dsn: Some(SecretString::from(
                "postgres://u:hunter2@h:5432/d?sslmode=require".to_owned(),
            )),
            ..PostgresConfig::default()
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
    }
}
