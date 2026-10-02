//! PostgreSQL profile switch, connection source, and pool capacity.
//!
//! The profile is inert until `enabled` is set. The DSN is the only
//! connection source and, being secret-like, arrives through the environment
//! only; its admission rules (URL form, explicit `sslmode`, no libpq side
//! channels) live in `infra-postgres`, which is the crate that knows what
//! the driver would otherwise read. Timeouts are template constants there
//! as well; the pool size is the one capacity value without a universal
//! safe answer, so it is the one an operator sets. Two keys describe the
//! deployment rather than tune it: where the password comes from when the
//! platform rotates it, and whether a pooler in front of the database lets
//! the service publish its session budgets.
//!
//! [`MigrationConfig`] is the narrower snapshot the migration binary loads.

use std::num::NonZeroU32;
use std::path::PathBuf;

use secrecy::SecretString;
use serde::Deserialize;

use crate::de::blank_secret_as_none;
use crate::validate::{ValidationError, int_range};
use crate::{AppConfig, LogConfig, ObservabilityConfig};

/// Where a pooled session's `statement_timeout` and
/// `idle_in_transaction_session_timeout` come from. The adapter verifies the
/// effective values when the pool opens, whichever is selected.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PostgresSessionBudgets {
    /// The service publishes them in each connection's startup packet.
    #[default]
    Startup,
    /// The database role or database carries them (`ALTER ROLE ... SET`);
    /// for a pooler that refuses startup parameters.
    Server,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PostgresConfig {
    /// Whether the service opens a pool at startup and reports it in
    /// readiness.
    pub enabled: bool,
    /// `postgres://user:password@host:port/database?sslmode=<mode>`.
    /// Environment only (`APP__POSTGRES__DSN`). Missing, empty, or
    /// whitespace-only is absent (`None`); `enabled` is a separate axis.
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    pub dsn: Option<SecretString>,
    /// A file that holds the password alone, for a platform that rotates it
    /// by rewriting the file. The DSN then carries no password, and the
    /// running service follows the file. Unset by default.
    pub password_file: Option<PathBuf>,
    pub session_budgets: PostgresSessionBudgets,
    /// Upper bound on pooled connections. Size it from the database's
    /// `max_connections` divided across every instance and job that shares
    /// the database, not from the service's concurrency.
    pub max_connections: NonZeroU32,
}

impl Default for PostgresConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dsn: None,
            password_file: None,
            session_budgets: PostgresSessionBudgets::Startup,
            max_connections: const { NonZeroU32::new(4).expect("4 is nonzero") },
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

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if self.enabled {
            self.required_dsn()?;
        }
        if self
            .password_file
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(ValidationError::new(
                "postgres.password_file",
                "cannot be empty when set",
            ));
        }
        int_range(
            "postgres.max_connections",
            u64::from(self.max_connections.get()),
            1,
            500,
        )?;
        Ok(())
    }
}

/// The sections the migration binary reads, from the same files and
/// environment as [`crate::Config`].
///
/// Every other section is ignored rather than decoded, so a migration run
/// holds no secret but the DSN and another section's mistake cannot stop
/// it; the binaries that use those sections still refuse them at startup.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct MigrationConfig {
    pub app: AppConfig,
    pub log: LogConfig,
    pub observability: ObservabilityConfig,
    pub postgres: PostgresConfig,
}

impl MigrationConfig {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        self.app.validate()?;
        self.log.validate()?;
        self.observability.validate()?;
        self.postgres.validate()
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
        assert_eq!(config.max_connections.get(), 4);
        assert_eq!(config.password_file, None);
        assert_eq!(config.session_budgets, PostgresSessionBudgets::Startup);
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
    fn an_empty_password_file_path_is_refused() {
        let config = PostgresConfig {
            password_file: Some(PathBuf::new()),
            ..PostgresConfig::default()
        };
        let err = config.validate().unwrap_err();
        assert_eq!(err.key, "postgres.password_file");
    }

    #[test]
    fn pool_size_is_bounded() {
        let config = PostgresConfig {
            max_connections: NonZeroU32::new(501).unwrap(),
            ..PostgresConfig::default()
        };
        let err = config.validate().unwrap_err();
        assert_eq!(err.key, "postgres.max_connections");
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
