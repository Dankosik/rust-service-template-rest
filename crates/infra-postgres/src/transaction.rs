//! The transaction seam and the commit-outcome policy.
//!
//! A feature chooses the atomic boundary by calling [`in_tx`] with an async
//! closure; the closure gets the connection, not the transaction, so it
//! cannot commit or roll back on its own. `Ok` commits, `Err` rolls back.
//!
//! The one thing a driver cannot hide is that `COMMIT` may have succeeded
//! on the server after the client stopped hearing from it. [`TxError`]
//! keeps the two cases apart: a commit the server definitely rejected may be
//! retried; a commit whose outcome is unknown must be reconciled against the
//! operation's own identity instead.

use std::time::Duration;

use sqlx::postgres::{PgConnection, PgPool};
use sqlx::{Connection, Postgres, Transaction};

/// Bound on the rollback issued after the closure failed. The connection is
/// returned to the pool either way; `sqlx` rolls a dropped transaction back
/// lazily, this makes the attempt explicit and bounded.
pub const ROLLBACK_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum TxError {
    #[error("postgres transaction: acquire connection: {0}")]
    Acquire(#[source] sqlx::Error),
    #[error("postgres transaction: begin: {0}")]
    Begin(#[source] sqlx::Error),
    /// The server rejected the commit; nothing was written.
    #[error("postgres transaction: commit rejected: {0}")]
    CommitFailed(#[source] sqlx::Error),
    /// The client did not receive a definitive commit result. The server
    /// may have committed. Callers must reconcile or preserve the original
    /// operation identity instead of retrying blindly.
    #[error("postgres commit outcome unknown: {0}")]
    CommitUnknown(#[source] sqlx::Error),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Isolation {
    #[default]
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

/// How the transaction is opened. The default is the server default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxOptions {
    pub isolation: Isolation,
    pub read_only: bool,
}

impl TxOptions {
    /// The `BEGIN` statement; `None` when the server default applies.
    fn begin_statement(self) -> Option<&'static str> {
        Some(match (self.isolation, self.read_only) {
            (Isolation::ReadCommitted, false) => return None,
            (Isolation::ReadCommitted, true) => "BEGIN ISOLATION LEVEL READ COMMITTED READ ONLY",
            (Isolation::RepeatableRead, false) => "BEGIN ISOLATION LEVEL REPEATABLE READ",
            (Isolation::RepeatableRead, true) => "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY",
            (Isolation::Serializable, false) => "BEGIN ISOLATION LEVEL SERIALIZABLE",
            (Isolation::Serializable, true) => "BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY",
        })
    }
}

/// Run `f` inside one transaction with the server's default isolation.
///
/// # Errors
///
/// The closure's error after a bounded rollback, or a [`TxError`] converted
/// into it.
pub async fn in_tx<T, E, F>(pool: &PgPool, f: F) -> Result<T, E>
where
    F: AsyncFnOnce(&mut PgConnection) -> Result<T, E>,
    E: From<TxError>,
{
    in_tx_with(pool, TxOptions::default(), f).await
}

/// Run `f` inside one transaction opened with `options`.
///
/// # Errors
///
/// The closure's error after a bounded rollback, or a [`TxError`] converted
/// into it.
pub async fn in_tx_with<T, E, F>(pool: &PgPool, options: TxOptions, f: F) -> Result<T, E>
where
    F: AsyncFnOnce(&mut PgConnection) -> Result<T, E>,
    E: From<TxError>,
{
    let mut conn = pool.acquire().await.map_err(TxError::Acquire)?;
    let mut tx: Transaction<'_, Postgres> = match options.begin_statement() {
        None => conn.begin().await,
        Some(statement) => conn.begin_with(statement).await,
    }
    .map_err(TxError::Begin)?;

    match f(&mut tx).await {
        Ok(value) => {
            tx.commit().await.map_err(classify_commit)?;
            Ok(value)
        }
        Err(err) => {
            match tokio::time::timeout(ROLLBACK_TIMEOUT, tx.rollback()).await {
                Ok(Ok(())) => {}
                Ok(Err(rollback)) => {
                    tracing::warn!(error = %rollback, "postgres rollback failed");
                }
                Err(_) => tracing::warn!(
                    budget = ?ROLLBACK_TIMEOUT,
                    "postgres rollback exceeded its budget"
                ),
            }
            Err(err)
        }
    }
}

/// Whether `err` is a failure the same request could succeed at if it ran
/// again: a serialization failure or a deadlock.
///
/// There is deliberately no retry loop here. Whether a retry is safe depends
/// on what the caller already did: a serialization failure in a read-only
/// query is free to retry, the same failure after an outbound side effect is
/// not.
#[must_use]
pub fn retryable(err: &sqlx::Error) -> bool {
    sqlstate(err).is_some_and(|code| code == "40001" || code == "40P01")
}

/// Preserve failures known to have rejected the commit and mark every other
/// commit response as an unknown durable outcome.
fn classify_commit(err: sqlx::Error) -> TxError {
    if sqlstate(&err)
        .as_deref()
        .is_some_and(commit_definitely_failed)
    {
        TxError::CommitFailed(err)
    } else {
        TxError::CommitUnknown(err)
    }
}

/// SQLSTATE class 23 (integrity constraint violation, which a deferred
/// constraint raises at commit) and class 40 (transaction rollback), except
/// `40003` statement completion unknown, which is the server saying it does
/// not know either.
fn commit_definitely_failed(code: &str) -> bool {
    code.starts_with("23") || (code.starts_with("40") && code != "40003")
}

fn sqlstate(err: &sqlx::Error) -> Option<String> {
    match err {
        sqlx::Error::Database(db) => db.code().map(std::borrow::Cow::into_owned),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_statements_render_isolation_and_read_only() {
        assert_eq!(TxOptions::default().begin_statement(), None);
        assert_eq!(
            TxOptions {
                isolation: Isolation::Serializable,
                read_only: false,
            }
            .begin_statement(),
            Some("BEGIN ISOLATION LEVEL SERIALIZABLE")
        );
        assert_eq!(
            TxOptions {
                isolation: Isolation::ReadCommitted,
                read_only: true,
            }
            .begin_statement(),
            Some("BEGIN ISOLATION LEVEL READ COMMITTED READ ONLY")
        );
    }

    #[test]
    fn commit_classification_by_sqlstate() {
        for code in ["23505", "23503", "40001", "40P01"] {
            assert!(commit_definitely_failed(code), "{code}");
        }
        for code in ["40003", "57014", "08006", "XX000"] {
            assert!(!commit_definitely_failed(code), "{code}");
        }
    }

    #[test]
    fn non_database_commit_errors_are_unknown_outcomes() {
        let err = classify_commit(sqlx::Error::Io(std::io::Error::other("reset")));
        assert!(matches!(err, TxError::CommitUnknown(_)), "{err}");
        assert!(!retryable(&sqlx::Error::PoolTimedOut));
    }
}
