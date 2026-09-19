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

use std::time::Duration;

use infra_postgres::{ACQUIRE_TIMEOUT, Dsn, connect_session, to_runtime_param};
use sqlx::Connection;
use sqlx::migrate::{Migrate, MigrateError, MigrationType, Migrator};
use sqlx::postgres::PgConnection;

/// The repository's migration set.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Bound on the whole run: connect, lock, every pending migration.
pub const DEADLINE: Duration = Duration::from_secs(300);
/// Session `statement_timeout` for the migration connection. DDL on a
/// large table legitimately runs longer than a request; the deadline above
/// still bounds the run.
pub const STATEMENT_TIMEOUT: Duration = Duration::from_secs(120);
/// Session `idle_in_transaction_session_timeout` for the migration
/// connection. Same duration as [`STATEMENT_TIMEOUT`] by policy, kept
/// separate so a later edit of one setting does not silently retune the other.
pub const IDLE_IN_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(120);
/// Session `lock_timeout`, which also bounds the wait for the advisory
/// session lock another migrator may hold.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(15);

/// Where a run failed. One word per stage, for the terminal record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The embedded set violates a template rule.
    Source,
    /// The connection could not be established inside its budget.
    Connect,
    /// The session lock was not acquired inside `lock_timeout`.
    Lock,
    /// The recorded history disagrees with the source, or the history table
    /// could not be read.
    History,
    /// A migration's SQL failed.
    Execute,
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
            Self::Execute => "execute",
            Self::Deadline => "deadline",
        }
    }
}

impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("migration source: {0}")]
    Source(String),
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
    pub observed: RunResult,
}

impl FailedRun {
    #[must_use]
    pub fn stage(&self) -> Stage {
        self.error.stage()
    }
}

/// What a run observed, for the terminal record.
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

impl RunResult {
    /// `success` when something was applied, `no_change` otherwise.
    #[must_use]
    pub fn outcome(&self) -> &'static str {
        if self.applied == 0 {
            "no_change"
        } else {
            "success"
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
            statement_timeout: STATEMENT_TIMEOUT,
            idle_in_transaction_timeout: IDLE_IN_TRANSACTION_TIMEOUT,
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
    let mut observed = RunResult {
        target: migrator.iter().map(|m| m.version).max(),
        ..RunResult::default()
    };
    let fail = |error: RunError, mut observed: RunResult| {
        observed.duration = started.elapsed();
        FailedRun {
            error: Box::new(error),
            observed,
        }
    };

    if let Err(message) = validate_source(migrator) {
        return Err(fail(RunError::Source(message), observed));
    }

    let lock_timeout = to_runtime_param(options.lock_timeout);
    let mut conn = match tokio::time::timeout(
        ACQUIRE_TIMEOUT,
        connect_session(
            options.dsn,
            options.application_name,
            options.statement_timeout,
            options.idle_in_transaction_timeout,
            &[
                ("lock_timeout", lock_timeout.as_str()),
                // `CREATE TABLE IF NOT EXISTS` on the history table raises a
                // notice on every run after the first; the terminal record is
                // the operator's evidence, not the server's chatter.
                ("client_min_messages", "warning"),
            ],
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
    let _ = conn.close().await;

    Ok(RunResult {
        after,
        applied,
        duration: started.elapsed(),
        ..observed
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

// `ExecuteMigration` and `_` both yield `Execute`; the named arm is the
// SQL-failed stage, `_` is only the non-exhaustive remainder.
#[allow(clippy::match_same_arms)]
fn stage_of(err: &MigrateError) -> Stage {
    match err {
        MigrateError::Source(_) => Stage::Source,
        MigrateError::Execute(source) => execute_stage(source),
        // `Execute` bookkeeping, dirty history, and version-mismatch variants.
        MigrateError::Dirty(_)
        | MigrateError::VersionMismatch(_)
        | MigrateError::VersionMissing(_)
        | MigrateError::VersionNotPresent(_)
        | MigrateError::VersionTooOld(..)
        | MigrateError::VersionTooNew(..) => Stage::History,
        // `ExecuteMigration` is the named file's SQL.
        MigrateError::ExecuteMigration(..) => Stage::Execute,
        // `ForceNotSupported`, `CreateSchemasNotSupported`, and later variants.
        _ => Stage::Execute,
    }
}

fn execute_stage(err: &sqlx::Error) -> Stage {
    match err {
        // `55P03` is `lock_timeout` while waiting for `pg_advisory_lock`.
        sqlx::Error::Database(db) if db.code().as_deref() == Some("55P03") => Stage::Lock,
        sqlx::Error::Io(_) | sqlx::Error::Tls(_) => Stage::Connect,
        _ => Stage::History,
    }
}

/// The rules `sqlx`'s resolver leaves to the template: positive versions,
/// simple forward-only files, one transaction each, `snake_case` names.
///
/// # Errors
///
/// The first violated rule, naming the migration version.
pub fn validate_source(migrator: &Migrator) -> Result<(), String> {
    for migration in migrator.iter() {
        let version = migration.version;
        if version <= 0 {
            return Err(format!("migration {version} must have a positive version"));
        }
        if migration.migration_type != MigrationType::Simple {
            return Err(format!(
                "migration {version} is reversible; the template is forward-only, write a new migration instead of a .down.sql"
            ));
        }
        if migration.no_tx {
            return Err(format!(
                "migration {version} disables its transaction (-- no-transaction); every migration runs in one"
            ));
        }
        if !is_canonical_description(&migration.description) {
            return Err(format!(
                "migration {version} must be named <version>_<lowercase_snake_case>.sql, got description {:?}",
                migration.description
            ));
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
            assert!(err.contains(expected), "{err}");
            assert!(err.contains(&version.to_string()), "{err}");
        }
    }

    #[test]
    fn outcome_reflects_applied_count() {
        assert_eq!(RunResult::default().outcome(), "no_change");
        assert_eq!(
            RunResult {
                applied: 1,
                ..RunResult::default()
            }
            .outcome(),
            "success"
        );
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
            Stage::Execute
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
        assert_eq!(RunError::Source(String::new()).stage(), Stage::Source);
    }
}
