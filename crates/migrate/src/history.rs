use std::time::Duration;

use infra_postgres::{failure_cause, observed, sqlstate};
use sqlx::migrate::{AppliedMigration, Migrate, MigrateError, Migration, Migrator};
use sqlx::postgres::{PgConnection, PgPool};

use crate::MIGRATOR;

pub const HISTORY_VERIFY_BUDGET: Duration = Duration::from_secs(5);

const UNDEFINED_TABLE: &str = "42P01";

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    #[error("embedded migrations are pending")]
    Pending,
    #[error("the migration history does not match the embedded migrations")]
    Mismatch,
    #[error("the migration history table is not visible to this session")]
    NotVisible,
    #[error("the migration history is unavailable")]
    Unavailable,
}

impl HistoryError {
    fn report(
        self,
        stage: &'static str,
        cause: &'static str,
        sqlstate: Option<&str>,
        version: Option<i64>,
    ) -> Self {
        tracing::error!(
            stage,
            cause,
            sqlstate,
            migration.version = version,
            "postgres_history_refused"
        );
        self
    }
}

pub async fn verify_history(pool: &PgPool) -> Result<(), HistoryError> {
    let mut stage = "acquire";
    let check = async {
        let mut conn = infra_postgres::acquire(pool, "check migration history").await?;
        stage = "read";
        read_history(&mut conn, &MIGRATOR.table_name).await
    };
    let history = match tokio::time::timeout(HISTORY_VERIFY_BUDGET, check).await {
        Ok(Ok(HistoryRead::History(history))) => history,
        Ok(Ok(HistoryRead::NotVisible)) => {
            return Err(HistoryError::NotVisible.report(stage, "not_visible", None, None));
        }
        Ok(Err(MigrateError::Dirty(version))) => {
            return Err(HistoryError::Mismatch.report(stage, "dirty", None, Some(version)));
        }
        Ok(Err(MigrateError::Execute(error))) => {
            return Err(HistoryError::Unavailable.report(
                stage,
                failure_cause(&error),
                sqlstate(&error).as_deref(),
                None,
            ));
        }
        Ok(Err(_)) => return Err(HistoryError::Unavailable.report(stage, "driver", None, None)),
        Err(_) => return Err(HistoryError::Unavailable.report(stage, "deadline", None, None)),
    };
    let pending = pending(&MIGRATOR, &history).map_err(|error| {
        let (cause, version) = match error {
            PendingError::ChecksumMismatch(v) => ("checksum_mismatch", v),
            PendingError::MissingVersion(v) => ("version_missing", v),
            PendingError::DuplicateVersion(v) => ("duplicate_version", v),
        };
        HistoryError::Mismatch.report("compare", cause, None, Some(version))
    })?;
    match pending.first() {
        Some(next) => {
            Err(HistoryError::Pending.report("compare", "pending", None, Some(next.version)))
        }
        None => Ok(()),
    }
}

enum HistoryRead {
    History(Vec<AppliedMigration>),
    NotVisible,
}

async fn read_history(conn: &mut PgConnection, table: &str) -> Result<HistoryRead, MigrateError> {
    match applied_history(conn, table).await {
        Ok(history) => Ok(HistoryRead::History(history)),
        Err(MigrateError::Execute(err)) if sqlstate(&err).as_deref() == Some(UNDEFINED_TABLE) => {
            if history_table_exists_elsewhere(conn, table).await? {
                Ok(HistoryRead::NotVisible)
            } else {
                Ok(HistoryRead::History(Vec::new()))
            }
        }
        Err(error) => Err(error),
    }
}

async fn history_table_exists_elsewhere(
    conn: &mut PgConnection,
    table: &str,
) -> Result<bool, MigrateError> {
    let relation = table
        .rsplit_once('.')
        .map_or(table, |(_, name)| name)
        .trim_matches('"');
    observed(
        "SELECT pg_class",
        sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM pg_class WHERE relname = $1 AND relkind = 'r') AS "exists!""#,
            relation
        )
        .fetch_one(conn),
    )
    .await
    .map_err(MigrateError::Execute)
}

pub(crate) async fn applied_history(
    conn: &mut PgConnection,
    table: &str,
) -> Result<Vec<AppliedMigration>, MigrateError> {
    if let Some(version) = conn.dirty_version(table).await? {
        return Err(MigrateError::Dirty(version));
    }
    conn.list_applied_migrations(table).await
}

#[derive(Debug)]
pub(crate) enum PendingError {
    ChecksumMismatch(i64),
    MissingVersion(i64),
    DuplicateVersion(i64),
}

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
                return Err(PendingError::ChecksumMismatch(applied.version));
            }
            None if newest.is_some_and(|newest| applied.version < newest) => {
                return Err(PendingError::MissingVersion(applied.version));
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
            Err(PendingError::MissingVersion(20_260_926_125_000))
        );
        std::assert_matches!(
            pending(
                &migrator,
                &[AppliedMigration {
                    version: first,
                    checksum: Cow::Owned(vec![0]),
                }]
            ),
            Err(PendingError::ChecksumMismatch(version)) if version == first
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
