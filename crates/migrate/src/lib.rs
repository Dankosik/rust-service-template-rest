//! Migration execution for the PostgreSQL profile.
//!
//! [`MIGRATOR`] embeds `migrations/`. [`run`] applies pending migrations over
//! one dedicated connection with the migration session budgets, under the
//! `sqlx` advisory session lock and a deadline, following the same
//! [`Migrate`] sequence as `sqlx migrate run` (sqlx-cli): lock, ensure table,
//! refuse dirty history, compare, apply each pending migration in its own
//! transaction, unlock. [`verify_history`] is the read-only startup check.
//! Both apply one history rule.
//!
//! Forward-only by policy. The source rules the resolver does not enforce
//! are a unit test over the embedded set.

use std::time::Duration;

use infra_postgres::{Dsn, SessionOptions, connect_session, raw_sqlstate};
use sqlx::Connection;
use sqlx::migrate::{AppliedMigration, Migrate, MigrateError, Migration, Migrator};
use sqlx::postgres::{PgConnection, PgPool};

/// The repository's migration set.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Bound on lock, history, and every pending migration after a live session
/// exists. Connect is a separate [`infra_postgres::ACQUIRE_TIMEOUT`] and is
/// not inside this value.
pub const DEADLINE: Duration = Duration::from_secs(300);
/// Session `statement_timeout` and `idle_in_transaction_session_timeout` of
/// the migration connection. DDL on a large table legitimately runs longer
/// than a request; [`DEADLINE`] still bounds the run.
pub const STATEMENT_TIMEOUT: Duration = Duration::from_secs(120);
/// Session `lock_timeout`, which also bounds the wait for the advisory
/// session lock another migrator may hold.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound on startup history admission, including pool acquire and its one
/// read-only snapshot query.
pub const HISTORY_VERIFY_BUDGET: Duration = Duration::from_secs(5);

/// PostgreSQL `undefined_table`.
const UNDEFINED_TABLE: &str = "42P01";

/// A sanitized reason startup cannot admit the embedded migration history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    /// At least one embedded migration is not applied yet, including an
    /// absent history table: the migrator has not run for this release.
    #[error("embedded migrations are pending")]
    Pending,
    /// An applied migration failed or changed, or the history holds an
    /// unknown version inside the embedded range.
    #[error("the migration history does not match the embedded migrations")]
    Mismatch,
    /// Pool, statement, decoding, or deadline failures stay private.
    #[error("the migration history is unavailable")]
    Unavailable,
}

/// Read-only startup check: no bookkeeping is created and no lock is taken.
///
/// Bounded by [`HISTORY_VERIFY_BUDGET`], including acquire. A version above
/// the newest embedded migration belongs to a later release and is admitted.
/// Errors are sanitized.
///
/// # Errors
///
/// A bounded, sanitized [`HistoryError`].
pub async fn verify_history(pool: &PgPool) -> Result<(), HistoryError> {
    let check = async {
        let mut conn = pool.acquire().await?;
        applied_history(&mut conn, &MIGRATOR.table_name).await
    };
    let history = match tokio::time::timeout(HISTORY_VERIFY_BUDGET, check).await {
        Ok(Ok(history)) => history,
        // A failed row is divergent history, not an unavailable database.
        Ok(Err(MigrateError::Dirty(_))) => return Err(HistoryError::Mismatch),
        // An absent history table means nothing is applied yet.
        Ok(Err(MigrateError::Execute(err)))
            if raw_sqlstate(&err).as_deref() == Some(UNDEFINED_TABLE) =>
        {
            Vec::new()
        }
        Ok(Err(_)) | Err(_) => return Err(HistoryError::Unavailable),
    };
    match pending(&MIGRATOR, &history) {
        Ok(migrations) if migrations.is_empty() => Ok(()),
        Ok(_) => Err(HistoryError::Pending),
        Err(_) => Err(HistoryError::Mismatch),
    }
}

/// Budgets for one run. [`RunOptions::defaults`] is what the binary uses;
/// tests shorten them to observe a failure.
#[derive(Clone, Debug)]
pub struct RunOptions<'a> {
    pub dsn: &'a Dsn,
    /// Reported as `application_name` for the migration session.
    pub application_name: &'a str,
    pub connect_timeout: Duration,
    pub deadline: Duration,
    pub lock_timeout: Duration,
}

impl<'a> RunOptions<'a> {
    #[must_use]
    pub const fn defaults(dsn: &'a Dsn, application_name: &'a str) -> Self {
        Self {
            dsn,
            application_name,
            connect_timeout: infra_postgres::ACQUIRE_TIMEOUT,
            deadline: DEADLINE,
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
    /// Versions this run applied, in order.
    pub applied: Vec<i64>,
}

impl Report {
    /// Newest applied version after the run.
    #[must_use]
    pub fn after(&self) -> Option<i64> {
        self.applied.last().copied().max(self.before)
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
    #[error(transparent)]
    Migrate(#[from] MigrateError),
}

/// PostgreSQL `lock_not_available`: `lock_timeout` fired, including on
/// `pg_advisory_lock`.
const LOCK_NOT_AVAILABLE: &str = "55P03";

impl RunError {
    /// The `stage` word on the terminal `migration_run` record.
    #[must_use]
    pub fn stage(&self) -> &'static str {
        match self {
            Self::Connect(_)
            | Self::ConnectTimeout(_)
            // The live session dropped during bookkeeping.
            | Self::Migrate(MigrateError::Execute(sqlx::Error::Io(_) | sqlx::Error::Tls(_))) => {
                "connect"
            }
            Self::Deadline(_) => "deadline",
            Self::Migrate(MigrateError::ExecuteMigration(..)) => "execute",
            Self::Migrate(MigrateError::Execute(sqlx::Error::Database(db)))
                if db.code().as_deref() == Some(LOCK_NOT_AVAILABLE) =>
            {
                "lock"
            }
            Self::Migrate(_) => "history",
        }
    }
}

/// Apply every pending migration in `migrator`.
///
/// # Errors
///
/// [`RunError`]. [`RunError::stage`] names where it stopped. Each migration
/// and its history row share one transaction.
pub async fn run(migrator: &Migrator, options: &RunOptions<'_>) -> Result<Report, RunError> {
    let session = SessionOptions {
        application_name: options.application_name,
        statement_timeout: STATEMENT_TIMEOUT,
        idle_in_transaction_timeout: STATEMENT_TIMEOUT,
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
    let mut conn = tokio::time::timeout(
        options.connect_timeout,
        connect_session(options.dsn, &session),
    )
    .await
    .map_err(|_| RunError::ConnectTimeout(options.connect_timeout))?
    .map_err(RunError::Connect)?;
    let report = tokio::time::timeout(options.deadline, apply_pending(&mut conn, migrator))
        .await
        .map_err(|_| RunError::Deadline(options.deadline))??;
    // A close error must not fail an applied run. On failure the connection is
    // dropped instead, which ends the session, its lock, and any transaction.
    let _ = conn.close().await;
    Ok(report)
}

/// The `sqlx migrate run` sequence over the [`Migrate`] trait.
///
/// The lock is held before the history is read, so `before` and `applied`
/// describe this run.
async fn apply_pending(
    conn: &mut PgConnection,
    migrator: &Migrator,
) -> Result<Report, MigrateError> {
    conn.lock().await?;
    conn.ensure_migrations_table(&migrator.table_name).await?;
    let history = applied_history(conn, &migrator.table_name).await?;
    let mut report = Report {
        before: history.iter().map(|migration| migration.version).max(),
        applied: Vec::new(),
    };
    for migration in pending(migrator, &history)? {
        conn.apply(&migrator.table_name, migration).await?;
        report.applied.push(migration.version);
    }
    conn.unlock().await?;
    Ok(report)
}

/// Applied rows. A failed row is dirty history, as in `Migrator::run`.
async fn applied_history(
    conn: &mut PgConnection,
    table: &str,
) -> Result<Vec<AppliedMigration>, MigrateError> {
    if let Some(version) = conn.dirty_version(table).await? {
        return Err(MigrateError::Dirty(version));
    }
    conn.list_applied_migrations(table).await
}

/// Embedded migrations the history has not applied yet.
///
/// A changed checksum is `VersionMismatch` and an applied version unknown
/// inside the embedded range is `VersionMissing`, as in `Migrator::run`. A
/// version above the newest embedded one belongs to a later release and is
/// admitted, as Flyway admits future migrations by default, so a rolled-back
/// release still migrates and starts.
fn pending<'m>(
    migrator: &'m Migrator,
    history: &[AppliedMigration],
) -> Result<Vec<&'m Migration>, MigrateError> {
    let newest = migrator.iter().map(|migration| migration.version).max();
    for applied in history {
        match migrator
            .iter()
            .find(|migration| migration.version == applied.version)
        {
            Some(embedded) if embedded.checksum != applied.checksum => {
                return Err(MigrateError::VersionMismatch(applied.version));
            }
            None if newest.is_some_and(|newest| applied.version < newest) => {
                return Err(MigrateError::VersionMissing(applied.version));
            }
            _ => {}
        }
    }
    Ok(migrator
        .iter()
        .filter(|migration| {
            !history
                .iter()
                .any(|applied| applied.version == migration.version)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use sqlx::SqlStr;
    use sqlx::migrate::{Migration, MigrationType};

    use super::*;

    fn migration(version: i64, description: &str, kind: MigrationType, no_tx: bool) -> Migration {
        Migration::new(
            version,
            Cow::Owned(description.to_owned()),
            kind,
            SqlStr::from_static("SELECT 1"),
            no_tx,
        )
    }

    /// The resolver replaced `_` with spaces; the canonical filename therefore
    /// yields words of `[a-z0-9]` separated by single spaces.
    fn is_canonical_description(description: &str) -> bool {
        !description.is_empty()
            && description.split(' ').all(|word| {
                !word.is_empty()
                    && word
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
    }

    fn source_rule_violation(migration: &Migration) -> Option<String> {
        let version = migration.version;
        if version <= 0 {
            return Some(format!("migration {version} must have a positive version"));
        }
        if migration.migration_type != MigrationType::Simple {
            return Some(format!(
                "migration {version} is reversible; the template is forward-only, write a new migration instead of a .down.sql"
            ));
        }
        if migration.no_tx {
            return Some(format!(
                "migration {version} disables its transaction (-- no-transaction); every migration runs in one"
            ));
        }
        if !is_canonical_description(&migration.description) {
            let description = migration.description.as_ref();
            return Some(format!(
                "migration {version} must be named <version>_<lowercase_snake_case>.sql, got description {description:?}"
            ));
        }
        None
    }

    #[test]
    fn the_embedded_set_follows_the_template_rules() {
        for migration in MIGRATOR.iter() {
            assert_eq!(source_rule_violation(migration), None);
        }
    }

    #[test]
    fn canonical_simple_migrations_pass() {
        for migration in [
            migration(
                20_260_918_120_000,
                "create widgets",
                MigrationType::Simple,
                false,
            ),
            migration(
                20_260_918_120_001,
                "widgets add sku 2",
                MigrationType::Simple,
                false,
            ),
        ] {
            assert_eq!(source_rule_violation(&migration), None);
        }
    }

    #[test]
    fn each_rule_names_the_offending_version() {
        let cases = [
            (
                migration(0, "zero", MigrationType::Simple, false),
                "positive version",
            ),
            (
                migration(7, "down", MigrationType::ReversibleDown, false),
                "forward-only",
            ),
            (
                migration(8, "up", MigrationType::ReversibleUp, false),
                "forward-only",
            ),
            (
                migration(9, "concurrently", MigrationType::Simple, true),
                "no-transaction",
            ),
            (
                migration(10, "Create Widgets", MigrationType::Simple, false),
                "snake_case",
            ),
            (
                migration(11, "create-widgets", MigrationType::Simple, false),
                "snake_case",
            ),
            (
                migration(12, "", MigrationType::Simple, false),
                "snake_case",
            ),
        ];
        for (bad, expected) in cases {
            let version = bad.version;
            let message = source_rule_violation(&bad).expect("a violated rule");
            assert!(message.contains(expected), "{message}");
            assert!(message.contains(&version.to_string()), "{message}");
        }
    }

    #[test]
    fn history_admits_a_later_release_and_refuses_pending_or_divergent_history() {
        let (first, second, newer) = (20_260_926_120_000, 20_260_926_130_000, 20_260_926_140_000);
        let migrator = Migrator::with_migrations(vec![
            migration(first, "create widget", MigrationType::Simple, false),
            migration(second, "create gadget", MigrationType::Simple, false),
        ]);
        // Both fixtures are `SELECT 1`, so they share one checksum.
        let checksum = migrator
            .iter()
            .next()
            .expect("an embedded migration")
            .checksum
            .to_vec();
        let applied = |version: i64| AppliedMigration {
            version,
            checksum: Cow::Owned(checksum.clone()),
        };
        let versions = |history: &[AppliedMigration]| {
            pending(&migrator, history).map(|waiting| {
                waiting
                    .iter()
                    .map(|migration| migration.version)
                    .collect::<Vec<_>>()
            })
        };

        assert_eq!(
            versions(&[applied(first), applied(second)]).unwrap(),
            Vec::<i64>::new()
        );
        assert_eq!(
            versions(&[applied(first), applied(second), applied(newer)]).unwrap(),
            Vec::<i64>::new()
        );
        assert_eq!(versions(&[]).unwrap(), vec![first, second]);
        assert_eq!(versions(&[applied(first)]).unwrap(), vec![second]);
        assert_eq!(versions(&[applied(newer)]).unwrap(), vec![first, second]);
        assert!(matches!(
            pending(
                &migrator,
                &[applied(first), applied(20_260_926_125_000), applied(second)]
            ),
            Err(MigrateError::VersionMissing(20_260_926_125_000))
        ));
        assert!(matches!(
            pending(
                &migrator,
                &[AppliedMigration {
                    version: first,
                    checksum: Cow::Owned(vec![0]),
                }]
            ),
            Err(MigrateError::VersionMismatch(version)) if version == first
        ));

        let empty = Migrator::with_migrations(Vec::new());
        assert_eq!(
            pending(&empty, &[applied(newer)])
                .unwrap()
                .iter()
                .map(|migration| migration.version)
                .collect::<Vec<_>>(),
            Vec::<i64>::new()
        );
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
            RunError::Deadline(Duration::from_secs(1)).stage(),
            "deadline"
        );
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
    }
}
