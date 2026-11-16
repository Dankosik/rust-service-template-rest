use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;

use crate::de::{blank_as_none, blank_secret_as_none};
use crate::validate::{
    Rejected, ValidationErrors, Violations, duration_range, int_range, required,
};
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

/// What the operator supplied for PostgreSQL, before admission.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct PostgresInput {
    pub(crate) enabled: bool,
    /// `postgres://user:password@host:port/database?sslmode=<mode>`.
    /// Set `APP__POSTGRES__DSN` in the environment or via `--secrets-dir`.
    /// Missing, empty, or whitespace-only is absent (`None`).
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    dsn: Option<SecretString>,
    /// A file that holds the password alone, for a platform that rotates it
    /// by rewriting the file; its trailing line breaks are not part of the
    /// password. The DSN then carries no password, and the running service
    /// follows the file. Missing, empty, or whitespace-only is unset (`None`),
    /// the default.
    #[serde(default, deserialize_with = "blank_as_none")]
    password_file: Option<PathBuf>,
    session_budgets: PostgresSessionBudgets,
    /// Upper bound on pooled connections. Size it from the database's
    /// `max_connections` divided across every instance and job that shares
    /// the database, not from the service's concurrency.
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    max_connections: NonZeroU32,
    /// Bound on one `migrate` run after it connected, and on each
    /// `-- no-transaction` migration in it. Raise it for the run that builds
    /// an index concurrently on a large table. Only `migrate` checks the range.
    #[serde(with = "humantime_serde")]
    migration_deadline: Duration,
}

impl Default for PostgresInput {
    fn default() -> Self {
        Self {
            enabled: false,
            dsn: None,
            password_file: None,
            session_budgets: PostgresSessionBudgets::Startup,
            max_connections: const { NonZeroU32::new(4).expect("4 is nonzero") },
            migration_deadline: Duration::from_mins(5),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PostgresConfig {
    pub dsn: SecretString,
    pub password_file: Option<PathBuf>,
    pub session_budgets: PostgresSessionBudgets,
    pub max_connections: NonZeroU32,
    pub migration_deadline: Duration,
}

impl PostgresInput {
    pub(crate) fn admit(
        self,
        violations: &mut Violations,
    ) -> Result<Option<PostgresConfig>, Rejected> {
        let dsn = if self.enabled {
            Some(violations.take(required(
                "postgres.dsn",
                self.dsn,
                "postgres.enabled = true",
            )))
        } else {
            None
        };
        violations.check(int_range(
            "postgres.max_connections",
            u64::from(self.max_connections.get()),
            1,
            500,
        ));
        let Some(dsn) = dsn else {
            return Ok(None);
        };
        Ok(Some(PostgresConfig {
            dsn: dsn?,
            password_file: self.password_file,
            session_budgets: self.session_budgets,
            max_connections: self.max_connections,
            migration_deadline: self.migration_deadline,
        }))
    }
}

/// What the migration binary decoded, from the same files and environment as
/// [`crate::Config`].
///
/// Other sections are ignored during decoding. The shared input rules
/// still apply to all files and variables passed to [`crate::load_migration`].
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct MigrationInput {
    pub(crate) app: AppConfig,
    log: LogConfig,
    observability: ObservabilityConfig,
    postgres: PostgresInput,
}

#[derive(Clone, Debug, Default)]
pub struct MigrationConfig {
    pub app: AppConfig,
    pub log: LogConfig,
    pub observability: ObservabilityConfig,
    pub postgres: Option<PostgresConfig>,
}

impl MigrationInput {
    pub(crate) fn admit(self) -> Result<MigrationConfig, ValidationErrors> {
        let mut violations = Violations::default();
        let v = &mut violations;
        self.app.validate(v);
        self.log.validate(v);
        self.observability.validate(v);
        v.check(duration_range(
            "postgres.migration_deadline",
            self.postgres.migration_deadline,
            Duration::from_secs(1),
            Duration::from_hours(24),
        ));
        let postgres = self.postgres.admit(v);
        let admitted = postgres.map(|postgres| MigrationConfig {
            app: self.app,
            log: self.log,
            observability: self.observability,
            postgres,
        });
        violations.finish(admitted)
    }
}

#[cfg(test)]
mod tests;
