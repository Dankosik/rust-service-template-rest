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

use std::cell::Cell;
use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnection, PgPool};
use sqlx::{Connection, Postgres, Transaction};

use crate::{commit_definitely_failed, failure_cause, raw_sqlstate, sqlstate};

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

/// An opaque capability for work inside a provider-owned transaction.
///
/// Only [`connection`] exposes the borrowed connection to provider adapters.
/// The transaction boundary retains commit and rollback ownership.
#[derive(Debug)]
pub struct Tx<'c> {
    conn: &'c mut PgConnection,
}

/// A connection that must not return to the pool while a `BEGIN` future is
/// pending. sqlx cannot know whether a cancelled `BEGIN` reached PostgreSQL,
/// so cancellation discards this physical connection. Once a transaction is
/// returned, sqlx owns its existing drop/rollback behavior again.
struct PendingBegin {
    connection: PoolConnection<Postgres>,
    armed: Cell<bool>,
}

impl PendingBegin {
    fn new(connection: PoolConnection<Postgres>) -> Self {
        Self {
            connection,
            armed: Cell::new(true),
        }
    }
}

impl Drop for PendingBegin {
    fn drop(&mut self) {
        if self.armed.get() {
            self.connection.close_on_drop();
        }
    }
}

/// The connection borrowed by `tx` for a provider adapter's statement.
///
/// Do not issue transaction-control SQL through this connection.
pub fn connection<'a>(tx: &'a mut Tx<'_>) -> &'a mut PgConnection {
    tx.conn
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

/// How the transaction is opened. [`Isolation::ServerDefault`] omits the clause.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxOptions {
    pub isolation: Isolation,
    pub read_only: bool,
}

impl TxOptions {
    /// The `BEGIN` statement; `None` when the server default applies.
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
    let mut pending = PendingBegin::new(pool.acquire().await.map_err(TxError::Acquire)?);
    let mut tx: Transaction<'_, Postgres> = match options.begin_statement() {
        None => pending.connection.begin().await,
        Some(statement) => pending.connection.begin_with(statement).await,
    }
    .map_err(TxError::Begin)?;
    pending.armed.set(false);

    let result = {
        let mut handle = Tx { conn: &mut tx };
        f(&mut handle).await
    };
    match result {
        Ok(value) => {
            tx.commit().await.map_err(classify_commit)?;
            Ok(value)
        }
        Err(err) => {
            match tokio::time::timeout(ROLLBACK_TIMEOUT, tx.rollback()).await {
                Ok(Ok(())) => {}
                Ok(Err(rollback)) => log_rollback_failure(&rollback),
                Err(_) => tracing::warn!(
                    event = "postgres_transaction_failure",
                    phase = "rollback",
                    failure_class = "rollback_timeout",
                    cause = "deadline"
                ),
            }
            Err(err)
        }
    }
}

fn log_rollback_failure(err: &sqlx::Error) {
    let code = sqlstate(err);
    if let Some(code) = code {
        tracing::warn!(
            event = "postgres_transaction_failure",
            phase = "rollback",
            failure_class = "rollback_failed",
            sqlstate = code.as_ref()
        );
    } else {
        let cause = failure_cause(err);
        tracing::warn!(
            event = "postgres_transaction_failure",
            phase = "rollback",
            failure_class = "rollback_failed",
            cause
        );
    }
}

/// Preserve failures known to have rejected the commit and mark every other
/// commit response as an unknown durable outcome.
fn classify_commit(err: sqlx::Error) -> TxError {
    if raw_sqlstate(&err)
        .as_deref()
        .is_some_and(commit_definitely_failed)
    {
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
        assert!(!crate::retryable(&sqlx::Error::PoolTimedOut));
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
            log_rollback_failure(&sqlx::Error::Protocol("sensitive bound value".to_owned()));
        });
        assert!(emitted.load(Ordering::Relaxed));
    }
}
