//! The startup check and the bounded cleanup of expired records.

use std::time::Duration;

use infra_postgres::{TxError, in_tx};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::Store;

/// Bound on the whole startup check, from the acquire to the result.
const STARTUP_CHECK_BUDGET: Duration = Duration::from_secs(5);

/// Cleanup cadence; the first run starts at once.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

/// The most rows one cleanup batch deletes.
const CLEANUP_BATCH_ROWS: u32 = 500;

/// Whether the current session can write. Migration-history admission owns
/// schema compatibility; this check keeps only the live writer property.
const STARTUP_CHECK: &str = "SELECT NOT pg_is_in_recovery() \
    AND current_setting('transaction_read_only') = 'off'";

/// Bounds a cleanup batch on the server, so a batch whose client has gone
/// still ends within 1 s.
const CLEANUP_STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '1000ms'";

/// One batch of at most `$1` expired records. It skips rows a running attempt
/// holds, and re-checks expiry, so it never deletes a live record. The tuple
/// locator is consumed under its row lock within this statement.
const CLEANUP_BATCH: &str = "WITH batch AS ( \
    SELECT ctid FROM http_idempotency_records \
    WHERE expires_at <= statement_timestamp() \
    ORDER BY expires_at LIMIT $1 FOR UPDATE SKIP LOCKED) \
    DELETE FROM http_idempotency_records AS r USING batch \
    WHERE r.ctid = batch.ctid AND r.expires_at <= statement_timestamp()";

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

impl From<TxError> for CleanupError {
    fn from(err: TxError) -> Self {
        match err {
            TxError::Acquire(_) => Self::Acquire,
            TxError::Begin(_) => Self::Begin,
            TxError::CommitFailed(_) | TxError::CommitUnknown(_) => Self::Commit,
        }
    }
}

impl Store {
    /// Check that the current session is writable, bounded to 5 s.
    ///
    /// # Errors
    ///
    /// [`StartupError::NotWritable`] for a read-only or recovering session,
    /// and [`StartupError::Unavailable`] for anything else, including the
    /// bound.
    pub async fn check_startup(&self) -> Result<(), StartupError> {
        let writable = sqlx::query_scalar::<_, bool>(STARTUP_CHECK).fetch_one(&self.pool);
        match tokio::time::timeout(STARTUP_CHECK_BUDGET, writable).await {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err(StartupError::NotWritable),
            Ok(Err(_)) | Err(_) => Err(StartupError::Unavailable),
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
    /// committed.
    pub async fn remove_expired(&self) -> Result<u64, CleanupError> {
        let mut removed = 0;
        loop {
            let batch = in_tx(&self.pool, async |tx| -> Result<u64, CleanupError> {
                sqlx::query(CLEANUP_STATEMENT_TIMEOUT)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| CleanupError::Statement)?;
                let deleted = sqlx::query(CLEANUP_BATCH)
                    .bind(i64::from(CLEANUP_BATCH_ROWS))
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| CleanupError::Statement)?;
                Ok(deleted.rows_affected())
            })
            .await?;
            removed += batch;
            if batch < u64::from(CLEANUP_BATCH_ROWS) {
                return Ok(removed);
            }
        }
    }

    /// The periodic cleanup task body: one [`Store::remove_expired`] run
    /// every 60 s, the first at once. A failed run logs its class and waits
    /// for the next tick; it changes neither readiness nor serving. Returns
    /// when `cancel` fires, dropping a run in flight.
    pub async fn run_cleanup(self, cancel: CancellationToken) {
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
