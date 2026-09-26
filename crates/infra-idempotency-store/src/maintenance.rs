//! The startup check and the bounded cleanup of expired records.

use std::time::Duration;

use infra_postgres::{TxError, connection, in_tx, in_tx_with};
use sqlx::Row;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::{READ_COMMITTED, Store};

/// Bound on the whole startup check, from the acquire to the transaction's
/// end.
const STARTUP_CHECK_BUDGET: Duration = Duration::from_secs(5);

/// Cleanup cadence; the first run starts at once.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

/// The most rows one cleanup batch deletes: the `LIMIT` in [`CLEANUP_BATCH`].
const CLEANUP_BATCH_ROWS: u64 = 500;

/// Whether the current session can write. Migration-history admission owns
/// schema compatibility; this check keeps only the live writer property.
const STARTUP_CHECK: &str = "SELECT NOT pg_is_in_recovery() \
    AND current_setting('transaction_read_only') = 'off' AS writable";

/// Bounds a cleanup batch on the server, so a batch whose client has gone
/// still ends within 1 s.
const CLEANUP_STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '1000ms'";

/// One batch of expired records. It skips rows a running attempt holds, and
/// re-checks expiry, so it never deletes a live record.
const CLEANUP_BATCH: &str = "DELETE FROM http_idempotency_records WHERE scope_key IN \
    (SELECT scope_key FROM http_idempotency_records \
    WHERE expires_at <= statement_timestamp() \
    ORDER BY expires_at LIMIT 500 FOR UPDATE SKIP LOCKED) \
    AND expires_at <= statement_timestamp()";

/// Why an active idempotency boundary cannot start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartupError {
    /// The session is read-only or recovering.
    #[error("the PostgreSQL session is not writable")]
    NotWritable,
    /// Anything else, including the check's bound.
    #[error("the idempotency store is unavailable")]
    Unavailable,
}

/// The failure class of one cleanup run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CleanupError {
    #[error("acquire")]
    Acquire,
    #[error("begin")]
    Begin,
    #[error("statement")]
    Statement,
    #[error("commit")]
    Commit,
}

impl Store {
    /// Check that the current session is writable, bounded to 5 s.
    ///
    /// # Errors
    ///
    /// [`StartupError::NotWritable`] for a read-only or recovering session,
    /// and [`StartupError::Unavailable`] for anything else, including the
    /// bound and an inert store.
    pub async fn check_startup(&self) -> Result<(), StartupError> {
        let Some(inner) = &self.inner else {
            return Err(StartupError::Unavailable);
        };
        let check = in_tx_with(
            &inner.pool,
            READ_COMMITTED,
            async |tx| -> Result<(), Refused> {
                let row = sqlx::query(STARTUP_CHECK)
                    .fetch_one(connection(tx))
                    .await
                    .map_err(|_| Refused(StartupError::Unavailable))?;
                match row.try_get::<bool, _>("writable") {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(Refused(StartupError::NotWritable)),
                    Err(_) => Err(Refused(StartupError::Unavailable)),
                }
            },
        );
        match tokio::time::timeout(STARTUP_CHECK_BUDGET, check).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(Refused(refusal))) => Err(refusal),
            Err(_elapsed) => Err(StartupError::Unavailable),
        }
    }

    /// Delete expired records in batches of at most 500 until a batch
    /// deletes fewer, and return how many were deleted. Each batch is its
    /// own transaction with a 1 s statement timeout, and skips records a
    /// running attempt holds.
    ///
    /// # Errors
    ///
    /// The failure class of the batch that failed; earlier batches stay
    /// committed. An inert store fails as [`CleanupError::Acquire`].
    pub async fn remove_expired(&self) -> Result<u64, CleanupError> {
        let Some(inner) = &self.inner else {
            return Err(CleanupError::Acquire);
        };
        let mut removed = 0;
        loop {
            let batch = in_tx(&inner.pool, async |tx| -> Result<u64, Failed> {
                sqlx::query(CLEANUP_STATEMENT_TIMEOUT)
                    .execute(connection(tx))
                    .await
                    .map_err(|_| Failed(CleanupError::Statement))?;
                let deleted = sqlx::query(CLEANUP_BATCH)
                    .execute(connection(tx))
                    .await
                    .map_err(|_| Failed(CleanupError::Statement))?;
                Ok(deleted.rows_affected())
            })
            .await
            .map_err(|Failed(failure)| failure)?;
            removed += batch;
            if batch < CLEANUP_BATCH_ROWS {
                return Ok(removed);
            }
        }
    }

    /// The periodic cleanup task body: one [`Store::remove_expired`] run
    /// every 60 s, the first at once. A failed run logs its class and waits
    /// for the next tick; it changes neither readiness nor serving. Returns
    /// when `cancel` fires, dropping a run in flight, and at once for an
    /// inert store.
    pub async fn run_cleanup(self, cancel: CancellationToken) {
        if self.inner.is_none() {
            return;
        }
        // An already cancelled token never polls the loop.
        let _ = cancel
            .run_until_cancelled(async {
                let mut ticker = tokio::time::interval(CLEANUP_INTERVAL);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    if let Err(failure) = self.remove_expired().await {
                        tracing::warn!(failure = %failure, "http_idempotency_cleanup_failed");
                    }
                }
            })
            .await;
    }
}

/// A startup refusal, which leaves the check's transaction as an error. A
/// transaction error refuses as [`StartupError::Unavailable`].
struct Refused(StartupError);

impl From<TxError> for Refused {
    fn from(_: TxError) -> Self {
        Self(StartupError::Unavailable)
    }
}

/// A failed cleanup batch, by class, which leaves the batch's transaction as
/// an error.
struct Failed(CleanupError);

impl From<TxError> for Failed {
    fn from(err: TxError) -> Self {
        Self(match err {
            TxError::Acquire(_) => CleanupError::Acquire,
            TxError::Begin(_) => CleanupError::Begin,
            TxError::CommitFailed(_) | TxError::CommitUnknown(_) => CleanupError::Commit,
        })
    }
}
