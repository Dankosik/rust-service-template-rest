use std::fmt;
use std::num::NonZeroU32;
use std::time::Duration;

use metrics::Unit;
use tokio_util::sync::CancellationToken;

use super::{LOG_TARGET, is_valid_label};
use crate::error::{failure_cause, sqlstate};
use crate::pool::Pool;
use crate::transaction::{Tx, TxError, TxOptions};

pub(crate) const CLEANUP_DURATION_METRIC: &str = "postgres_cleanup_pass_duration_seconds";

pub(crate) const CLEANUP_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
    600.0, 1200.0, 1800.0, 3600.0,
];

const CLEANUP_BATCH_BUDGET: Duration = Duration::from_secs(12);
const CLEANUP_STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CleanupName(&'static str);

impl CleanupName {
    /// ```compile_fail
    /// const _: infra_postgres::maintenance::CleanupName =
    ///     infra_postgres::maintenance::CleanupName::new("Not-Valid");
    /// ```
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        assert!(is_valid_label(name), "invalid cleanup name");
        Self(name)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::borrow::Borrow<str> for CleanupName {
    fn borrow(&self) -> &str {
        self.0
    }
}

impl AsRef<str> for CleanupName {
    fn as_ref(&self) -> &str {
        self.0
    }
}

impl fmt::Display for CleanupName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassEnd {
    Completed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CleanupPassResult {
    pub removed_rows: u64,
    pub end: PassEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CleanupBatchOptions {
    pub transaction: TxOptions,
    pub limit: NonZeroU32,
    pub removed_metric: Option<&'static str>,
    pub failure_message: &'static str,
}

const CLEANUP_ACTIVE_METRIC: &str = "postgres_cleanup_active_passes";
const CLEANUP_BATCHES_METRIC: &str = "postgres_cleanup_committed_batches_total";
pub(super) const CLEANUP_REMOVED_ROWS_METRIC: &str = "postgres_cleanup_removed_rows_total";
const CLEANUP_PASSES_METRIC: &str = "postgres_cleanup_passes_total";

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

#[derive(Debug)]
pub struct CleanupPass {
    cleanup: CleanupName,
    started: tokio::time::Instant,
    outcome: CleanupOutcome,
    active: metrics::Gauge,
    batches: metrics::Counter,
    rows: metrics::Counter,
    committed_batches: u64,
    removed_rows: u64,
}

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
    pub fn start(cleanup: CleanupName) -> Self {
        let started = tokio::time::Instant::now();
        describe_cleanup();
        let label = cleanup.as_str();
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

    pub async fn run_until_short_batch<F>(
        mut self,
        pool: &Pool,
        cancel: Option<&CancellationToken>,
        options: &CleanupBatchOptions,
        mut delete: F,
    ) -> Result<CleanupPassResult, CleanupError>
    where
        F: AsyncFnMut(&mut Tx<'_>, NonZeroU32) -> Result<u64, sqlx::Error>,
    {
        loop {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Ok(CleanupPassResult {
                    removed_rows: self.removed_rows,
                    end: PassEnd::Cancelled,
                });
            }
            let rows = self.run_batch(pool, options, &mut delete).await?;
            if let Some(metric) = options.removed_metric {
                metrics::counter!(metric).increment(rows);
            }
            if rows < u64::from(options.limit.get()) {
                let removed_rows = self.complete();
                return Ok(CleanupPassResult {
                    removed_rows,
                    end: PassEnd::Completed,
                });
            }
        }
    }

    async fn run_batch<F>(
        &mut self,
        pool: &Pool,
        options: &CleanupBatchOptions,
        delete: &mut F,
    ) -> Result<u64, CleanupError>
    where
        F: AsyncFnMut(&mut Tx<'_>, NonZeroU32) -> Result<u64, sqlx::Error>,
    {
        let limit = options.limit;
        self.await_batch(
            options.failure_message,
            pool.in_tx_with(
                options.transaction,
                async move |tx| -> Result<u64, BatchError> {
                    tx.set_statement_timeout(CLEANUP_STATEMENT_TIMEOUT).await?;
                    delete(tx, limit).await.map_err(BatchError::from)
                },
            ),
        )
        .await
    }

    async fn await_batch(
        &mut self,
        failure_message: &'static str,
        batch: impl Future<Output = Result<u64, BatchError>>,
    ) -> Result<u64, CleanupError> {
        let failure = match tokio::time::timeout(CLEANUP_BATCH_BUDGET, batch).await {
            Ok(Ok(rows)) => {
                self.committed(rows);
                return Ok(rows);
            }
            Ok(Err(BatchError { class, driver })) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    cleanup = self.cleanup.as_str(),
                    failure = %class,
                    sqlstate = sqlstate(&driver).as_deref(),
                    cause = failure_cause(&driver),
                    "{failure_message}"
                );
                class
            }
            Err(_) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    cleanup = self.cleanup.as_str(),
                    "postgres_cleanup_batch_timed_out"
                );
                CleanupError::TimedOut
            }
        };
        self.outcome = CleanupOutcome::Failed;
        Err(failure)
    }

    pub fn committed(&mut self, rows: u64) {
        self.batches.increment(1);
        self.rows.increment(rows);
        self.committed_batches = self.committed_batches.saturating_add(1);
        self.removed_rows = self.removed_rows.saturating_add(rows);
    }

    #[must_use]
    pub fn complete(mut self) -> u64 {
        self.outcome = CleanupOutcome::Completed;
        self.removed_rows
    }

    pub fn fail(mut self) {
        self.outcome = CleanupOutcome::Failed;
    }
}

impl Drop for CleanupPass {
    fn drop(&mut self) {
        let elapsed_seconds = self.started.elapsed().as_secs_f64();
        let cleanup = self.cleanup.as_str();
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
