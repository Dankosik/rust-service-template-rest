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

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use futures_util::Stream;
use sqlx::postgres::{
    PgConnection, PgPool, PgQueryResult, PgRow, PgStatement, PgTypeInfo, Postgres,
};
use sqlx::{Connection, Either, Execute, Executor, SqlStr};
use tracing::Instrument;

use crate::checkout::Checkout;
use crate::observe::{self, Observed, Outcome};
use crate::sqlstate;

#[derive(Debug, thiserror::Error)]
pub enum TxError {
    #[error("postgres transaction: acquire connection: {0}")]
    Acquire(#[source] sqlx::Error),
    #[error("postgres transaction: begin: {0}")]
    Begin(#[source] sqlx::Error),
    /// The server rejected the commit, or the transaction was already
    /// aborted by a failed statement the closure did not propagate and the
    /// commit was never sent; nothing was written.
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
/// `&mut Tx` is a `sqlx` executor, used like `&mut PgConnection`:
/// `sqlx::query(..).execute(&mut *tx)`. The transaction boundary retains
/// commit and rollback ownership. It reads a statement's success as proof
/// that the transaction is alive, which holds for SQL the server executes;
/// an empty statement proves nothing and has no place here.
#[derive(Debug)]
pub struct Tx<'c> {
    conn: &'c mut PgConnection,
    /// Whether the last statement run through this handle succeeded, which
    /// proves the transaction is not aborted. Cleared when a statement
    /// starts and by [`connection`]; a fresh transaction holds the proof.
    not_aborted: bool,
}

/// The connection borrowed by `tx`, for what the executor does not offer: a
/// savepoint (`connection(tx).begin()`) or an API that takes a connection.
///
/// The boundary cannot see what runs through it, so the commit that follows
/// first checks that the transaction is not aborted, unless a later
/// statement through `tx` succeeds. Do not issue transaction-control SQL
/// through this connection.
pub fn connection<'a>(tx: &'a mut Tx<'_>) -> &'a mut PgConnection {
    tx.not_aborted = false;
    tx.conn
}

type BoxFuture<'e, T> = Pin<Box<dyn Future<Output = Result<T, sqlx::Error>> + Send + 'e>>;
type BoxStream<'e, T> = Pin<Box<dyn Stream<Item = Result<T, sqlx::Error>> + Send + 'e>>;

/// Statements run on the borrowed connection; each one withdraws the proof
/// when it starts and restores it when the server answered it completely
/// and without an error. A dropped or failed statement leaves it withdrawn.
impl<'c> Executor<'c> for &'c mut Tx<'_> {
    type Database = Postgres;

    fn fetch_many<'e, 'q: 'e, E>(self, query: E) -> BoxStream<'e, Either<PgQueryResult, PgRow>>
    where
        'c: 'e,
        E: 'q + Execute<'q, Postgres>,
    {
        self.not_aborted = false;
        Box::pin(Proving {
            statement: self.conn.fetch_many(query),
            failed: false,
            not_aborted: &mut self.not_aborted,
        })
    }

    fn fetch_optional<'e, 'q: 'e, E>(self, query: E) -> BoxFuture<'e, Option<PgRow>>
    where
        'c: 'e,
        E: 'q + Execute<'q, Postgres>,
    {
        self.not_aborted = false;
        let statement = self.conn.fetch_optional(query);
        let not_aborted = &mut self.not_aborted;
        Box::pin(async move {
            let row = statement.await?;
            *not_aborted = true;
            Ok(row)
        })
    }

    // A prepare or describe the driver answers from its statement cache
    // never reaches the server, so neither one restores the proof.
    fn prepare_with<'e>(
        self,
        sql: SqlStr,
        parameters: &'e [PgTypeInfo],
    ) -> BoxFuture<'e, PgStatement>
    where
        'c: 'e,
    {
        self.not_aborted = false;
        self.conn.prepare_with(sql, parameters)
    }

    fn describe<'e>(self, sql: SqlStr) -> BoxFuture<'e, sqlx::Describe<Postgres>>
    where
        'c: 'e,
    {
        self.not_aborted = false;
        self.conn.describe(sql)
    }
}

/// A statement's result stream that restores the proof once the stream ends
/// without having yielded an error.
struct Proving<'e, T> {
    statement: BoxStream<'e, T>,
    failed: bool,
    not_aborted: &'e mut bool,
}

impl<T> Stream for Proving<'_, T> {
    type Item = Result<T, sqlx::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let item = ready!(this.statement.as_mut().poll_next(cx));
        match &item {
            Some(Ok(_)) => {}
            Some(Err(_)) => this.failed = true,
            // The stream also ends after an error it yielded.
            None => *this.not_aborted = !this.failed,
        }
        Poll::Ready(item)
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
    let span = observe::transaction_span(pool);
    let mut observed = Observed::start(span.clone());
    async {
        let acquired = Checkout::acquire(pool).await;
        observed.waited();
        let mut checkout = acquired.map_err(|err| observed.fail(TxError::Acquire(err)))?;
        let mut tx = match options.begin_statement() {
            None => checkout.connection().begin().await,
            Some(statement) => checkout.connection().begin_with(statement).await,
        }
        .map_err(|err| observed.fail(TxError::Begin(err)))?;

        let mut handle = Tx {
            conn: &mut tx,
            not_aborted: true,
        };
        // Dropping `tx` queues rollback before bounded return flushes it. A
        // cancelled callback instead drops the checkout and releases capacity
        // immediately, without claiming the server confirmed rollback.
        let value = match f(&mut handle).await {
            Ok(value) => value,
            Err(err) => {
                observed.end(Outcome::RolledBack);
                drop(tx);
                checkout.release().await;
                return Err(err);
            }
        };
        if !(options.read_only || handle.not_aborted) {
            // PostgreSQL answers `COMMIT` in an aborted transaction with a
            // silent `ROLLBACK`, and sqlx does not check the command tag (pgx
            // reports it as `ErrTxCommitRollback`). The closure's last
            // statement did not prove the transaction alive, so this one
            // does: an aborted transaction answers `25P02`, and a closure
            // that swallowed a failed statement does not look committed.
            // The commit is not sent after any failure here, so nothing was
            // written whatever the failure was.
            if let Err(err) = (&mut *tx).execute("SELECT 1").await {
                let failure = observed.fail(TxError::CommitFailed(err));
                drop(tx);
                checkout.release().await;
                return Err(failure.into());
            }
        }
        if let Err(err) = tx.commit().await {
            let failure = observed.fail(classify_commit(err));
            if matches!(failure, TxError::CommitFailed(_)) {
                checkout.release().await;
            }
            // An unresolved commit reply never returns to the pool.
            return Err(failure.into());
        }
        observed.end(Outcome::Committed);
        checkout.release().await;
        Ok(value)
    }
    .instrument(span)
    .await
}

/// Preserve failures known to have rejected the commit and mark every other
/// commit response as an unknown durable outcome.
///
/// A deferred constraint (class `23`) or a transaction rollback (class `40`)
/// reported by `COMMIT` means the server rolled back. `40003` (statement
/// completion unknown) is the exception, as is every transport failure.
/// `25P02` is kept as rejected for a caller that classifies a statement
/// error of its own; the boundary's check ahead of `COMMIT` reports it
/// without sending the commit.
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
    use futures_util::{StreamExt, stream};

    use super::*;

    async fn proof_after(items: Vec<Result<u8, sqlx::Error>>, take: usize) -> bool {
        let mut not_aborted = false;
        let mut statement = Proving {
            statement: Box::pin(stream::iter(items)),
            failed: false,
            not_aborted: &mut not_aborted,
        };
        for _ in 0..take {
            let _ = statement.next().await;
        }
        drop(statement);
        not_aborted
    }

    #[tokio::test]
    async fn only_a_statement_that_ended_without_an_error_proves_the_transaction() {
        // Two rows and the end of the stream.
        assert!(proof_after(vec![Ok(1), Ok(2)], 3).await);
        // A statement with no rows still has to reach its end.
        assert!(proof_after(vec![], 1).await);
        // Dropped before the end: the server's answer was not read.
        assert!(!proof_after(vec![Ok(1), Ok(2)], 2).await);
        // The end that follows an error is not a completed statement.
        let failed = || vec![Ok(1), Err(sqlx::Error::Protocol("failed".into()))];
        assert!(!proof_after(failed(), 2).await);
        assert!(!proof_after(failed(), 3).await);
    }

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
