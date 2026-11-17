use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;
use serde::de::IgnoredAny as IgnoredSection;

use crate::de::{blank_as_none, blank_secret_as_none};
use crate::observability::validate_service_name;
use crate::validate::{
    Rejected, ValidationErrors, Violations, duration_range, int_range, required,
};
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

/// What the operator supplied for PostgreSQL, before admission.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct PostgresInput {
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
    #[serde(deserialize_with = "crate::de::string_enum")]
    session_budgets: PostgresSessionBudgets,
    /// Upper bound on pooled connections. Size it from the database's
    /// `max_connections` divided across every instance and job that shares
    /// the database, not from the service's concurrency.
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    max_connections: NonZeroU32,
    /// Bound on one `migrate` run after it connected, and on each
    /// `-- no-transaction` migration in it. Raise it for the run that builds
    /// an index concurrently on a large table. Only `migrate` checks the range.
    #[serde(deserialize_with = "crate::de::duration")]
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
            self.max_connections.get(),
            1..=500,
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

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct MigrationInput {
    pub(crate) app: AppConfig,
    // template:begin authn:migration-input-authn
    authn: IgnoredSection,
    // template:end authn:migration-input-authn
    // template:begin cache:migration-input-cache
    cache: IgnoredSection,
    // template:end cache:migration-input-cache
    // template:begin grpc:migration-input-grpc
    grpc: IgnoredSection,
    // template:end grpc:migration-input-grpc
    health: IgnoredSection,
    http: IgnoredSection,
    // template:begin http-idempotency:migration-input-http-idempotency
    http_idempotency: IgnoredSection,
    // template:end http-idempotency:migration-input-http-idempotency
    // template:begin inbound-webhooks:migration-input-inbound-webhooks
    inbound_webhooks: IgnoredSection,
    // template:end inbound-webhooks:migration-input-inbound-webhooks
    // template:begin jobs:migration-input-jobs
    jobs: IgnoredSection,
    // template:end jobs:migration-input-jobs
    log: LogConfig,
    // template:begin messaging:migration-input-messaging
    messaging: IgnoredSection,
    // template:end messaging:migration-input-messaging
    // template:begin object-storage:migration-input-object-storage
    object_storage: IgnoredSection,
    // template:end object-storage:migration-input-object-storage
    observability: MigrationObservabilityInput,
    postgres: PostgresInput,
    runtime: IgnoredSection,
    // template:begin service-auth:migration-input-service-auth
    service_auth: IgnoredSection,
    // template:end service-auth:migration-input-service-auth
    // template:begin webhooks:migration-input-webhooks
    webhooks: IgnoredSection,
    // template:end webhooks:migration-input-webhooks
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
    pub app: AppConfig,
    pub log: LogConfig,
    pub service_name: String,
    pub postgres: Option<PostgresConfig>,
}

impl Default for MigrationConfig {
    fn default() -> Self {
        Self {
            app: AppConfig::default(),
            log: LogConfig::default(),
            service_name: OtelConfig::default().service_name,
            postgres: None,
        }
    }
}

impl MigrationInput {
    pub(crate) fn admit(self) -> Result<MigrationConfig, ValidationErrors> {
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
        v.check(duration_range(
            "postgres.migration_deadline",
            self.postgres.migration_deadline,
            Duration::from_secs(1)..=Duration::from_hours(24),
        ));
        let postgres = self.postgres.admit(v);
        let admitted = postgres.map(|postgres| MigrationConfig {
            app: self.app,
            log: self.log,
            service_name,
            postgres,
        });
        violations.finish(admitted)
    }
}

#[cfg(test)]
mod tests;
