//! PostgreSQL profile switch, connection source, and pool capacity.
//!
//! The profile is inert until `enabled` is set. The DSN is the only
//! connection source and, being secret-like, arrives only as its `APP__`
//! variable; its admission rules (URL form, explicit `sslmode`, no libpq side
//! channels) live in `infra-postgres`, which is the crate that knows what
//! the driver would otherwise read. Timeouts are template constants there
//! as well; the pool size is the one capacity value without a universal
//! safe answer, so it is the one an operator sets. Two keys describe the
//! deployment rather than tune it: where the password comes from when the
//! platform rotates it, and whether a pooler in front of the database lets
//! the service publish its session budgets. One key bounds a migration run,
//! because an index built concurrently takes as long as that database's
//! table is large.
//!
//! [`MigrationConfig`] is the narrower snapshot the migration binary loads.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;

use crate::de::{blank_as_none, blank_secret_as_none};
use crate::validate::{ValidationError, duration_range, int_range, required};
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
    /// running service follows the file. Missing, empty, or whitespace-only
    /// is unset (`None`), the default.
    #[serde(default, deserialize_with = "blank_as_none")]
    pub password_file: Option<PathBuf>,
    /// Where each pooled session's `statement_timeout` and `idle_in_transaction_session_timeout` come from; `startup` by default.
    pub session_budgets: PostgresSessionBudgets,
    /// Upper bound on pooled connections. Size it from the database's
    /// `max_connections` divided across every instance and job that shares
    /// the database, not from the service's concurrency.
    pub max_connections: NonZeroU32,
    /// Bound on one `migrate` run after it connected, and on each
    /// `-- no-transaction` migration in it. Raise it for the run that builds
    /// an index concurrently on a large table; the service ignores it.
    #[serde(with = "humantime_serde")]
    pub migration_deadline: Duration,
}

impl Default for PostgresConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dsn: None,
            password_file: None,
            session_budgets: PostgresSessionBudgets::Startup,
            max_connections: const { NonZeroU32::new(4).expect("4 is nonzero") },
            // Above the two-minute statement budget of a transactional
            // migration, so a few of them fit in one run.
            migration_deadline: Duration::from_secs(300),
        }
    }
}

impl PostgresConfig {
    /// The occupied DSN, or the same validation error `enabled` uses.
    ///
    /// # Errors
    ///
    /// Returns `postgres.dsn` when the value is absent.
    pub fn required_dsn(&self) -> Result<&SecretString, ValidationError> {
        required("postgres.dsn", self.dsn.as_ref(), "postgres.enabled = true")
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if self.enabled {
            self.required_dsn()?;
        }
        int_range(
            "postgres.max_connections",
            u64::from(self.max_connections.get()),
            1,
            500,
        )?;
        duration_range(
            "postgres.migration_deadline",
            self.migration_deadline,
            Duration::from_secs(1),
            Duration::from_hours(24),
        )?;
        Ok(())
    }
}

/// The sections the migration binary reads, from the same files and
/// environment as [`crate::Config`].
///
/// Every other section is ignored rather than decoded, so a migration run
/// holds no other section's secrets and another section's mistake cannot
/// stop it; the binaries that use those sections still refuse them at startup.
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
mod tests;
