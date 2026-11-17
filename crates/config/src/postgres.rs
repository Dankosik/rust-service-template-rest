use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;
use serde::de::IgnoredAny as IgnoredSection;

use crate::de::{blank_as_none, blank_secret_as_none};
use crate::load::{Error, MigrationLoadError};
use crate::observability::validate_service_name;
use crate::validate::{Rejected, Violations, duration_range, int_range, required};
use crate::{AppConfig, LogConfig, OtelConfig};

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

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct PostgresFields<Session, Capacity, Deadline> {
    #[serde(deserialize_with = "crate::de::boolean")]
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
    session_budgets: Session,
    /// Upper bound on pooled connections. Size it from the database's
    /// `max_connections` divided across every instance and job that shares
    /// the database, not from the service's concurrency.
    max_connections: Capacity,
    /// Bound on one `migrate` run after it connected, and on each
    /// `-- no-transaction` migration in it. Raise it for the run that builds
    /// an index concurrently on a large table. Only `migrate` checks the range.
    migration_deadline: Deadline,
}

#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
pub(crate) struct SessionBudgetsInput(
    #[serde(deserialize_with = "crate::de::string_enum")] PostgresSessionBudgets,
);

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub(crate) struct PoolCapacity(#[serde(deserialize_with = "crate::de::unsigned_count")] NonZeroU32);

impl Default for PoolCapacity {
    fn default() -> Self {
        Self(const { NonZeroU32::new(4).expect("4 is nonzero") })
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct MigrationDeadline(#[serde(deserialize_with = "crate::de::duration")] Duration);

impl Default for MigrationDeadline {
    fn default() -> Self {
        Self(Duration::from_mins(5))
    }
}

pub(crate) type PostgresInput = PostgresFields<SessionBudgetsInput, PoolCapacity, IgnoredSection>;
type MigrationPostgresInput = PostgresFields<IgnoredSection, IgnoredSection, MigrationDeadline>;
// template:begin jobs:postgres-operator-input
pub(crate) type OperatorPostgresInput =
    PostgresFields<SessionBudgetsInput, IgnoredSection, IgnoredSection>;
// template:end jobs:postgres-operator-input

#[derive(Clone, Debug)]
pub struct PostgresConfig {
    pub dsn: SecretString,
    pub password_file: Option<PathBuf>,
    pub session_budgets: PostgresSessionBudgets,
    pub max_connections: NonZeroU32,
}

fn admit_dsn(
    enabled: bool,
    dsn: Option<SecretString>,
    violations: &mut Violations,
) -> Result<Option<SecretString>, Rejected> {
    if enabled {
        violations
            .take(required("postgres.dsn", dsn, "postgres.enabled = true"))
            .map(Some)
    } else {
        Ok(None)
    }
}

impl PostgresInput {
    pub(crate) fn admit(
        self,
        violations: &mut Violations,
    ) -> Result<Option<PostgresConfig>, Rejected> {
        let dsn = admit_dsn(self.enabled, self.dsn, violations);
        violations.check(int_range(
            "postgres.max_connections",
            self.max_connections.0.get(),
            1..=500,
        ));
        Ok(dsn?.map(|dsn| PostgresConfig {
            dsn,
            password_file: self.password_file,
            session_budgets: self.session_budgets.0,
            max_connections: self.max_connections.0,
        }))
    }
}

// template:begin jobs:postgres-operator-admission
#[derive(Clone, Debug)]
pub struct OperatorPostgresConfig {
    pub dsn: SecretString,
    pub password_file: Option<PathBuf>,
    pub session_budgets: PostgresSessionBudgets,
}

impl OperatorPostgresInput {
    pub(crate) fn admit(
        self,
        violations: &mut Violations,
    ) -> Result<Option<OperatorPostgresConfig>, Rejected> {
        Ok(
            admit_dsn(self.enabled, self.dsn, violations)?.map(|dsn| OperatorPostgresConfig {
                dsn,
                password_file: self.password_file,
                session_budgets: self.session_budgets.0,
            }),
        )
    }
}
// template:end jobs:postgres-operator-admission

#[derive(Clone, Debug)]
pub struct MigrationPostgresConfig {
    pub dsn: SecretString,
    pub password_file: Option<PathBuf>,
    pub migration_deadline: Duration,
}

impl MigrationPostgresInput {
    fn admit(
        self,
        violations: &mut Violations,
    ) -> Result<Option<MigrationPostgresConfig>, Rejected> {
        violations.check(duration_range(
            "postgres.migration_deadline",
            self.migration_deadline.0,
            Duration::from_secs(1)..=Duration::from_hours(24),
        ));
        Ok(
            admit_dsn(self.enabled, self.dsn, violations)?.map(|dsn| MigrationPostgresConfig {
                dsn,
                password_file: self.password_file,
                migration_deadline: self.migration_deadline.0,
            }),
        )
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct MigrationInput {
    #[serde(deserialize_with = "crate::app::deserialize_migration_app")]
    pub(crate) app: AppConfig,
    log: LogConfig,
    observability: MigrationObservabilityInput,
    postgres: MigrationPostgresInput,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct MigrationObservabilityInput {
    metrics: IgnoredSection,
    otel: MigrationOtelInput,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct MigrationOtelInput {
    #[serde(deserialize_with = "crate::de::optional_string")]
    service_name: Option<String>,
    traces_sampler: IgnoredSection,
    traces_sampler_arg: IgnoredSection,
    exporter: IgnoredSection,
}

#[derive(Clone, Debug)]
pub struct MigrationConfig {
    app: AppConfig,
    log: LogConfig,
    service_name: String,
    postgres: MigrationPostgresConfig,
}

impl MigrationConfig {
    #[must_use]
    pub const fn app(&self) -> &AppConfig {
        &self.app
    }

    #[must_use]
    pub const fn log(&self) -> &LogConfig {
        &self.log
    }

    #[must_use]
    pub const fn service_name(&self) -> &str {
        self.service_name.as_str()
    }

    #[must_use]
    pub const fn postgres(&self) -> &MigrationPostgresConfig {
        &self.postgres
    }
}

impl MigrationInput {
    pub(crate) fn admit(self) -> Result<MigrationConfig, MigrationLoadError> {
        let mut violations = Violations::default();
        let v = &mut violations;
        self.app.validate(v);
        self.log.validate(v);
        let service_name = self
            .observability
            .otel
            .service_name
            .unwrap_or_else(|| OtelConfig::default().service_name);
        v.check(validate_service_name(&service_name));
        let postgres = self.postgres.admit(v);
        let Some(postgres) = violations.finish(postgres).map_err(Error::from)? else {
            return Err(MigrationLoadError::PostgresDisabled { log: self.log });
        };
        Ok(MigrationConfig {
            app: self.app,
            log: self.log,
            service_name,
            postgres,
        })
    }
}

#[cfg(test)]
mod tests;
