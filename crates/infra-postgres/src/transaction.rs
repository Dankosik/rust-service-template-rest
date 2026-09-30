//! The transaction seam and the commit-outcome policy.
//!
//! A feature chooses the atomic boundary by calling [`in_tx`] with an async
//! closure; the closure gets an opaque transaction capability, so it cannot
//! commit or roll back on its own. `Ok` commits, `Err` rolls back.
//!
//! The one thing a driver cannot hide is that `COMMIT` may have succeeded
//! on the server after the client stopped hearing from it. [`TxError`]
//! keeps the two cases apart: a commit the server definitely rejected may be
//! retried; a commit whose outcome is unknown must be reconciled against the
//! operation's own identity instead.

use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnection, PgPool};
use sqlx::{Connection, Executor, Postgres};

use crate::sqlstate;

#[derive(Debug, thiserror::Error)]
pub enum TxError {
    #[error("postgres transaction: acquire connection: {0}")]
    Acquire(#[source] sqlx::Error),
    #[error("postgres transaction: begin: {0}")]
    Begin(#[source] sqlx::Error),
    /// The server rejected the commit, or the transaction was already
    /// aborted by a failed statement the closure did not propagate; nothing
    /// was written.
    #[error("postgres transaction: commit rejected: {0}")]
    CommitFailed(#[source] sqlx::Error),
    /// The client did not receive a definitive commit result. The server
    /// may have committed. Callers must reconcile or preserve the original
    /// operation identity instead of retrying blindly.
    #[error("postgres commit outcome unknown: {0}")]
    CommitUnknown(#[source] sqlx::Error),
}

/// An opaque capability for work inside a provider-owned transaction.
///
/// Only [`connection`] exposes the borrowed connection to provider adapters.
/// The transaction boundary retains commit and rollback ownership.
#[derive(Debug)]
pub struct Tx<'c> {
    conn: &'c mut PgConnection,
    /// Set by [`statement_succeeded`] and cleared by [`connection`].
    last_statement_succeeded: bool,
}

/// The connection borrowed by `tx` for a provider adapter's statement.
///
/// Do not issue transaction-control SQL through this connection.
pub fn connection<'a>(tx: &'a mut Tx<'_>) -> &'a mut PgConnection {
    tx.last_statement_succeeded = false;
    tx.conn
}

/// Record that the statement just run through [`connection`] succeeded.
///
/// That success proves the transaction is not aborted, so a commit that
/// follows skips its probe statement. Borrowing
/// [`connection`] again withdraws the proof.
pub fn statement_succeeded(tx: &mut Tx<'_>) {
    tx.last_statement_succeeded = true;
}

/// A pooled connection that is closed instead of returned to the pool
/// while `discard` is set.
///
/// sqlx 0.9 does not roll back a `BEGIN` whose future is cancelled: its
/// guard only acts once the transaction depth was incremented, which happens
/// after the server answered. The pool's return ping succeeds, so the
/// connection would re-enter the pool inside an open transaction.
struct DiscardOnDrop {
    connection: PoolConnection<Postgres>,
    discard: bool,
}

impl Drop for DiscardOnDrop {
    fn drop(&mut self) {
        if self.discard {
            self.connection.close_on_drop();
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Isolation {
    /// Omit the isolation clause; the server uses `default_transaction_isolation`.
    #[default]
    ServerDefault,
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

impl Isolation {
    /// The SQL name of the level; `None` leaves the server default.
    #[must_use]
    pub(crate) const fn as_sql(self) -> Option<&'static str> {
        match self {
            Self::ServerDefault => None,
            Self::ReadCommitted => Some("READ COMMITTED"),
            Self::RepeatableRead => Some("REPEATABLE READ"),
            Self::Serializable => Some("SERIALIZABLE"),
        }
    }
}

/// How the transaction is opened. [`Isolation::ServerDefault`] omits the clause.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxOptions {
    pub isolation: Isolation,
    pub read_only: bool,
}

impl TxOptions {
    /// The `BEGIN` statement; `None` when the server default applies.
    ///
    /// Spelled out as literals so `begin_with` takes a static string instead
    /// of dynamic SQL behind `AssertSqlSafe`.
    fn begin_statement(self) -> Option<&'static str> {
        match (self.isolation, self.read_only) {
            (Isolation::ServerDefault, false) => None,
            (Isolation::ServerDefault, true) => Some("BEGIN READ ONLY"),
            (Isolation::ReadCommitted, false) => Some("BEGIN ISOLATION LEVEL READ COMMITTED"),
            (Isolation::ReadCommitted, true) => {
                Some("BEGIN ISOLATION LEVEL READ COMMITTED READ ONLY")
            }
            (Isolation::RepeatableRead, false) => Some("BEGIN ISOLATION LEVEL REPEATABLE READ"),
            (Isolation::RepeatableRead, true) => {
                Some("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
            }
            (Isolation::Serializable, false) => Some("BEGIN ISOLATION LEVEL SERIALIZABLE"),
            (Isolation::Serializable, true) => Some("BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY"),
        }
    }
}

/// Run `f` inside one transaction with the server's default isolation.
///
/// # Errors
///
/// The closure's error, or a [`TxError`] converted into it.
pub async fn in_tx<T, E, F>(pool: &PgPool, f: F) -> Result<T, E>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
    E: From<TxError>,
{
    in_tx_with(pool, TxOptions::default(), f).await
}

/// Run `f` inside one transaction opened with `options`.
///
/// # Errors
///
/// The closure's error, or a [`TxError`] converted into it.
pub async fn in_tx_with<T, E, F>(pool: &PgPool, options: TxOptions, f: F) -> Result<T, E>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
    E: From<TxError>,
{
    let mut guard = DiscardOnDrop {
        connection: pool.acquire().await.map_err(TxError::Acquire)?,
        discard: true,
    };
    let mut tx = match options.begin_statement() {
        None => guard.connection.begin().await,
        Some(statement) => guard.connection.begin_with(statement).await,
    }
    .map_err(TxError::Begin)?;
    guard.discard = false;

    let mut handle = Tx {
        conn: &mut tx,
        last_statement_succeeded: false,
    };
    // On `Err`, dropping `tx` queues the rollback and the pool's return ping
    // sends it, so the caller does not wait a round trip for it. A rollback
    // the server rejects fails that ping, and the pool closes the connection
    // instead of reusing it.
    let value = f(&mut handle).await?;
    if options.read_only || handle.last_statement_succeeded {
        tx.commit().await.map_err(classify_commit)?;
    } else {
        // PostgreSQL answers `COMMIT` in an aborted transaction with a silent
        // `ROLLBACK`, and sqlx does not check the command tag (pgx reports it
        // as `ErrTxCommitRollback`). The probe statement turns that state into
        // `25P02` and the server skips the rest of the message: a closure that
        // swallowed a failed statement must not look committed. One simple
        // query carries probe and commit in one round trip; its trailing
        // `BEGIN` keeps the server in the transaction sqlx still tracks, and
        // dropping `tx` rolls that empty transaction back with the pool's
        // return ping, without a round trip of its own.
        (&mut *tx)
            .execute("SELECT 1; COMMIT; BEGIN")
            .await
            .map_err(classify_commit)?;
    }
    Ok(value)
}

/// Preserve failures known to have rejected the commit and mark every other
/// commit response as an unknown durable outcome.
///
/// A deferred constraint (class `23`) or a transaction rollback (class `40`)
/// reported by `COMMIT` means the server rolled back. `40003` (statement
/// completion unknown) is the exception, as is every transport failure.
/// `25P02` comes from the probe ahead of `COMMIT`: the transaction was
/// already aborted and the server skipped the commit.
fn classify_commit(err: sqlx::Error) -> TxError {
    let rejected = sqlstate(&err).is_some_and(|code| {
        code.starts_with("23") || (code.starts_with("40") && code != "40003") || code == "25P02"
    });
    if rejected {
        TxError::CommitFailed(err)
    } else {
        TxError::CommitUnknown(err)
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
                isolation: Isolation::ServerDefault,
                read_only: true,
            }
            .begin_statement(),
            Some("BEGIN READ ONLY")
        );
        assert_eq!(
            TxOptions {
                isolation: Isolation::ReadCommitted,
                read_only: false,
            }
            .begin_statement(),
            Some("BEGIN ISOLATION LEVEL READ COMMITTED")
        );
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
    fn isolation_names_its_sql_level() {
        assert_eq!(Isolation::ServerDefault.as_sql(), None);
        assert_eq!(Isolation::ReadCommitted.as_sql(), Some("READ COMMITTED"));
        assert_eq!(Isolation::Serializable.as_sql(), Some("SERIALIZABLE"));
    }

    #[test]
    fn commit_classification_by_sqlstate() {
        for code in ["23505", "23503", "40001", "40P01", "25P02"] {
            let err = classify_commit(crate::error::tests::database(code));
            assert!(matches!(err, TxError::CommitFailed(_)), "{code}");
        }
        for code in ["40003", "57014", "08006", "XX000"] {
            let err = classify_commit(crate::error::tests::database(code));
            assert!(matches!(err, TxError::CommitUnknown(_)), "{code}");
        }
        let err = classify_commit(sqlx::Error::Io(std::io::Error::other("reset")));
        assert!(matches!(err, TxError::CommitUnknown(_)), "{err}");
    }
}
