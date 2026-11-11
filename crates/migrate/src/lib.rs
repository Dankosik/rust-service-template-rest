//! Migration execution for the PostgreSQL profile.
//!
//! [`MIGRATOR`] embeds `migrations/`. [`run()`] applies pending migrations over
//! one dedicated connection with the migration session budgets, under the
//! `sqlx` advisory session lock and a deadline, following the same
//! [`Migrate`](sqlx::migrate::Migrate) sequence as `sqlx migrate run` (sqlx-cli): lock, ensure table,
//! refuse dirty history, compare, apply each pending migration in its own
//! transaction, unlock. [`verify_history`] is the read-only startup check.
//! Both apply one history rule.
//!
//! A `-- no-transaction` migration carries what PostgreSQL refuses inside a
//! transaction (`CREATE INDEX CONCURRENTLY`). It runs under the remaining run
//! deadline instead of the session timeouts and is refused while the database
//! holds an invalid index.
//!
//! Forward-only by policy. The source rules the resolver does not enforce
//! are a unit test over the embedded set, including unique versions, which
//! every run and startup check also refuses.

mod history;
mod run;

use sqlx::migrate::Migrator;

pub use history::{HISTORY_VERIFY_BUDGET, HistoryError, verify_history};
pub use run::{Report, RunError, RunFailure, RunOptions, run};

pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use sqlx::SqlStr;
    use sqlx::migrate::{Migration, MigrationType};

    use super::*;

    pub(crate) fn migration(
        version: i64,
        description: &str,
        kind: MigrationType,
        no_tx: bool,
    ) -> Migration {
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
}
