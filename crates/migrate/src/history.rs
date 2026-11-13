//! The history rule a run and the startup check share, and the read-only
//! startup check itself.

use std::time::Duration;

use infra_postgres::sqlstate;
use sqlx::migrate::{AppliedMigration, Migrate, MigrateError, Migration, Migrator};
use sqlx::postgres::{PgConnection, PgPool};

use crate::MIGRATOR;

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
    /// unknown version inside the embedded range. Two embedded migrations
    /// sharing one version also land here.
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
    let relation = table
        .rsplit_once('.')
        .map_or(table, |(_, name)| name)
        .trim_matches('"');
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM pg_class WHERE relname = $1 AND relkind = 'r') AS "exists!""#,
        relation
    )
    .fetch_one(conn)
    .await
    .map_err(MigrateError::Execute)
}

/// Applied rows. A failed row is dirty history, as in `Migrator::run`.
pub(crate) async fn applied_history(
    conn: &mut PgConnection,
    table: &str,
) -> Result<Vec<AppliedMigration>, MigrateError> {
    if let Some(version) = conn.dirty_version(table).await? {
        return Err(MigrateError::Dirty(version));
    }
    conn.list_applied_migrations(table).await
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PendingError {
    #[error(transparent)]
    Migrate(#[from] MigrateError),
    #[error("migration version {0} is embedded more than once")]
    DuplicateVersion(i64),
}

/// Embedded migrations the history has not applied yet.
///
/// A changed checksum is `VersionMismatch` and an applied version unknown
/// inside the embedded range is `VersionMissing`, as in `Migrator::run`. A
/// version above the newest embedded one belongs to a later release and is
/// admitted, as Flyway admits future migrations by default, so a rolled-back
/// release still migrates and starts. Down migrations are skipped, as in
/// `Migrator::run`; the embedded set rejects them separately. Two embedded
/// migrations sharing one version are [`PendingError::DuplicateVersion`]
/// before any history is compared.
pub(crate) fn pending<'m>(
    migrator: &'m Migrator,
    history: &[AppliedMigration],
) -> Result<Vec<&'m Migration>, PendingError> {
    let embedded: Vec<&Migration> = migrator
        .iter()
        .filter(|migration| !migration.migration_type.is_down_migration())
        .collect();
    if let Some([_, duplicate]) = embedded
        .array_windows()
        .find(|[earlier, later]| earlier.version == later.version)
    {
        return Err(PendingError::DuplicateVersion(duplicate.version));
    }
    let newest = embedded.iter().map(|migration| migration.version).max();
    for applied in history {
        match embedded
            .iter()
            .find(|migration| migration.version == applied.version)
        {
            Some(migration) if migration.checksum != applied.checksum => {
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
    Ok(embedded
        .into_iter()
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

    use sqlx::migrate::MigrationType;

    use super::*;
    use crate::tests::migration;

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
        std::assert_matches!(
            pending(
                &migrator,
                &[applied(first), applied(20_260_926_125_000), applied(second)]
            ),
            Err(PendingError::Migrate(MigrateError::VersionMissing(
                20_260_926_125_000
            )))
        );
        std::assert_matches!(
            pending(
                &migrator,
                &[AppliedMigration {
                    version: first,
                    checksum: Cow::Owned(vec![0]),
                }]
            ),
            Err(PendingError::Migrate(MigrateError::VersionMismatch(version))) if version == first
        );

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
        std::assert_matches!(
            pending(&migrator, &[]),
            Err(PendingError::DuplicateVersion(version)) if version == duplicate
        );
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
}
