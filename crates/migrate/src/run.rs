use std::time::Duration;

use infra_postgres::{
    ACQUIRE_TIMEOUT, Dsn, SessionOptions, connect_session, format_pg_duration, observed, sqlstate,
};
use sqlx::Connection;
use sqlx::migrate::{Migrate, MigrateError, Migration, Migrator};
use sqlx::postgres::PgConnection;

use crate::history::{PendingError, applied_history, pending};

const STATEMENT_TIMEOUT: Duration = Duration::from_mins(2);
const LOCK_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
pub struct RunOptions<'a> {
    pub dsn: &'a Dsn,
    pub application_name: &'a str,
    pub deadline: Duration,
    pub statement_timeout: Duration,
    pub lock_timeout: Duration,
}

impl<'a> RunOptions<'a> {
    #[must_use]
    pub const fn defaults(dsn: &'a Dsn, application_name: &'a str, deadline: Duration) -> Self {
        Self {
            dsn,
            application_name,
            deadline,
            statement_timeout: STATEMENT_TIMEOUT,
            lock_timeout: LOCK_TIMEOUT,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub before: Option<i64>,
    pub applied: Vec<i64>,
}

impl Report {
    #[must_use]
    pub fn after(&self) -> Option<i64> {
        self.applied.iter().copied().max().max(self.before)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("migration connect: {0}")]
    Connect(#[source] sqlx::Error),
    #[error("migration connect exceeded {0:?}")]
    ConnectTimeout(Duration),
    #[error("migration run exceeded the {0:?} deadline")]
    Deadline(Duration),
    #[error("migration deadline {0:?} exceeds the monotonic clock range")]
    InvalidDeadline(Duration),
    #[error(
        "migration {version} runs outside a transaction and the database holds invalid indexes ({indexes}); drop each with DROP INDEX CONCURRENTLY, then run again"
    )]
    InvalidIndexes { version: i64, indexes: String },
    #[error("migration version {0} is embedded more than once")]
    DuplicateVersion(i64),
    #[error(transparent)]
    Migrate(#[from] MigrateError),
}

const LOCK_NOT_AVAILABLE: &str = "55P03";

impl RunError {
    fn stage(&self) -> &'static str {
        match self {
            Self::Connect(_)
            | Self::ConnectTimeout(_)
            | Self::Migrate(MigrateError::Execute(sqlx::Error::Io(_) | sqlx::Error::Tls(_))) => {
                "connect"
            }
            Self::Deadline(_) | Self::InvalidDeadline(_) => "deadline",
            Self::InvalidIndexes { .. } | Self::Migrate(MigrateError::ExecuteMigration(..)) => {
                "execute"
            }
            Self::Migrate(MigrateError::Execute(err))
                if sqlstate(err).as_deref() == Some(LOCK_NOT_AVAILABLE) =>
            {
                "lock"
            }
            Self::DuplicateVersion(_) | Self::Migrate(_) => "history",
        }
    }
}

impl From<PendingError> for RunError {
    fn from(error: PendingError) -> Self {
        match error {
            PendingError::Migrate(error) => Self::Migrate(error),
            PendingError::DuplicateVersion(version) => Self::DuplicateVersion(version),
        }
    }
}

impl From<RunError> for RunFailure {
    fn from(error: RunError) -> Self {
        Self {
            error,
            report: Report::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct RunFailure {
    #[source]
    pub error: RunError,
    pub report: Report,
}

impl RunFailure {
    #[must_use]
    pub fn stage(&self) -> &'static str {
        self.error.stage()
    }
}

pub async fn run(migrator: &Migrator, options: &RunOptions<'_>) -> Result<Report, RunFailure> {
    run_cutoff(options.deadline)?;
    let session = SessionOptions {
        application_name: options.application_name,
        statement_timeout: options.statement_timeout,
        idle_in_transaction_timeout: options.statement_timeout,
        lock_timeout: options.lock_timeout,
        extra: &[
            ("client_min_messages", "warning"),
            ("client_connection_check_interval", "1s"),
        ],
    };
    let mut conn = tokio::time::timeout(ACQUIRE_TIMEOUT, connect_session(options.dsn, &session))
        .await
        .map_err(|_| RunError::ConnectTimeout(ACQUIRE_TIMEOUT))?
        .map_err(RunError::Connect)?;
    let deadline_at = run_cutoff(options.deadline)?;
    let mut report = Report::default();
    let outcome = tokio::time::timeout_at(
        deadline_at,
        apply_pending(&mut conn, migrator, options, deadline_at, &mut report),
    )
    .await;
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(RunFailure { error, report }),
        Err(_) => {
            return Err(RunFailure {
                error: RunError::Deadline(options.deadline),
                report,
            });
        }
    }
    let _ = conn.close().await;
    Ok(report)
}

fn run_cutoff(budget: Duration) -> Result<tokio::time::Instant, RunFailure> {
    tokio::time::Instant::now()
        .checked_add(budget)
        .filter(|deadline| deadline.checked_add(Duration::from_millis(1)).is_some())
        .ok_or_else(|| RunError::InvalidDeadline(budget).into())
}

async fn apply_pending(
    conn: &mut PgConnection,
    migrator: &Migrator,
    options: &RunOptions<'_>,
    deadline_at: tokio::time::Instant,
    report: &mut Report,
) -> Result<(), RunError> {
    conn.lock().await?;
    conn.ensure_migrations_table(&migrator.table_name).await?;
    let history = applied_history(conn, &migrator.table_name).await?;
    report.before = history.iter().map(|migration| migration.version).max();
    for migration in pending(migrator, &history)? {
        tracing::event!(
            target: "migrate_record",
            tracing::Level::INFO,
            migration.version = migration.version,
            migration.transaction = !migration.no_tx,
            "migration_applying"
        );
        let elapsed = if migration.no_tx {
            apply_outside_transaction(conn, &migrator.table_name, migration, options, deadline_at)
                .await?
        } else {
            conn.apply(&migrator.table_name, migration).await?
        };
        tracing::event!(
            target: "migrate_record",
            tracing::Level::INFO,
            migration.version = migration.version,
            migration.duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            "migration_applied"
        );
        report.applied.push(migration.version);
    }
    conn.unlock().await?;
    Ok(())
}

async fn apply_outside_transaction(
    conn: &mut PgConnection,
    table: &str,
    migration: &Migration,
    options: &RunOptions<'_>,
    deadline_at: tokio::time::Instant,
) -> Result<Duration, RunError> {
    let invalid = invalid_indexes(conn).await?;
    if !invalid.is_empty() {
        return Err(RunError::InvalidIndexes {
            version: migration.version,
            indexes: invalid.join(", "),
        });
    }
    let remaining = deadline_at.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Err(RunError::Deadline(options.deadline));
    }
    set_session_timeouts(conn, remaining, remaining).await?;
    let elapsed = conn.apply(table, migration).await?;
    set_session_timeouts(conn, options.statement_timeout, options.lock_timeout).await?;
    Ok(elapsed)
}

async fn invalid_indexes(conn: &mut PgConnection) -> Result<Vec<String>, MigrateError> {
    observed(
        "SELECT pg_index",
        sqlx::query_scalar!(
            r#"SELECT c.oid::regclass::text AS "name!" FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid WHERE NOT i.indisvalid AND c.relkind = 'i' ORDER BY 1"#
        )
        .fetch_all(conn),
    )
    .await
    .map_err(MigrateError::Execute)
}

async fn set_session_timeouts(
    conn: &mut PgConnection,
    statement_timeout: Duration,
    lock_timeout: Duration,
) -> Result<(), MigrateError> {
    observed(
        "SELECT set_config",
        sqlx::query!(
            "SELECT set_config('statement_timeout', $1, false) AS statement_timeout, set_config('lock_timeout', $2, false) AS lock_timeout",
            format_pg_duration(statement_timeout),
            format_pg_duration(lock_timeout),
        )
        .fetch_one(conn),
    )
    .await
    .map_err(MigrateError::Execute)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unrepresentable_migration_deadline_is_refused_before_connecting() {
        let dsn = Dsn::admit("postgres://app:pw@127.0.0.1:1/app?sslmode=disable").unwrap();
        let options = RunOptions::defaults(&dsn, "test", Duration::MAX);
        let failure = run(&crate::MIGRATOR, &options).await.unwrap_err();

        std::assert_matches!(failure.error, RunError::InvalidDeadline(budget) if budget == Duration::MAX);
        assert_eq!(failure.report.before, None);
        assert_eq!(failure.report.applied, Vec::<i64>::new());
    }

    #[test]
    fn stages_name_where_the_run_stopped() {
        assert_eq!(
            RunError::Migrate(MigrateError::VersionMismatch(3)).stage(),
            "history"
        );
        assert_eq!(RunError::Migrate(MigrateError::Dirty(3)).stage(), "history");
        assert_eq!(
            RunError::Migrate(MigrateError::Execute(sqlx::Error::PoolTimedOut)).stage(),
            "history"
        );
        assert_eq!(
            RunError::Migrate(MigrateError::ExecuteMigration(sqlx::Error::PoolTimedOut, 3)).stage(),
            "execute"
        );
        assert_eq!(
            RunError::Migrate(MigrateError::Execute(sqlx::Error::Io(
                std::io::Error::other("reset")
            )))
            .stage(),
            "connect"
        );
        assert_eq!(
            RunError::InvalidIndexes {
                version: 3,
                indexes: "widgets_sku".to_owned(),
            }
            .stage(),
            "execute"
        );
        assert_eq!(
            RunError::Deadline(Duration::from_secs(1)).stage(),
            "deadline"
        );
        assert_eq!(RunError::DuplicateVersion(3).stage(), "history");
        assert_eq!(
            RunError::ConnectTimeout(Duration::from_secs(1)).stage(),
            "connect"
        );
    }

    #[test]
    fn report_after_is_the_newest_version() {
        assert_eq!(Report::default().after(), None);
        assert_eq!(
            Report {
                before: Some(5),
                applied: vec![],
            }
            .after(),
            Some(5)
        );
        assert_eq!(
            Report {
                before: Some(5),
                applied: vec![6, 7],
            }
            .after(),
            Some(7)
        );
        assert_eq!(
            Report {
                before: Some(9),
                applied: vec![6],
            }
            .after(),
            Some(9)
        );
        assert_eq!(
            Report {
                before: None,
                applied: vec![7, 6],
            }
            .after(),
            Some(7)
        );
    }
}
