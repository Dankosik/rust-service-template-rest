//! `COMMIT` may have succeeded on the server after the client stopped hearing
//! from it. [`TxError`] separates a definitely rejected commit, which may be
//! retried, from an unknown outcome, which must be reconciled against the
//! operation's own identity.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use futures_util::Stream;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{
    PgConnection, PgPool, PgQueryResult, PgRow, PgStatement, PgTypeInfo, Postgres,
};
use sqlx::{Connection, Either, Execute, Executor, SqlStr};
use tracing::Instrument;

use crate::error::sqlstate;
use crate::observe::{self, TransactionObservation, TransactionOutcome, observed};
use crate::pool::{acquire, acquire_until, format_pg_duration};

#[derive(Debug, thiserror::Error)]
pub enum TxError {
    #[error("postgres transaction: acquire connection: {0}")]
    Acquire(#[source] sqlx::Error),
    #[error("postgres transaction: begin: {0}")]
    Begin(#[source] sqlx::Error),
    /// The commit was rejected or never sent (including a failed pre-commit check);
    /// nothing was written.
    #[error("postgres transaction: not committed: {0}")]
    CommitFailed(#[source] sqlx::Error),
    /// No definitive commit result arrived; the server may have committed.
    /// Reconcile instead of retrying blindly.
    #[error("postgres transaction: commit outcome unknown: {0}")]
    CommitUnknown(#[source] sqlx::Error),
}

impl TxError {
    #[must_use]
    pub const fn driver(&self) -> &sqlx::Error {
        match self {
            Self::Acquire(err)
            | Self::Begin(err)
            | Self::CommitFailed(err)
            | Self::CommitUnknown(err) => err,
        }
    }
}

/// A statement's success proves the transaction is alive; an empty statement proves nothing.
#[derive(Debug)]
pub struct Tx<'c> {
    conn: &'c mut PgConnection,
    /// Whether the last statement succeeded, proving the transaction is not
    /// aborted. Cleared when a statement starts and by [`Tx::connection`].
    proven_alive: bool,
}

impl Tx<'_> {
    /// The borrowed connection, for savepoints or APIs that take a connection.
    ///
    /// The commit that follows first checks the transaction is not aborted, unless
    /// a later statement through `tx` succeeds. Do not issue transaction-control
    /// SQL through this connection.
    pub fn connection(&mut self) -> &mut PgConnection {
        self.proven_alive = false;
        self.conn
    }
}

/// `is_local = true` lasts until the transaction ends, so PostgreSQL restores
/// the session timeout itself.
pub async fn set_statement_timeout(tx: &mut Tx<'_>, timeout: Duration) -> Result<(), sqlx::Error> {
    observed(
        "set statement timeout",
        sqlx::query!(
            "SELECT set_config('statement_timeout', $1, true)",
            format_pg_duration(timeout)
        )
        .fetch_one(&mut *tx),
    )
    .await?;
    Ok(())
}

type DriverFuture<'e, T> = Pin<Box<dyn Future<Output = Result<T, sqlx::Error>> + Send + 'e>>;
type DriverStream<'e, T> = Pin<Box<dyn Stream<Item = Result<T, sqlx::Error>> + Send + 'e>>;

/// Each statement withdraws the proof when it starts and restores it only on a
/// complete, error-free answer.
impl<'c> Executor<'c> for &'c mut Tx<'_> {
    type Database = Postgres;

    fn fetch_many<'e, 'q: 'e, E>(self, query: E) -> DriverStream<'e, Either<PgQueryResult, PgRow>>
    where
        'c: 'e,
        E: 'q + Execute<'q, Postgres>,
    {
        self.proven_alive = false;
        Box::pin(Proving {
            statement: self.conn.fetch_many(query),
            proof_to_restore: Some(&mut self.proven_alive),
        })
    }

    fn fetch_optional<'e, 'q: 'e, E>(self, query: E) -> DriverFuture<'e, Option<PgRow>>
    where
        'c: 'e,
        E: 'q + Execute<'q, Postgres>,
    {
        self.proven_alive = false;
        let statement = self.conn.fetch_optional(query);
        let proven_alive = &mut self.proven_alive;
        Box::pin(async move {
            let row = statement.await?;
            *proven_alive = true;
            Ok(row)
        })
    }

    // A prepare or describe answered from the statement cache never reaches the
    // server, so it proves nothing.
    fn prepare_with<'e>(
        self,
        sql: SqlStr,
        parameters: &'e [PgTypeInfo],
    ) -> DriverFuture<'e, PgStatement>
    where
        'c: 'e,
    {
        self.proven_alive = false;
        self.conn.prepare_with(sql, parameters)
    }

    fn describe<'e>(self, sql: SqlStr) -> DriverFuture<'e, sqlx::Describe<Postgres>>
    where
        'c: 'e,
    {
        self.proven_alive = false;
        self.conn.describe(sql)
    }
}

struct Proving<'e, T> {
    statement: DriverStream<'e, T>,
    proof_to_restore: Option<&'e mut bool>,
}

impl<T> Stream for Proving<'_, T> {
    type Item = Result<T, sqlx::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let item = ready!(this.statement.as_mut().poll_next(cx));
        match &item {
            Some(Ok(_)) => {}
            Some(Err(_)) => this.proof_to_restore = None,
            None => {
                if let Some(proven_alive) = this.proof_to_restore.take() {
                    *proven_alive = true;
                }
            }
        }
        Poll::Ready(item)
    }
}

/// sqlx 0.9 does not roll back a `BEGIN` whose future is cancelled: its guard
/// acts only after the depth was incremented, which follows the server's answer.
/// The pool's return ping succeeds, so the connection would re-enter the pool
/// inside an open transaction.
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
    #[default]
    ServerDefault,
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

impl Isolation {
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxOptions {
    pub isolation: Isolation,
    pub read_only: bool,
}

impl TxOptions {
    /// Literals, so `begin_with` takes a static string instead of dynamic SQL
    /// behind `AssertSqlSafe`.
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

pub async fn in_tx<T, E, F>(pool: &PgPool, f: F) -> Result<T, E>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
    E: From<TxError>,
{
    in_tx_with(pool, TxOptions::default(), f).await
}

pub async fn in_tx_with<T, E, F>(pool: &PgPool, options: TxOptions, f: F) -> Result<T, E>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
    E: From<TxError>,
{
    transaction(pool, options, None, f).await
}

/// [`in_tx_with`] with `deadline` bounding only the wait for a connection, as
/// [`acquire_until`] describes.
pub async fn in_tx_with_acquire_deadline<T, E, F>(
    pool: &PgPool,
    deadline: tokio::time::Instant,
    options: TxOptions,
    f: F,
) -> Result<T, E>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
    E: From<TxError>,
{
    transaction(pool, options, Some(deadline), f).await
}

async fn transaction<T, E, F>(
    pool: &PgPool,
    options: TxOptions,
    deadline: Option<tokio::time::Instant>,
    f: F,
) -> Result<T, E>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
    E: From<TxError>,
{
    let span = observe::transaction_span(pool);
    let mut observed = TransactionObservation::start(span.clone());
    async {
        // Keeps the native pool wait out of callers' transaction futures; nested callers
        // otherwise exceed Clippy's size and Rust's hotpath layout-query depth limits.
        let acquired = Box::pin(async {
            match deadline {
                Some(deadline) => acquire_until(pool, "transaction", deadline).await,
                None => acquire(pool, "transaction").await,
            }
        })
        .await;
        observed.waited();
        let connection = acquired.map_err(|err| observed.fail(TxError::Acquire(err)))?;
        let mut guard = DiscardOnDrop {
            connection,
            discard: true,
        };
        let mut tx = match options.begin_statement() {
            None => guard.connection.begin().await,
            Some(statement) => guard.connection.begin_with(statement).await,
        }
        .map_err(|err| observed.fail(TxError::Begin(err)))?;
        guard.discard = false;

        let mut handle = Tx {
            conn: &mut tx,
            proven_alive: true,
        };
        // Dropping `tx` queues the rollback and the pool's return ping sends it; a
        // rejected rollback fails that ping and the pool closes the connection.
        let value = match f(&mut handle).await {
            Ok(value) => value,
            Err(err) => {
                observed.end(TransactionOutcome::RolledBack);
                return Err(err);
            }
        };
        // A read-only transaction wrote nothing a silent rollback could lose.
        if !(options.read_only || handle.proven_alive) {
            // PostgreSQL answers `COMMIT` in an aborted transaction with a silent
            // `ROLLBACK` and sqlx does not check the command tag (pgx reports it as
            // `ErrTxCommitRollback`), so this statement proves liveness: an aborted
            // transaction answers `25P02`. The commit is never sent after a failure here.
            (&mut *tx)
                .execute("SELECT 1")
                .await
                .map_err(|err| observed.fail(TxError::CommitFailed(err)))?;
        }
        tx.commit()
            .await
            .map_err(|err| observed.fail(classify_commit(err)))?;
        observed.end(TransactionOutcome::Committed);
        Ok(value)
    }
    .instrument(span)
    .await
}

/// A deferred constraint (class `23`) or transaction rollback (class `40`)
/// reported by `COMMIT` means the server rolled back, except `40003` (statement
/// completion unknown) and every transport failure.
fn classify_commit(err: sqlx::Error) -> TxError {
    let rejected = sqlstate(&err)
        .is_some_and(|code| code.starts_with("23") || (code.starts_with("40") && code != "40003"));
    if rejected {
        TxError::CommitFailed(err)
    } else {
        TxError::CommitUnknown(err)
    }
}

#[cfg(test)]
mod tests {
    use futures_util::{StreamExt, stream};

    use super::*;

    async fn proof_after(items: Vec<Result<u8, sqlx::Error>>, take: usize) -> bool {
        let mut proven_alive = false;
        let mut statement = Proving {
            statement: Box::pin(stream::iter(items)),
            proof_to_restore: Some(&mut proven_alive),
        };
        for _ in 0..take {
            let _ = statement.next().await;
        }
        drop(statement);
        proven_alive
    }

    #[tokio::test]
    async fn only_a_statement_that_ended_without_an_error_proves_the_transaction() {
        assert!(proof_after(vec![Ok(1), Ok(2)], 3).await);
        assert!(proof_after(vec![], 1).await);
        assert!(!proof_after(vec![Ok(1), Ok(2)], 2).await);
        let failed = || vec![Ok(1), Err(sqlx::Error::Protocol("failed".into()))];
        assert!(!proof_after(failed(), 2).await);
        assert!(!proof_after(failed(), 3).await);

        let mut proven_alive = false;
        let mut statement = Proving::<()> {
            statement: Box::pin(stream::pending()),
            proof_to_restore: Some(&mut proven_alive),
        };
        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(Pin::new(&mut statement).poll_next(&mut cx).is_pending());
        drop(statement);
        assert!(!proven_alive);
    }

    #[test]
    fn commit_classification_by_sqlstate() {
        for code in ["23505", "23503", "40001", "40P01"] {
            let err = classify_commit(crate::error::tests::database(code));
            std::assert_matches!(err, TxError::CommitFailed(_), "{code}");
        }
        for code in ["40003", "25P02", "57014", "08006", "XX000"] {
            let err = classify_commit(crate::error::tests::database(code));
            std::assert_matches!(err, TxError::CommitUnknown(_), "{code}");
        }
        let err = classify_commit(sqlx::Error::Io(std::io::Error::other("reset")));
        std::assert_matches!(err, TxError::CommitUnknown(_), "{err}");
    }
}
