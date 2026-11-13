//! Applying pending migrations over one dedicated session.

use std::time::Duration;

use infra_postgres::{
    ACQUIRE_TIMEOUT, Dsn, SessionOptions, connect_session, observed, sqlstate, to_runtime_param,
};
use sqlx::Connection;
use sqlx::migrate::{Migrate, MigrateError, Migration, Migrator};
use sqlx::postgres::PgConnection;

use crate::history::{PendingError, applied_history, pending};

/// Session `statement_timeout` and `idle_in_transaction_session_timeout` of
/// the migration connection. DDL on a large table legitimately runs longer
/// than a request, and a statement that holds a table lock must not run for
/// as long as [`RunOptions::deadline`] allows. [`apply_outside_transaction`]
/// lifts it.
const STATEMENT_TIMEOUT: Duration = Duration::from_mins(2);
/// Session `lock_timeout`, which also bounds the wait for the advisory
/// session lock another migrator may hold. [`apply_outside_transaction`]
/// lifts it.
const LOCK_TIMEOUT: Duration = Duration::from_secs(15);

/// Budgets for one run. [`RunOptions::defaults`] is what the binary uses,
/// with `postgres.migration_deadline`; tests shorten them to observe a
/// failure.
#[derive(Clone, Debug)]
pub struct RunOptions<'a> {
    pub dsn: &'a Dsn,
    /// Reported as `application_name` for the migration session.
    pub application_name: &'a str,
    /// Bound on lock, history, and every pending migration after a live
    /// session exists. Connect is not inside this value; a
    /// `-- no-transaction` migration receives the remaining deadline.
    /// Must fit the monotonic clock's range.
    pub deadline: Duration,
    /// Session `statement_timeout`, and `idle_in_transaction_session_timeout`, of the migration connection.
    pub statement_timeout: Duration,
    /// Session `lock_timeout` of the migration connection; it also bounds the wait for the advisory session lock.
    pub lock_timeout: Duration,
}

impl<'a> RunOptions<'a> {
    /// Options with the default session statement and lock timeouts, and the given application name and run deadline.
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

/// What one run observed and applied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Newest version the history held before this run.
    ///
    /// `None` for an empty history. May be a later release's version.
    pub before: Option<i64>,
    pub applied: Vec<i64>,
}

impl Report {
    #[must_use]
    pub fn after(&self) -> Option<i64> {
        self.applied.iter().copied().max().max(self.before)
    }
}

/// Why [`run`] stopped.
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
    /// A `-- no-transaction` migration was not started: an interrupted
    /// concurrent build left an index that `IF NOT EXISTS` would keep.
    #[error(
        "migration {version} runs outside a transaction and the database holds invalid indexes ({indexes}); drop each with DROP INDEX CONCURRENTLY, then run again"
    )]
    InvalidIndexes {
        /// Version of the `-- no-transaction` migration that was refused.
        version: i64,
        /// Names of the invalid indexes found, joined with `, `.
        indexes: String,
    },
    /// Two embedded migrations carry one version, so the history can name at
    /// most one of them.
    #[error("migration version {0} is embedded more than once")]
    DuplicateVersion(i64),
    #[error(transparent)]
    Migrate(#[from] MigrateError),
}

/// PostgreSQL `lock_not_available`: `lock_timeout` fired, including on
/// `pg_advisory_lock`.
const LOCK_NOT_AVAILABLE: &str = "55P03";

impl RunError {
    fn stage(&self) -> &'static str {
        match self {
            Self::Connect(_)
            | Self::ConnectTimeout(_)
            // The live session dropped during bookkeeping.
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

/// A failed run and the progress it observed before stopping.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct RunFailure {
    #[source]
    pub error: RunError,
    /// What the run observed before it stopped. `before` and `applied` stay
    /// empty when the history was not read.
    pub report: Report,
}

impl RunFailure {
    /// The `stage` word on the terminal `migration_run` record.
    #[must_use]
    pub fn stage(&self) -> &'static str {
        self.error.stage()
    }
}

/// Apply every pending migration in `migrator`.
///
/// # Errors
///
/// [`RunFailure`] carries the [`RunError`] and the progress observed before
/// the failure. [`RunFailure::stage`] names where it stopped. Each migration
/// and its history row share one transaction, except a `-- no-transaction`
/// migration, whose row is written after its statement.
pub async fn run(migrator: &Migrator, options: &RunOptions<'_>) -> Result<Report, RunFailure> {
    run_cutoff(options.deadline)?;
    let session = SessionOptions {
        application_name: options.application_name,
        statement_timeout: options.statement_timeout,
        idle_in_transaction_timeout: options.statement_timeout,
        lock_timeout: options.lock_timeout,
        extra: &[
            // `CREATE TABLE IF NOT EXISTS` on the history table raises a
            // notice on every run after the first; the terminal record is
            // the operator's evidence, not the server's chatter.
            ("client_min_messages", "warning"),
            // A dropped session (deadline, stop signal) is noticed while a
            // statement still runs, so its locks and transaction end within
            // a second instead of at `statement_timeout` (PostgreSQL 14+).
            ("client_connection_check_interval", "1s"),
        ],
    };
    // The connect bound is applied before the run deadline starts.
    let mut conn = tokio::time::timeout(ACQUIRE_TIMEOUT, connect_session(options.dsn, &session))
        .await
        .map_err(|_| RunFailure {
            error: RunError::ConnectTimeout(ACQUIRE_TIMEOUT),
            report: Report::default(),
        })?
        .map_err(|error| RunFailure {
            error: RunError::Connect(error),
            report: Report::default(),
        })?;
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
    // A close error must not fail an applied run. On failure the connection is
    // dropped instead, which ends the session, its lock, and any transaction.
    let _ = conn.close().await;
    Ok(report)
}

fn run_cutoff(budget: Duration) -> Result<tokio::time::Instant, RunFailure> {
    tokio::time::Instant::now()
        .checked_add(budget)
        // Tokio rounds deadlines up to milliseconds before registering the timer.
        .filter(|deadline| deadline.checked_add(Duration::from_millis(1)).is_some())
        .ok_or_else(|| RunFailure {
            error: RunError::InvalidDeadline(budget),
            report: Report::default(),
        })
}

/// The `sqlx migrate run` sequence over the [`Migrate`] trait.
///
/// The lock is held before the history is read, so `before` and `applied`
/// describe this run.
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

/// Apply one `-- no-transaction` migration.
///
/// Nothing rolls such a statement back, so two things differ from a
/// transactional migration. An invalid index refuses the run before the
/// statement starts: a failed `CREATE INDEX CONCURRENTLY` leaves one, and the
/// `IF NOT EXISTS` that makes the file safe to rerun would otherwise accept
/// it as built. And the statement runs under the deadline instead of the
/// session `statement_timeout` and `lock_timeout`: a concurrent build takes
/// as long as the table is large and waits for every older transaction to
/// end, while holding no lock that blocks writes.
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
            // `relkind = 'i'` leaves out the index of a partitioned table,
            // which is invalid by design until every partition is attached.
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
            to_runtime_param(statement_timeout),
            to_runtime_param(lock_timeout),
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
