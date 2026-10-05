//! The startup check and the bounded cleanup of expired records.

use std::time::Duration;

use infra_postgres::{CleanupPass, TxError, failure_cause, in_tx_with, observed, sqlstate};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::{READ_COMMITTED, Store};

/// Bound on the whole startup check, from the acquire to the result.
const STARTUP_CHECK_BUDGET: Duration = Duration::from_secs(5);

/// Cleanup cadence; the first run starts at once.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

/// The most rows one cleanup batch deletes.
const CLEANUP_BATCH_ROWS: u32 = 500;

/// Runs of the periodic cleanup task, by `outcome` (`completed`, `failed`).
pub const CLEANUP_RUNS_METRIC: &str = "http_idempotency_cleanup_runs_total";

/// Expired records the cleanup deleted, counted per committed batch.
pub const CLEANUP_REMOVED_METRIC: &str = "http_idempotency_cleanup_removed_records_total";

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
        match &err {
            TxError::Acquire(err) => cleanup_failed(err, Self::Acquire),
            TxError::Begin(err) => cleanup_failed(err, Self::Begin),
            TxError::CommitFailed(err) | TxError::CommitUnknown(err) => {
                cleanup_failed(err, Self::Commit)
            }
        }
    }
}

/// Log a failed cleanup batch with only bounded fields, never driver text,
/// and return its class.
fn cleanup_failed(err: &sqlx::Error, class: CleanupError) -> CleanupError {
    tracing::warn!(
        failure = %class,
        sqlstate = sqlstate(err).as_deref(),
        cause = failure_cause(err),
        "http_idempotency_cleanup_failed"
    );
    class
}

impl Store {
    /// Check that the current session is writable, bounded to 5 s.
    ///
    /// # Errors
    ///
    /// [`StartupError::NotWritable`] for a read-only or recovering session,
    /// and [`StartupError::Unavailable`] for anything else, including the
    /// bound; that one is logged with a bounded cause.
    pub async fn check_startup(&self) -> Result<(), StartupError> {
        // Migration-history admission owns schema compatibility; this check
        // keeps only the live writer property.
        let writable = observed("check idempotency session", async {
            let mut connection =
                infra_postgres::acquire(&self.pool, "check idempotency startup").await?;
            sqlx::query_scalar!(
                "SELECT NOT pg_is_in_recovery() \
                 AND current_setting('transaction_read_only') = 'off' AS \"writable!\""
            )
            .fetch_one(&mut *connection)
            .await
        });
        match tokio::time::timeout(STARTUP_CHECK_BUDGET, writable).await {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err(StartupError::NotWritable),
            Ok(Err(err)) => {
                tracing::warn!(
                    sqlstate = sqlstate(&err).as_deref(),
                    cause = failure_cause(&err),
                    "http_idempotency_startup_check_failed"
                );
                Err(StartupError::Unavailable)
            }
            Err(_) => {
                tracing::warn!(cause = "timeout", "http_idempotency_startup_check_failed");
                Err(StartupError::Unavailable)
            }
        }
    }

    /// Delete expired records in batches of at most 500 until a batch
    /// deletes fewer, and return how many were deleted. Each batch is its
    /// own `READ COMMITTED` transaction with a 1 s statement timeout, and
    /// skips records a running attempt holds.
    ///
    /// # Errors
    ///
    /// The failure class of the batch that failed, logged with a bounded
    /// cause; earlier batches stay committed.
    pub async fn remove_expired(&self) -> Result<u64, CleanupError> {
        let mut pass = CleanupPass::start("http_idempotency");
        let mut removed = 0;
        loop {
            let batch = in_tx_with(
                &self.pool,
                READ_COMMITTED,
                async |tx| -> Result<u64, CleanupError> {
                    // Bounds the batch on the server, so a batch whose client has
                    // gone still ends within 1 s.
                    observed(
                        "set statement timeout",
                        sqlx::query!("SET LOCAL statement_timeout = '1000ms'").execute(&mut *tx),
                    )
                    .await
                    .map_err(|err| cleanup_failed(&err, CleanupError::Statement))?;
                    // One batch of at most `$1` expired records. It skips rows a
                    // running attempt holds, and re-checks expiry, so it never
                    // deletes a live record. The tuple locator is consumed under
                    // its row lock within this statement.
                    let deleted = observed(
                        "delete expired idempotency records",
                        sqlx::query!(
                            "WITH batch AS ( \
                         SELECT ctid FROM http_idempotency_records \
                         WHERE expires_at <= statement_timestamp() \
                         ORDER BY expires_at LIMIT $1 FOR UPDATE SKIP LOCKED) \
                         DELETE FROM http_idempotency_records AS r USING batch \
                         WHERE r.ctid = batch.ctid AND r.expires_at <= statement_timestamp()",
                            i64::from(CLEANUP_BATCH_ROWS),
                        )
                        .execute(&mut *tx),
                    )
                    .await
                    .map_err(|err| cleanup_failed(&err, CleanupError::Statement))?;
                    Ok(deleted.rows_affected())
                },
            )
            .await
            .inspect_err(|_| pass.failed())?;
            pass.committed(batch);
            metrics::counter!(CLEANUP_REMOVED_METRIC).increment(batch);
            removed += batch;
            if batch < u64::from(CLEANUP_BATCH_ROWS) {
                pass.completed();
                return Ok(removed);
            }
        }
    }

    /// The periodic cleanup task body: one [`Store::remove_expired`] run
    /// every 60 s, the first at once. Every run counts its outcome; a failed
    /// run waits for the next tick and changes neither readiness nor serving.
    /// Returns when `cancel` fires, dropping a run in flight.
    pub async fn run_cleanup(self, cancel: CancellationToken) {
        metrics::describe_counter!(
            CLEANUP_RUNS_METRIC,
            metrics::Unit::Count,
            "Runs of the expired idempotency record cleanup, by outcome."
        );
        metrics::describe_counter!(
            CLEANUP_REMOVED_METRIC,
            metrics::Unit::Count,
            "Expired idempotency records the cleanup deleted."
        );
        // An already cancelled token never polls the loop.
        let _ = cancel
            .run_until_cancelled(async {
                let mut ticker = tokio::time::interval(CLEANUP_INTERVAL);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    let outcome = match self.remove_expired().await {
                        Ok(_) => "completed",
                        Err(_) => "failed",
                    };
                    metrics::counter!(CLEANUP_RUNS_METRIC, "outcome" => outcome).increment(1);
                }
            })
            .await;
    }
}
