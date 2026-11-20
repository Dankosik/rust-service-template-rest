use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;

use crate::de::{
    BlankAsNone, BlankAsNoneVerbatim, Keep, Mode, ModeMarker, Skip, StrictBool, StrictCount,
    StrictDuration, When,
};
use crate::load::{Error, MigrationLoadError};
use crate::observability::{ObservabilityFields, validate_service_name};
use crate::validate::{Rejected, Violations, admit_with, duration_range, int_range, required};
use crate::{AppConfig, BuildInfo, LogConfig};

const DEFAULT_MAX_CONNECTIONS: NonZeroU32 = NonZeroU32::MIN.saturating_add(3);

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

#[serde_with::serde_as]
#[derive(Debug, Deserialize, educe::Educe)]
#[educe(Default(bound = false))]
#[serde(bound(deserialize = ""), deny_unknown_fields, default)]
pub(crate) struct PostgresFields<Session: Mode, Pool: Mode, Migration: Mode> {
    #[serde_as(as = "StrictBool")]
    pub(crate) enabled: bool,
    /// `postgres://user:password@host:port/database?sslmode=<mode>`.
    /// Set `APP__POSTGRES__DSN` in the environment or via `--secrets-dir`.
    /// Missing, empty, or whitespace-only is absent (`None`).
    #[serde_as(as = "BlankAsNoneVerbatim")]
    dsn: Option<SecretString>,
    /// Optional direct connection URL, including credentials, for the migrator.
    #[serde_as(as = "When<Migration, BlankAsNoneVerbatim>")]
    migration_dsn: Option<SecretString>,
    /// A file that holds the password alone, for a platform that rotates it
    /// by rewriting the file; its trailing line breaks are not part of the
    /// password. The DSN then carries no password, and the running service
    /// follows the file. Its target must be a regular file of at most 16 KiB.
    /// Missing, empty, or whitespace-only is unset (`None`), the default.
    #[serde_as(as = "BlankAsNone")]
    password_file: Option<PathBuf>,
    #[serde_as(as = "When<Session>")]
    session_budgets: PostgresSessionBudgets,
    /// Upper bound on pooled connections. Size it from the database's
    /// `max_connections` divided across every instance and job that shares
    /// the database, not from the service's concurrency.
    #[serde_as(as = "When<Pool, Option<StrictCount>>")]
    max_connections: Option<NonZeroU32>,
    /// Enables PostgreSQL LISTEN/NOTIFY for job wakeups. Set to false on producers
    /// and workers using PgBouncer transaction pooling to use periodic polling.
    #[serde_as(as = "When<Pool, StrictBool>")]
    #[educe(Default = true)]
    notifications: bool,
    /// Bound on one `migrate` run after it connected, and on each
    /// `-- no-transaction` migration in it. Raise it for the run that builds
    /// an index concurrently on a large table. Only `migrate` checks the range.
    #[serde_as(as = "When<Migration, StrictDuration>")]
    #[educe(Default(expression = Duration::from_mins(5)))]
    migration_deadline: Duration,
    #[serde(skip)]
    _mode: ModeMarker<(Session, Pool, Migration)>,
}

pub(crate) type PostgresInput = PostgresFields<Keep, Keep, Skip>;
type MigrationPostgresInput = PostgresFields<Skip, Skip, Keep>;
// template:begin jobs:postgres-operator-input
pub(crate) type OperatorPostgresInput = PostgresFields<Keep, Skip, Skip>;
// template:end jobs:postgres-operator-input

#[derive(Clone, Debug)]
pub struct PostgresConfig {
    pub dsn: SecretString,
    pub password_file: Option<PathBuf>,
    pub session_budgets: PostgresSessionBudgets,
    pub max_connections: NonZeroU32,
    pub notifications: bool,
}

fn admit_dsn(
    enabled: bool,
    dsn: Option<SecretString>,
    violations: &mut Violations,
) -> Result<Option<SecretString>, Rejected> {
    if enabled {
        violations
            .record(required("postgres.dsn", dsn, "postgres.enabled = true"))
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
        let max_connections = self.max_connections.unwrap_or(DEFAULT_MAX_CONNECTIONS);
        violations.check(int_range(
            "postgres.max_connections",
            max_connections.get(),
            1..=500,
        ));
        Ok(dsn?.map(|dsn| PostgresConfig {
            dsn,
            password_file: self.password_file,
            session_budgets: self.session_budgets,
            max_connections,
            notifications: self.notifications,
        }))
    }
}

// template:begin jobs:postgres-operator-admission
#[derive(Clone, Debug)]
pub struct OperatorPostgresConfig {
    pub local_development: bool,
    pub dsn: SecretString,
    pub password_file: Option<PathBuf>,
    pub session_budgets: PostgresSessionBudgets,
}

impl OperatorPostgresInput {
    pub(crate) fn admit(
        self,
        local_development: bool,
        violations: &mut Violations,
    ) -> Result<Option<OperatorPostgresConfig>, Rejected> {
        Ok(
            admit_dsn(self.enabled, self.dsn, violations)?.map(|dsn| OperatorPostgresConfig {
                local_development,
                dsn,
                password_file: self.password_file,
                session_budgets: self.session_budgets,
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
            self.migration_deadline,
            Duration::from_secs(1)..=Duration::from_hours(24),
        ));
        let (dsn, password_file) = match self.migration_dsn {
            Some(dsn) => (Some(dsn), None),
            None => (self.dsn, self.password_file),
        };
        Ok(
            admit_dsn(self.enabled, dsn, violations)?.map(|dsn| MigrationPostgresConfig {
                dsn,
                password_file,
                migration_deadline: self.migration_deadline,
            }),
        )
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct MigrationInput {
    app: crate::app::AppFields<Keep, Skip>,
    log: LogConfig,
    observability: ObservabilityFields<Skip>,
    postgres: MigrationPostgresInput,
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
    pub(crate) fn admit(self, build: BuildInfo) -> Result<MigrationConfig, MigrationLoadError> {
        let (app, log, service_name, postgres) = admit_with(|v| {
            let app = self.app.admit(build, v);
            self.log.validate(v);
            let service_name = self.observability.otel.service_name;
            v.check(validate_service_name(&service_name));
            let postgres = self.postgres.admit(v);
            Ok((app, self.log, service_name, postgres?))
        })
        .map_err(Error::from)?;
        let Some(postgres) = postgres else {
            return Err(MigrationLoadError::PostgresDisabled { log });
        };
        Ok(MigrationConfig {
            app,
            log,
            service_name,
            postgres,
        })
    }
}

#[cfg(test)]
mod tests;
