use std::time::Duration;

use metrics::Unit;
use sqlx::postgres::PgPool;
use tokio_util::sync::CancellationToken;

use super::LOG_TARGET;
use crate::error::{failure_cause, sqlstate};
use crate::transaction::{Tx, TxError, TxOptions, in_tx_with, set_statement_timeout};

/// The terminal duration of a cleanup pass, including every admitted wait.
pub(crate) const CLEANUP_DURATION_METRIC: &str = "postgres_cleanup_pass_duration_seconds";

/// Buckets in seconds for [`CLEANUP_DURATION_METRIC`]. They reach 3600 s so
/// long passes, which have no deadline, stay visible.
pub(crate) const CLEANUP_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
    600.0, 1200.0, 1800.0, 3600.0,
];

/// Matches the jobs retention backstop, with room for pool acquisition, the
/// cleanup statement, and transaction acknowledgement.
const CLEANUP_BATCH_BUDGET: Duration = Duration::from_secs(12);
const CLEANUP_STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Every `cleanup` label of the cleanup metrics: the populations a cleanup
/// pass deletes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cleanup {
    Jobs,
    HttpIdempotency,
    WebhookReceipts,
}

impl Cleanup {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Jobs => "jobs",
            Self::HttpIdempotency => "http_idempotency",
            Self::WebhookReceipts => "webhook_receipts",
        }
    }
}

/// How a cleanup pass that did not fail ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassEnd {
    Completed,
    /// The pass saw the owner's cancellation between batches.
    Cancelled,
}

const CLEANUP_ACTIVE_METRIC: &str = "postgres_cleanup_active_passes";
const CLEANUP_BATCHES_METRIC: &str = "postgres_cleanup_committed_batches_total";
pub(super) const CLEANUP_REMOVED_ROWS_METRIC: &str = "postgres_cleanup_removed_rows_total";
const CLEANUP_PASSES_METRIC: &str = "postgres_cleanup_passes_total";

/// Every `outcome` label of a cleanup pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanupOutcome {
    Completed,
    Failed,
    Cancelled,
}

impl CleanupOutcome {
    const fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

fn describe_cleanup() {
    metrics::describe_gauge!(
        CLEANUP_ACTIVE_METRIC,
        Unit::Count,
        "Started PostgreSQL cleanup passes that have not terminated in this process"
    );
    metrics::describe_counter!(
        CLEANUP_BATCHES_METRIC,
        Unit::Count,
        "Cleanup batches confirmed committed, including empty terminal batches"
    );
    metrics::describe_counter!(
        CLEANUP_REMOVED_ROWS_METRIC,
        Unit::Count,
        "Deleted rows confirmed committed by PostgreSQL cleanup"
    );
    metrics::describe_counter!(
        CLEANUP_PASSES_METRIC,
        Unit::Count,
        "Terminated PostgreSQL cleanup passes by outcome"
    );
    metrics::describe_histogram!(
        CLEANUP_DURATION_METRIC,
        Unit::Seconds,
        "Whole PostgreSQL cleanup pass duration including admission and database waits"
    );
}

/// Whole-pass cleanup evidence, including direct and concurrent callers.
///
/// Start inside the polled pass before its first wait. Record a batch only
/// after its transaction confirms commit, including an empty terminal batch.
/// Mark the terminal disposition completed or failed; dropping an unfinished
/// pass records cancellation and preserves its earlier confirmed progress.
#[derive(Debug)]
pub struct CleanupPass {
    cleanup: Cleanup,
    started: tokio::time::Instant,
    outcome: CleanupOutcome,
    active: metrics::Gauge,
    batches: metrics::Counter,
    rows: metrics::Counter,
    committed_batches: u64,
    removed_rows: u64,
}

/// The failure class of a cleanup batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CleanupError {
    #[error("acquire")]
    Acquire,
    #[error("begin")]
    Begin,
    #[error("statement")]
    Statement,
    /// The commit failed or its outcome is unknown.
    #[error("commit")]
    Commit,
    /// The batch missed its client-side deadline and may have committed.
    #[error("cleanup batch timed out")]
    TimedOut,
}

struct BatchError {
    class: CleanupError,
    driver: sqlx::Error,
}

impl From<TxError> for BatchError {
    fn from(error: TxError) -> Self {
        let (class, driver) = match error {
            TxError::Acquire(driver) => (CleanupError::Acquire, driver),
            TxError::Begin(driver) => (CleanupError::Begin, driver),
            TxError::CommitFailed(driver) | TxError::CommitUnknown(driver) => {
                (CleanupError::Commit, driver)
            }
        };
        Self { class, driver }
    }
}

impl From<sqlx::Error> for BatchError {
    fn from(driver: sqlx::Error) -> Self {
        Self {
            class: CleanupError::Statement,
            driver,
        }
    }
}

impl CleanupPass {
    #[must_use]
    pub fn start(cleanup: Cleanup) -> Self {
        let started = tokio::time::Instant::now();
        describe_cleanup();
        let label = cleanup.label();
        let active = metrics::gauge!(CLEANUP_ACTIVE_METRIC, "cleanup" => label);
        active.increment(1.0);
        Self {
            cleanup,
            started,
            outcome: CleanupOutcome::Cancelled,
            active,
            batches: metrics::counter!(CLEANUP_BATCHES_METRIC, "cleanup" => label),
            rows: metrics::counter!(CLEANUP_REMOVED_ROWS_METRIC, "cleanup" => label),
            committed_batches: 0,
            removed_rows: 0,
        }
    }

    /// Pass `batch_limit` to each delete and stop when it removes fewer rows
    /// or the owner cancels between batches. Count only acknowledged commits.
    /// A short batch does not establish that the eligible population is empty.
    ///
    /// # Errors
    ///
    /// The failed batch's error. Earlier confirmed batches stay committed.
    pub async fn run_until_short_batch<F>(
        mut self,
        pool: &PgPool,
        options: TxOptions,
        cancel: Option<&CancellationToken>,
        batch_limit: u32,
        removed_metric: &'static str,
        delete: F,
    ) -> Result<(u64, PassEnd), CleanupError>
    where
        F: Copy + AsyncFnOnce(&mut Tx<'_>, u32) -> Result<u64, sqlx::Error>,
    {
        let mut removed = 0;
        loop {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Ok((removed, PassEnd::Cancelled));
            }
            let rows = self.run_batch(pool, options, batch_limit, delete).await?;
            metrics::counter!(removed_metric).increment(rows);
            removed += rows;
            if rows < u64::from(batch_limit) {
                self.completed();
                return Ok((removed, PassEnd::Completed));
            }
        }
    }

    /// Execute one cleanup transaction with a server statement timeout and
    /// a client deadline from acquisition through commit acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns and logs the batch's failure class.
    async fn run_batch<F>(
        &mut self,
        pool: &PgPool,
        options: TxOptions,
        batch_limit: u32,
        delete: F,
    ) -> Result<u64, CleanupError>
    where
        F: AsyncFnOnce(&mut Tx<'_>, u32) -> Result<u64, sqlx::Error>,
    {
        self.await_batch(in_tx_with(
            pool,
            options,
            async |tx| -> Result<u64, BatchError> {
                // The server still bounds its statements if the client stops waiting.
                set_statement_timeout(tx, CLEANUP_STATEMENT_TIMEOUT).await?;
                delete(tx, batch_limit).await.map_err(BatchError::from)
            },
        ))
        .await
    }

    async fn await_batch(
        &mut self,
        batch: impl Future<Output = Result<u64, BatchError>>,
    ) -> Result<u64, CleanupError> {
        let failure = match tokio::time::timeout(CLEANUP_BATCH_BUDGET, batch).await {
            Ok(Ok(rows)) => {
                self.committed(rows);
                return Ok(rows);
            }
            Ok(Err(BatchError { class, driver })) => {
                let message = match self.cleanup {
                    Cleanup::HttpIdempotency => "http_idempotency_cleanup_failed",
                    Cleanup::WebhookReceipts => "webhook_receipt_cleanup_failed",
                    Cleanup::Jobs => "postgres_cleanup_batch_failed",
                };
                tracing::warn!(
                    target: LOG_TARGET,
                    cleanup = self.cleanup.label(),
                    failure = %class,
                    sqlstate = sqlstate(&driver).as_deref(),
                    cause = failure_cause(&driver),
                    "{message}"
                );
                class
            }
            Err(_) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    cleanup = self.cleanup.label(),
                    "postgres_cleanup_batch_timed_out"
                );
                CleanupError::TimedOut
            }
        };
        self.failed();
        Err(failure)
    }

    /// Publish a known successful commit before the next wait in the pass.
    pub fn committed(&mut self, rows: u64) {
        self.batches.increment(1);
        self.rows.increment(rows);
        self.committed_batches = self.committed_batches.saturating_add(1);
        self.removed_rows = self.removed_rows.saturating_add(rows);
    }

    /// The pass is returning successfully after a short batch.
    pub fn completed(&mut self) {
        self.outcome = CleanupOutcome::Completed;
    }

    /// The pass is returning its original error, including unknown commit.
    pub fn failed(&mut self) {
        self.outcome = CleanupOutcome::Failed;
    }
}

impl Drop for CleanupPass {
    fn drop(&mut self) {
        let elapsed_seconds = self.started.elapsed().as_secs_f64();
        let cleanup = self.cleanup.label();
        let outcome = self.outcome.label();
        self.active.decrement(1.0);
        metrics::counter!(
            CLEANUP_PASSES_METRIC,
            "cleanup" => cleanup,
            "outcome" => outcome
        )
        .increment(1);
        metrics::histogram!(
            CLEANUP_DURATION_METRIC,
            "cleanup" => cleanup,
            "outcome" => outcome
        )
        .record(elapsed_seconds);
        tracing::info!(
            target: LOG_TARGET,
            cleanup,
            outcome,
            elapsed_seconds,
            committed_batches = self.committed_batches,
            removed_rows = self.removed_rows,
            "postgres_cleanup_pass_finished"
        );
    }
}

#[cfg(test)]
mod tests;
