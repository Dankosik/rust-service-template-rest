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
//! A `-- no-transaction` migration carries what PostgreSQL refuses inside a
//! transaction (`CREATE INDEX CONCURRENTLY`). Its statement is bounded by the
//! remaining run deadline instead of the session `statement_timeout` and
//! `lock_timeout`, and it is refused while the database holds an invalid
//! index, which is what an interrupted concurrent build leaves behind.
//!
//! Forward-only by policy. The source rules the resolver does not enforce
//! are a unit test over the embedded set, including unique versions.

use std::sync::Mutex;
use std::time::Duration;

use infra_postgres::{Dsn, SessionOptions, connect_session, observed, sqlstate, to_runtime_param};
use sqlx::Connection;
use sqlx::migrate::{AppliedMigration, Migrate, MigrateError, Migration, Migrator};
use sqlx::postgres::{PgConnection, PgPool};

pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Session `statement_timeout` and `idle_in_transaction_session_timeout` of
/// the migration connection. DDL on a large table legitimately runs longer
/// than a request, and a statement that holds a table lock must not run for
/// as long as [`RunOptions::deadline`] allows. A `-- no-transaction`
/// migration is bounded by the remaining deadline instead.
pub const STATEMENT_TIMEOUT: Duration = Duration::from_secs(120);
/// Session `lock_timeout`, which also bounds the wait for the advisory
/// session lock another migrator may hold. A `-- no-transaction` migration
/// waits up to the remaining deadline instead: a concurrent build waits for
/// every transaction older than itself and blocks no write while it does.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound on startup history admission, including pool acquire and its
/// read-only history statements.
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
    /// The history table exists in the database but is outside this
    /// session's `search_path`, so its absence is a visibility failure.
    #[error("the migration history table is not visible to this session")]
    NotVisible,
    /// Pool, statement, decoding, or deadline failures stay private.
    #[error("the migration history is unavailable")]
    Unavailable,
}

/// Read-only startup check: no bookkeeping is created and no lock is taken.
///
/// Bounded by [`HISTORY_VERIFY_BUDGET`], including acquire. A version above
/// the newest embedded migration belongs to a later release and is admitted.
/// An absent table is [`HistoryError::Pending`]; a table outside the session's
/// `search_path` is [`HistoryError::NotVisible`]. Errors are sanitized.
///
/// # Errors
///
/// A bounded, sanitized [`HistoryError`].
pub async fn verify_history(pool: &PgPool) -> Result<(), HistoryError> {
    let check = async {
        let mut conn = infra_postgres::acquire(pool, "check migration history").await?;
        read_history(&mut conn, &MIGRATOR.table_name).await
    };
    let history = match tokio::time::timeout(HISTORY_VERIFY_BUDGET, check).await {
        Ok(Ok(HistoryRead::History(history))) => history,
        Ok(Ok(HistoryRead::Missing)) => Vec::new(),
        Ok(Ok(HistoryRead::NotVisible)) => return Err(HistoryError::NotVisible),
        // A failed row is divergent history, not an unavailable database.
        Ok(Err(MigrateError::Dirty(_))) => return Err(HistoryError::Mismatch),
        Ok(Err(_)) | Err(_) => return Err(HistoryError::Unavailable),
    };
    match pending(&MIGRATOR, &history) {
        Ok(migrations) if migrations.is_empty() => Ok(()),
        Ok(_) => Err(HistoryError::Pending),
        Err(_) => Err(HistoryError::Mismatch),
    }
}

/// What the read-only history read observed.
enum HistoryRead {
    History(Vec<AppliedMigration>),
    /// No table with the history name exists in any schema.
    Missing,
    /// The table exists, but this session cannot see it.
    NotVisible,
}

async fn read_history(conn: &mut PgConnection, table: &str) -> Result<HistoryRead, MigrateError> {
    match applied_history(conn, table).await {
        Ok(history) => Ok(HistoryRead::History(history)),
        // An absent history table means nothing is applied yet. A table that
        // exists outside this session's search_path is a visibility failure,
        // not an empty history, and must not send the operator to the migrator.
        Err(MigrateError::Execute(err)) if sqlstate(&err).as_deref() == Some(UNDEFINED_TABLE) => {
            if history_table_exists_elsewhere(conn, table).await? {
                Ok(HistoryRead::NotVisible)
            } else {
                Ok(HistoryRead::Missing)
            }
        }
        Err(error) => Err(error),
    }
}

/// Whether a relation with the history table's name exists in any schema.
///
/// The runtime history table is unqualified, so the relation name is the last
/// dotted component with surrounding quotes removed.
async fn history_table_exists_elsewhere(
    conn: &mut PgConnection,
    table: &str,
) -> Result<bool, MigrateError> {
    let relation = table.rsplit('.').next().unwrap_or(table).trim_matches('"');
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relname = $1 AND relkind = 'r')",
    )
    .bind(relation)
    .fetch_one(conn)
    .await
    .map_err(MigrateError::Execute)
}

/// Budgets for one run. [`RunOptions::defaults`] is what the binary uses,
/// with `postgres.migration_deadline`; tests shorten them to observe a
/// failure.
#[derive(Clone, Debug)]
pub struct RunOptions<'a> {
    pub dsn: &'a Dsn,
    /// Reported as `application_name` for the migration session.
    pub application_name: &'a str,
    /// Bound on establishing the migration connection, applied before `deadline` starts.
    pub connect_timeout: Duration,
    /// Bound on lock, history, and every pending migration after a live
    /// session exists. Connect is not inside this value; a
    /// `-- no-transaction` migration receives the remaining deadline.
    pub deadline: Duration,
    /// Session `statement_timeout`, and `idle_in_transaction_session_timeout`, of the migration connection.
    pub statement_timeout: Duration,
    /// Session `lock_timeout` of the migration connection; it also bounds the wait for the advisory session lock.
    pub lock_timeout: Duration,
}

impl<'a> RunOptions<'a> {
    /// Options with the default connect timeout, [`STATEMENT_TIMEOUT`], and [`LOCK_TIMEOUT`], and the given application name and run deadline.
    #[must_use]
    pub const fn defaults(dsn: &'a Dsn, application_name: &'a str, deadline: Duration) -> Self {
        Self {
            dsn,
            application_name,
            connect_timeout: infra_postgres::ACQUIRE_TIMEOUT,
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
            Self::InvalidIndexes { .. } | Self::Migrate(MigrateError::ExecuteMigration(..)) => {
                "execute"
            }
            Self::Migrate(MigrateError::Execute(sqlx::Error::Database(db)))
                if db.code().as_deref() == Some(LOCK_NOT_AVAILABLE) =>
            {
                "lock"
            }
            Self::DuplicateVersion(_) | Self::Migrate(_) => "history",
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
/// the failure. [`RunError::stage`] names where it stopped. Each migration
/// and its history row share one transaction, except a `-- no-transaction`
/// migration, whose row is written after its statement.
pub async fn run(migrator: &Migrator, options: &RunOptions<'_>) -> Result<Report, RunFailure> {
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
    let mut conn = tokio::time::timeout(
        options.connect_timeout,
        connect_session(options.dsn, &session),
    )
    .await
    .map_err(|_| RunFailure {
        error: RunError::ConnectTimeout(options.connect_timeout),
        report: Report::default(),
    })?
    .map_err(|error| RunFailure {
        error: RunError::Connect(error),
        report: Report::default(),
    })?;
    let deadline = tokio::time::Instant::now() + options.deadline;
    // The run owns one progress snapshot shared with the timeout wrapper, so a
    // deadline still reports the migrations that committed before it fired.
    let progress = Mutex::new(Report::default());
    let report = match tokio::time::timeout_at(
        deadline,
        apply_pending(&mut conn, migrator, options, deadline, &progress),
    )
    .await
    {
        Ok(Ok(report)) => report,
        Ok(Err(error)) => {
            return Err(RunFailure {
                error,
                report: report_snapshot(&progress),
            });
        }
        Err(_) => {
            return Err(RunFailure {
                error: RunError::Deadline(options.deadline),
                report: report_snapshot(&progress),
            });
        }
    };
    // A close error must not fail an applied run. On failure the connection is
    // dropped instead, which ends the session, its lock, and any transaction.
    let _ = conn.close().await;
    Ok(report)
}

fn report_snapshot(progress: &Mutex<Report>) -> Report {
    progress
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The `sqlx migrate run` sequence over the [`Migrate`] trait.
///
/// The lock is held before the history is read, so `before` and `applied`
/// describe this run.
async fn apply_pending(
    conn: &mut PgConnection,
    migrator: &Migrator,
    options: &RunOptions<'_>,
    deadline: tokio::time::Instant,
    progress: &Mutex<Report>,
) -> Result<Report, RunError> {
    conn.lock().await?;
    conn.ensure_migrations_table(&migrator.table_name).await?;
    let history = applied_history(conn, &migrator.table_name).await?;
    let mut report = Report {
        before: history.iter().map(|migration| migration.version).max(),
        applied: Vec::new(),
    };
    *progress
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = report.clone();
    for migration in pending(migrator, &history)? {
        tracing::event!(
            target: "migrate_record",
            tracing::Level::INFO,
            migration.version = migration.version,
            migration.transaction = !migration.no_tx,
            "migration_applying"
        );
        let elapsed = if migration.no_tx {
            apply_outside_transaction(conn, &migrator.table_name, migration, options, deadline)
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
        *progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = report.clone();
    }
    conn.unlock().await?;
    Ok(report)
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
    deadline: tokio::time::Instant,
) -> Result<Duration, RunError> {
    let invalid = observed(
        "SELECT pg_index",
        sqlx::query_scalar!(
            // `relkind = 'i'` leaves out the index of a partitioned table,
            // which is invalid by design until every partition is attached.
            r#"SELECT c.oid::regclass::text AS "name!" FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid WHERE NOT i.indisvalid AND c.relkind = 'i' ORDER BY 1"#
        )
        .fetch_all(&mut *conn),
    )
    .await
    .map_err(MigrateError::Execute)?;
    if !invalid.is_empty() {
        return Err(RunError::InvalidIndexes {
            version: migration.version,
            indexes: invalid.join(", "),
        });
    }
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Err(RunError::Deadline(options.deadline));
    }
    set_session_timeouts(conn, remaining, remaining).await?;
    let elapsed = conn.apply(table, migration).await?;
    set_session_timeouts(conn, options.statement_timeout, options.lock_timeout).await?;
    Ok(elapsed)
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

#[derive(Debug, thiserror::Error)]
enum PendingError {
    #[error(transparent)]
    Migrate(#[from] MigrateError),
    #[error("migration version {0} is embedded more than once")]
    DuplicateVersion(i64),
}

impl From<PendingError> for RunError {
    fn from(error: PendingError) -> Self {
        match error {
            PendingError::Migrate(error) => Self::Migrate(error),
            PendingError::DuplicateVersion(version) => Self::DuplicateVersion(version),
        }
    }
}

/// Embedded migrations the history has not applied yet.
///
/// A changed checksum is `VersionMismatch` and an applied version unknown
/// inside the embedded range is `VersionMissing`, as in `Migrator::run`. A
/// version above the newest embedded one belongs to a later release and is
/// admitted, as Flyway admits future migrations by default, so a rolled-back
/// release still migrates and starts. Down migrations are skipped, as in
/// `Migrator::run`; the embedded set rejects them separately.
fn pending<'m>(
    migrator: &'m Migrator,
    history: &[AppliedMigration],
) -> Result<Vec<&'m Migration>, PendingError> {
    let mut previous = None;
    for migration in migrator
        .iter()
        .filter(|migration| !migration.migration_type.is_down_migration())
    {
        if previous == Some(migration.version) {
            return Err(PendingError::DuplicateVersion(migration.version));
        }
        previous = Some(migration.version);
    }
    let newest = migrator
        .iter()
        .filter(|migration| !migration.migration_type.is_down_migration())
        .map(|migration| migration.version)
        .max();
    for applied in history {
        match migrator.iter().find(|migration| {
            migration.version == applied.version && !migration.migration_type.is_down_migration()
        }) {
            Some(embedded) if embedded.checksum != applied.checksum => {
                return Err(PendingError::Migrate(MigrateError::VersionMismatch(
                    applied.version,
                )));
            }
            None if newest.is_some_and(|newest| applied.version < newest) => {
                return Err(PendingError::Migrate(MigrateError::VersionMissing(
                    applied.version,
                )));
            }
            _ => {}
        }
    }
    Ok(migrator
        .iter()
        .filter(|migration| !migration.migration_type.is_down_migration())
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
    fn the_embedded_set_has_unique_versions() {
        let mut previous = None;
        for migration in MIGRATOR.iter() {
            assert_ne!(
                previous,
                Some(migration.version),
                "migration version {} is embedded more than once",
                migration.version
            );
            previous = Some(migration.version);
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
            migration(
                20_260_918_120_002,
                "widgets sku index",
                MigrationType::Simple,
                true,
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
            Err(PendingError::Migrate(MigrateError::VersionMissing(
                20_260_926_125_000
            )))
        ));
        assert!(matches!(
            pending(
                &migrator,
                &[AppliedMigration {
                    version: first,
                    checksum: Cow::Owned(vec![0]),
                }]
            ),
            Err(PendingError::Migrate(MigrateError::VersionMismatch(version))) if version == first
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
    fn duplicate_versions_are_refused_before_any_history_is_compared() {
        let duplicate = 20_260_926_120_000;
        let migrator = Migrator::with_migrations(vec![
            migration(duplicate, "first", MigrationType::Simple, false),
            migration(duplicate, "second", MigrationType::Simple, false),
        ]);
        assert!(matches!(
            pending(&migrator, &[]),
            Err(PendingError::DuplicateVersion(version)) if version == duplicate
        ));
    }

    #[test]
    fn pending_skips_down_migrations() {
        let version = 20_260_926_120_000;
        let migrator = Migrator::with_migrations(vec![
            migration(version, "up", MigrationType::ReversibleUp, false),
            migration(version, "down", MigrationType::ReversibleDown, false),
        ]);
        let waiting = pending(&migrator, &[]).unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].migration_type, MigrationType::ReversibleUp);
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
