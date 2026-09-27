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

use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnection, PgPool};
use sqlx::{Connection, Executor, Postgres};

use crate::{failure_cause, sqlstate};

/// Bound on the rollback issued after the closure failed. A rollback that
/// fails or overruns this budget discards the physical connection instead
/// of returning it to the pool in an unknown state.
const ROLLBACK_TIMEOUT: Duration = Duration::from_secs(3);

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
}

/// The connection borrowed by `tx` for a provider adapter's statement.
///
/// Do not issue transaction-control SQL through this connection.
pub fn connection<'a>(tx: &'a mut Tx<'_>) -> &'a mut PgConnection {
    tx.conn
}

/// A pooled connection that is closed instead of returned to the pool
/// while `discard` is set.
///
/// sqlx 0.9 does not roll back a `BEGIN` whose future is cancelled: its
/// guard only acts once the transaction depth was incremented, which happens
/// after the server answered. The pool's return ping succeeds, so the
/// connection would re-enter the pool inside an open transaction. The same
/// holds for a rollback that failed or overran [`ROLLBACK_TIMEOUT`].
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
/// The closure's error after a bounded rollback, or a [`TxError`] converted
/// into it.
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
/// The closure's error after a bounded rollback, or a [`TxError`] converted
/// into it.
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

    let result = f(&mut Tx { conn: &mut tx }).await;
    match result {
        Ok(value) => {
            if !options.read_only {
                // PostgreSQL answers `COMMIT` in an aborted transaction with
                // a silent `ROLLBACK`, and sqlx does not check the command
                // tag (pgx reports it as `ErrTxCommitRollback`). One
                // statement first turns that state into an error: a closure
                // that swallowed a failed statement must not look committed.
                (&mut *tx)
                    .execute("SELECT 1")
                    .await
                    .map_err(TxError::CommitFailed)?;
            }
            tx.commit().await.map_err(classify_commit)?;
            Ok(value)
        }
        Err(err) => {
            let rolled_back = match tokio::time::timeout(ROLLBACK_TIMEOUT, tx.rollback()).await {
                Ok(Ok(())) => true,
                Ok(Err(rollback)) => {
                    log_rollback_failure("rollback_failed", Some(&rollback));
                    false
                }
                Err(_elapsed) => {
                    log_rollback_failure("rollback_timeout", None);
                    false
                }
            };
            guard.discard = !rolled_back;
            Err(err)
        }
    }
}

fn log_rollback_failure(failure_class: &'static str, err: Option<&sqlx::Error>) {
    let code = err.and_then(sqlstate);
    let cause = err.map_or("deadline", failure_cause);
    tracing::warn!(
        event = "postgres_transaction_failure",
        phase = "rollback",
        failure_class,
        sqlstate = code.as_deref(),
        cause
    );
}

/// Preserve failures known to have rejected the commit and mark every other
/// commit response as an unknown durable outcome.
///
/// A deferred constraint (class `23`) or a transaction rollback (class `40`)
/// reported by `COMMIT` means the server rolled back. `40003` (statement
/// completion unknown) is the exception, as is every transport failure.
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
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

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
        for code in ["23505", "23503", "40001", "40P01"] {
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

    struct RollbackDiagnostic(Arc<AtomicBool>);

    impl tracing::Subscriber for RollbackDiagnostic {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut fields = BTreeMap::new();
            event.record(
                &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                    fields.insert(field.name(), format!("{value:?}"));
                },
            );
            assert_eq!(
                fields,
                BTreeMap::from([
                    ("event", "\"postgres_transaction_failure\"".to_owned()),
                    ("phase", "\"rollback\"".to_owned()),
                    ("failure_class", "\"rollback_failed\"".to_owned()),
                    ("cause", "\"protocol\"".to_owned()),
                ])
            );
            self.0.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn rollback_failure_diagnostic_does_not_render_driver_text() {
        let emitted = Arc::new(AtomicBool::new(false));
        tracing::subscriber::with_default(RollbackDiagnostic(Arc::clone(&emitted)), || {
            log_rollback_failure(
                "rollback_failed",
                Some(&sqlx::Error::Protocol("sensitive bound value".to_owned())),
            );
        });
        assert!(emitted.load(Ordering::Relaxed));
    }
}
