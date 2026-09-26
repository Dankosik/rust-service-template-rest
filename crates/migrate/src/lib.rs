//! Migration execution for the PostgreSQL profile.
//!
//! [`MIGRATOR`] embeds `migrations/` at compile time, so the binary carries
//! its schema history and the image needs no migration directory. [`run`]
//! applies a migrator over one dedicated connection whose session defaults
//! are the migration budgets, under the `sqlx` advisory session lock and an
//! orchestration deadline. `sqlx` owns the append-only history: an applied
//! file that was edited or removed fails the run before anything is applied.
//!
//! Forward-only by policy: a rollback is a new forward migration. The
//! source rules the resolver does not enforce are proven by a test over the
//! embedded set; [`run`] still rejects a migrator that breaks them.

use std::collections::BTreeMap;
use std::time::Duration;

use infra_postgres::{ACQUIRE_TIMEOUT, Dsn, SessionOptions, connect_session, raw_sqlstate};
use sqlx::Connection;
use sqlx::migrate::{Migrate, MigrateError, MigrationType, Migrator};
use sqlx::postgres::{PgConnection, PgPool};
use sqlx::{Row, postgres::PgRow};

/// The repository's migration set.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Bound on lock, history, and every pending migration after a live session
/// exists. Connect is a separate [`ACQUIRE_TIMEOUT`] and is not inside this
/// value.
pub const DEADLINE: Duration = Duration::from_secs(300);
/// Session `statement_timeout` for the migration connection. DDL on a
/// large table legitimately runs longer than a request; [`DEADLINE`] still
/// bounds the run after connect.
pub const MIGRATION_STATEMENT_TIMEOUT: Duration = Duration::from_secs(120);
/// Session `idle_in_transaction_session_timeout` for the migration
/// connection. Same duration as [`MIGRATION_STATEMENT_TIMEOUT`] by policy,
/// kept separate so a later edit of one setting does not silently retune
/// the other.
pub const MIGRATION_IDLE_IN_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(120);
/// Session `lock_timeout`, which also bounds the wait for the advisory
/// session lock another migrator may hold.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound on startup history admission, including pool acquire and its one
/// read-only snapshot query.
pub const HISTORY_VERIFY_BUDGET: Duration = Duration::from_secs(5);

/// A sanitized reason startup cannot admit the embedded migration history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    /// The embedded migration source breaks the template's source rules.
    #[error("the embedded migration source is invalid")]
    Source,
    /// At least one embedded migration is not applied yet, including an
    /// absent history table: the migrator has not run for this release.
    #[error("embedded migrations are pending")]
    Pending,
    /// An applied migration failed or has a different checksum, or the
    /// history holds a version inside the embedded range that the embedded
    /// set does not contain.
    #[error("the migration history does not match the embedded migrations")]
    Mismatch,
    /// Pool, statement, decoding, or deadline failures stay private.
    #[error("the migration history is unavailable")]
    Unavailable,
}

#[derive(Clone, Debug)]
struct HistoryRow {
    version: i64,
    success: bool,
    checksum: Vec<u8>,
}

const HISTORY_QUERY: &str = "SELECT version, success, checksum FROM _sqlx_migrations";

/// Verify that the database applied every embedded migration with its
/// checksum, without creating bookkeeping or applying SQL.
///
/// Versions newer than the newest embedded migration belong to a later
/// release and are admitted, so a rolled-back binary or a replica restarted
/// during a rollout still starts; [`HistoryError`] names every refusal.
///
/// The single statement gives one read-committed snapshot. It deliberately
/// does not use `Migrator::run`, locks, or migration bookkeeping because this
/// is startup admission rather than schema mutation.
///
/// # Errors
///
/// A bounded, sanitized [`HistoryError`].
pub async fn verify_history(pool: &PgPool) -> Result<(), HistoryError> {
    verify_history_with(&MIGRATOR, pool).await
}

async fn verify_history_with(migrator: &Migrator, pool: &PgPool) -> Result<(), HistoryError> {
    validate_source(migrator).map_err(|_| HistoryError::Source)?;
    let result = tokio::time::timeout(
        HISTORY_VERIFY_BUDGET,
        sqlx::query(HISTORY_QUERY).fetch_all(pool),
    )
    .await;
    let rows = match result {
        Ok(Ok(rows)) => rows
            .iter()
            .map(history_row)
            .collect::<Result<Vec<_>, _>>()?,
        // An absent history table means nothing is applied yet: an empty
        // embedded set needs no bookkeeping, a nonempty one is pending.
        Ok(Err(err)) if raw_sqlstate(&err).as_deref() == Some("42P01") => {
            return if migrator.iter().next().is_none() {
                Ok(())
            } else {
                Err(HistoryError::Pending)
            };
        }
        Ok(Err(_)) | Err(_) => return Err(HistoryError::Unavailable),
    };
    compare_history(migrator, &rows)
}

fn history_row(row: &PgRow) -> Result<HistoryRow, HistoryError> {
    Ok(HistoryRow {
        version: row
            .try_get("version")
            .map_err(|_| HistoryError::Unavailable)?,
        success: row
            .try_get("success")
            .map_err(|_| HistoryError::Unavailable)?,
        checksum: row
            .try_get("checksum")
            .map_err(|_| HistoryError::Unavailable)?,
    })
}

/// Admit a history that applied every embedded migration with its checksum.
///
/// A version above the newest embedded migration belongs to a later release
/// that already migrated this database, so an older binary still starts. A
/// failed row, a checksum mismatch, or an unknown version inside the embedded
/// range is divergent history, which outranks pending migrations.
fn compare_history(migrator: &Migrator, rows: &[HistoryRow]) -> Result<(), HistoryError> {
    let mut applied = BTreeMap::new();
    for row in rows {
        if !row.success
            || applied
                .insert(row.version, row.checksum.as_slice())
                .is_some()
        {
            return Err(HistoryError::Mismatch);
        }
    }
    let embedded = migrator
        .iter()
        .map(|migration| (migration.version, migration.checksum.as_ref()))
        .collect::<BTreeMap<_, _>>();
    let newest = embedded.keys().next_back().copied();
    let unknown_in_range = applied.keys().any(|version| {
        !embedded.contains_key(version) && newest.is_some_and(|newest| *version <= newest)
    });
    let changed = embedded.iter().any(|(version, checksum)| {
        applied
            .get(version)
            .is_some_and(|applied| applied != checksum)
    });
    if unknown_in_range || changed {
        return Err(HistoryError::Mismatch);
    }
    if embedded
        .keys()
        .any(|version| !applied.contains_key(version))
    {
        return Err(HistoryError::Pending);
    }
    Ok(())
}

/// Where a run failed. One word per stage, for the terminal record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The embedded set violates a template rule.
    Source,
    /// A connection-class failure: acquire timed out, or the live session
    /// dropped (`Io`/`Tls`) during bookkeeping `Execute`.
    Connect,
    /// The session lock was not acquired inside `lock_timeout`.
    Lock,
    /// The recorded history disagrees with the source, or the history table
    /// could not be read.
    History,
    /// A migration's SQL failed.
    SqlExecute,
    /// The orchestration deadline elapsed; the connection was dropped and
    /// the server rolls back whatever was in flight.
    Deadline,
}

impl Stage {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Connect => "connect",
            Self::Lock => "lock",
            Self::History => "history",
            Self::SqlExecute => "execute",
            Self::Deadline => "deadline",
        }
    }
}

impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why [`validate_source`] rejected the embedded set.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("migration {version} must have a positive version")]
    NonPositiveVersion { version: i64 },
    #[error(
        "migration {version} is reversible; the template is forward-only, write a new migration instead of a .down.sql"
    )]
    Reversible { version: i64 },
    #[error(
        "migration {version} disables its transaction (-- no-transaction); every migration runs in one"
    )]
    TransactionDisabled { version: i64 },
    #[error(
        "migration {version} must be named <version>_<lowercase_snake_case>.sql, got description {description:?}"
    )]
    Description { version: i64, description: String },
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("migration source: {0}")]
    Source(#[source] SourceError),
    #[error("migration connect: {0}")]
    Connect(#[source] sqlx::Error),
    #[error("migration connect exceeded {budget:?}")]
    ConnectTimeout { budget: Duration },
    #[error("migration run exceeded the {budget:?} deadline")]
    Deadline { budget: Duration },
    #[error("migration {stage}: {source}")]
    Migrate {
        stage: Stage,
        #[source]
        source: MigrateError,
    },
}

impl RunError {
    #[must_use]
    pub fn stage(&self) -> Stage {
        match self {
            Self::Source(_) => Stage::Source,
            Self::Connect(_) | Self::ConnectTimeout { .. } => Stage::Connect,
            Self::Deadline { .. } => Stage::Deadline,
            Self::Migrate { stage, .. } => *stage,
        }
    }
}

/// A failed run with what it had observed by then, so the terminal record
/// can report the real `target` and `before`. The error is boxed because
/// `MigrateError` is large and the `Ok` path is the common one.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct FailedRun {
    #[source]
    pub error: Box<RunError>,
    pub observed: RunObservation,
}

/// Partial progress of a run that did not finish applying.
///
/// This is not a completed [`RunResult`]: `after` and `applied` are not
/// counted here, so `0` cannot be read as [`ApplyOutcome::NoChange`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunObservation {
    /// Highest applied version before the run; `None` if history was not
    /// read, or the history is empty.
    pub before: Option<i64>,
    /// Highest version in the source; `None` for an empty set.
    pub target: Option<i64>,
    pub duration: Duration,
}

impl FailedRun {
    #[must_use]
    pub fn stage(&self) -> Stage {
        self.error.stage()
    }
}

/// A finished apply, for the terminal record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunResult {
    /// Highest applied version before the run; `None` on an empty history.
    pub before: Option<i64>,
    /// Highest version in the source; `None` for an empty set.
    pub target: Option<i64>,
    /// Highest applied version after the run.
    pub after: Option<i64>,
    /// Migrations applied by this run.
    pub applied: usize,
    pub duration: Duration,
}

/// Success-path words on the terminal `migration_run` record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// At least one migration was applied.
    Success,
    /// History already matched the source.
    NoChange,
}

impl ApplyOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::NoChange => "no_change",
        }
    }
}

impl RunResult {
    /// [`ApplyOutcome::Success`] when something was applied, [`ApplyOutcome::NoChange`] otherwise.
    #[must_use]
    pub fn outcome(&self) -> ApplyOutcome {
        if self.applied == 0 {
            ApplyOutcome::NoChange
        } else {
            ApplyOutcome::Success
        }
    }
}

/// Budgets for one run. [`RunOptions::defaults`] is what the binary uses;
/// tests shorten them to observe the corresponding failure stage.
#[derive(Clone, Debug)]
pub struct RunOptions<'a> {
    pub dsn: &'a Dsn,
    /// Reported as `application_name` for the migration session.
    pub application_name: &'a str,
    pub deadline: Duration,
    pub statement_timeout: Duration,
    pub idle_in_transaction_timeout: Duration,
    pub lock_timeout: Duration,
}

impl<'a> RunOptions<'a> {
    #[must_use]
    pub fn defaults(dsn: &'a Dsn, application_name: &'a str) -> Self {
        Self {
            dsn,
            application_name,
            deadline: DEADLINE,
            statement_timeout: MIGRATION_STATEMENT_TIMEOUT,
            idle_in_transaction_timeout: MIGRATION_IDLE_IN_TRANSACTION_TIMEOUT,
            lock_timeout: LOCK_TIMEOUT,
        }
    }
}

/// Apply every pending migration in `migrator`.
///
/// # Errors
///
/// [`FailedRun`], whose [`FailedRun::stage`] names where the run stopped.
/// Nothing partial is left behind: each migration and its history row share
/// one transaction, and a dropped connection releases the session lock.
pub async fn run(migrator: &Migrator, options: &RunOptions<'_>) -> Result<RunResult, FailedRun> {
    let started = std::time::Instant::now();
    let mut observed = RunObservation {
        target: migrator.iter().map(|m| m.version).max(),
        ..RunObservation::default()
    };
    let fail = |error: RunError, mut observed: RunObservation| {
        observed.duration = started.elapsed();
        FailedRun {
            error: Box::new(error),
            observed,
        }
    };

    if let Err(error) = validate_source(migrator) {
        return Err(fail(RunError::Source(error), observed));
    }

    let mut conn = match tokio::time::timeout(
        ACQUIRE_TIMEOUT,
        connect_session(
            options.dsn,
            &SessionOptions {
                application_name: options.application_name,
                statement_timeout: options.statement_timeout,
                idle_in_transaction_timeout: options.idle_in_transaction_timeout,
                lock_timeout: options.lock_timeout,
                extra: &[
                    // `CREATE TABLE IF NOT EXISTS` on the history table raises a
                    // notice on every run after the first; the terminal record is
                    // the operator's evidence, not the server's chatter.
                    ("client_min_messages", "warning"),
                ],
            },
        ),
    )
    .await
    {
        Ok(Ok(conn)) => conn,
        Ok(Err(err)) => return Err(fail(RunError::Connect(err), observed)),
        Err(_) => {
            return Err(fail(
                RunError::ConnectTimeout {
                    budget: ACQUIRE_TIMEOUT,
                },
                observed,
            ));
        }
    };

    let outcome = tokio::time::timeout(options.deadline, async {
        // The session lock is taken here, before the history is read, so
        // `before` and `applied` describe this run and not a concurrent
        // one. `Migrator::run` takes the same advisory lock again (it is
        // re-entrant within a session) and releases its own count; the
        // `unlock` below releases ours.
        conn.lock().await?;
        conn.ensure_migrations_table(&migrator.table_name).await?;
        let (before, before_count) = applied_summary(&mut conn, migrator).await?;
        observed.before = before;
        migrator.run(&mut conn).await?;
        let (after, after_count) = applied_summary(&mut conn, migrator).await?;
        conn.unlock().await?;
        Ok::<_, MigrateError>((after, after_count.saturating_sub(before_count)))
    })
    .await;
    // Dropping the connection on any failure ends the session and, with it,
    // the advisory lock and any open transaction.
    let (after, applied) = match outcome {
        Ok(Ok(summary)) => summary,
        Ok(Err(source)) => {
            return Err(fail(
                RunError::Migrate {
                    stage: stage_of(&source),
                    source,
                },
                observed,
            ));
        }
        Err(_) => {
            return Err(fail(
                RunError::Deadline {
                    budget: options.deadline,
                },
                observed,
            ));
        }
    };
    // Success closes gracefully after unlock. A close error must not fail
    // an already-applied run; failure still drops and does not wait on close.
    let _ = conn.close().await;

    Ok(RunResult {
        before: observed.before,
        target: observed.target,
        after,
        applied,
        duration: started.elapsed(),
    })
}

/// Highest applied version and count, independent of history row order.
async fn applied_summary(
    conn: &mut PgConnection,
    migrator: &Migrator,
) -> Result<(Option<i64>, usize), MigrateError> {
    let applied = conn.list_applied_migrations(&migrator.table_name).await?;
    Ok((
        applied.iter().map(|migration| migration.version).max(),
        applied.len(),
    ))
}

/// PostgreSQL `lock_not_available`: `lock_timeout` fired while waiting,
/// including on `pg_advisory_lock`. Bookkeeping `Execute` only.
const LOCK_NOT_AVAILABLE: &str = "55P03";

fn stage_of(err: &MigrateError) -> Stage {
    #[allow(clippy::match_same_arms)] // ExecuteMigration is named-file SQL; `_` is later variants.
    match err {
        MigrateError::Source(_) => Stage::Source,
        // Bookkeeping `Execute` never means named-file SQL.
        MigrateError::Execute(source) => match source {
            sqlx::Error::Database(db) if db.code().as_deref() == Some(LOCK_NOT_AVAILABLE) => {
                Stage::Lock
            }
            // Session drop after connect succeeded; `Connect` is the
            // terminal word for that connection class.
            sqlx::Error::Io(_) | sqlx::Error::Tls(_) => Stage::Connect,
            _ => Stage::History,
        },
        MigrateError::Dirty(_)
        | MigrateError::VersionMismatch(_)
        | MigrateError::VersionMissing(_)
        | MigrateError::VersionNotPresent(_)
        | MigrateError::VersionTooOld(..)
        | MigrateError::VersionTooNew(..) => Stage::History,
        // `ExecuteMigration` is the named file's SQL; `_` is later variants.
        MigrateError::ExecuteMigration(..) => Stage::SqlExecute,
        _ => Stage::SqlExecute,
    }
}

/// The rules `sqlx`'s resolver leaves to the template: positive versions,
/// simple forward-only files, one transaction each, `snake_case` names.
///
/// # Errors
///
/// The first violated rule as [`SourceError`], naming the migration version.
pub fn validate_source(migrator: &Migrator) -> Result<(), SourceError> {
    for migration in migrator.iter() {
        let version = migration.version;
        if version <= 0 {
            return Err(SourceError::NonPositiveVersion { version });
        }
        if migration.migration_type != MigrationType::Simple {
            return Err(SourceError::Reversible { version });
        }
        if migration.no_tx {
            return Err(SourceError::TransactionDisabled { version });
        }
        if !is_canonical_description(&migration.description) {
            return Err(SourceError::Description {
                version,
                description: migration.description.to_string(),
            });
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use sqlx::SqlStr;
    use sqlx::migrate::Migration;

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

    #[test]
    fn the_embedded_set_follows_the_template_rules() {
        validate_source(&MIGRATOR).unwrap();
    }

    #[test]
    fn canonical_simple_migrations_pass() {
        let m = Migrator::with_migrations(vec![
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
        ]);
        validate_source(&m).unwrap();
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
            let err = validate_source(&Migrator::with_migrations(vec![bad])).unwrap_err();
            let message = err.to_string();
            assert!(message.contains(expected), "{message}");
            assert!(message.contains(&version.to_string()), "{message}");
        }
    }

    #[test]
    fn outcome_reflects_applied_count() {
        assert_eq!(RunResult::default().outcome(), ApplyOutcome::NoChange);
        assert_eq!(
            RunResult {
                applied: 1,
                ..RunResult::default()
            }
            .outcome(),
            ApplyOutcome::Success
        );
        assert_eq!(ApplyOutcome::NoChange.as_str(), "no_change");
        assert_eq!(ApplyOutcome::Success.as_str(), "success");
    }

    #[test]
    fn history_admits_a_later_release_and_refuses_pending_or_divergent_history() {
        let (first, second, newer) = (20_260_926_120_000, 20_260_926_130_000, 20_260_926_140_000);
        let migrator = Migrator::with_migrations(vec![
            migration(first, "create widget", MigrationType::Simple, false),
            migration(second, "create gadget", MigrationType::Simple, false),
        ]);
        let checksum = migrator
            .iter()
            .next()
            .expect("an embedded migration")
            .checksum
            .to_vec();
        let applied = |version: i64| HistoryRow {
            version,
            success: true,
            checksum: checksum.clone(),
        };

        for rows in [
            vec![applied(first), applied(second)],
            vec![applied(first), applied(second), applied(newer)],
        ] {
            assert_eq!(compare_history(&migrator, &rows), Ok(()));
        }
        for rows in [vec![], vec![applied(first)], vec![applied(newer)]] {
            assert_eq!(
                compare_history(&migrator, &rows),
                Err(HistoryError::Pending)
            );
        }
        for rows in [
            vec![applied(first), applied(20_260_926_125_000), applied(second)],
            vec![
                HistoryRow {
                    success: false,
                    ..applied(first)
                },
                applied(second),
            ],
            vec![
                HistoryRow {
                    checksum: vec![0],
                    ..applied(first)
                },
                applied(second),
            ],
            vec![HistoryRow {
                checksum: vec![0],
                ..applied(first)
            }],
        ] {
            assert_eq!(
                compare_history(&migrator, &rows),
                Err(HistoryError::Mismatch)
            );
        }

        let empty = Migrator::with_migrations(Vec::new());
        assert_eq!(compare_history(&empty, &[applied(newer)]), Ok(()));
    }

    #[test]
    fn stages_map_from_migrate_errors() {
        assert_eq!(stage_of(&MigrateError::VersionMismatch(3)), Stage::History);
        assert_eq!(stage_of(&MigrateError::Dirty(3)), Stage::History);
        assert_eq!(
            stage_of(&MigrateError::Execute(sqlx::Error::PoolTimedOut)),
            Stage::History
        );
        assert_eq!(
            stage_of(&MigrateError::ExecuteMigration(
                sqlx::Error::PoolTimedOut,
                3
            )),
            Stage::SqlExecute
        );
        assert_eq!(
            stage_of(&MigrateError::Execute(sqlx::Error::Io(
                std::io::Error::other("reset")
            ))),
            Stage::Connect
        );
        assert_eq!(
            RunError::Deadline {
                budget: Duration::from_secs(1)
            }
            .stage(),
            Stage::Deadline
        );
        assert_eq!(
            RunError::Source(SourceError::NonPositiveVersion { version: 0 }).stage(),
            Stage::Source
        );
    }
}
