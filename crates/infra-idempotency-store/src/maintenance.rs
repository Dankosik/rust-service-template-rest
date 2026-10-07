//! The startup check and the bounded cleanup of expired records.

use std::time::Duration;

use infra_postgres::{
    CleanupBudget, CleanupPass, CleanupSchedule, MAINTENANCE_OBSERVATION_BUDGET,
    MAINTENANCE_OBSERVATION_INTERVAL, MaintenanceFailure, MaintenanceObserver,
    MaintenancePopulation, TxError, TxOptions, failure_cause, in_tx_with, observed, sqlstate,
};
use tokio_util::sync::CancellationToken;

use crate::{READ_COMMITTED, Store};

/// Bound on the whole startup check, from the acquire to the result.
const STARTUP_CHECK_BUDGET: Duration = Duration::from_secs(5);

/// The most rows one cleanup batch deletes.
const CLEANUP_BATCH_ROWS: u32 = 500;

/// Runs of periodic cleanup, by `outcome` (`completed`, `budget_exhausted`, `failed`).
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
    #[error("timeout")]
    TimedOut,
}

struct CleanupResult {
    removed: u64,
    outcome: &'static str,
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
    /// deletes fewer or the pass admission budget ends. Return confirmed
    /// deletions, which do not certify that the population is empty. Each batch is its
    /// own `READ COMMITTED` transaction with a 5 s statement timeout, and
    /// skips records a running attempt holds.
    ///
    /// # Errors
    ///
    /// The failure class of the batch that failed, logged with a bounded
    /// cause; earlier batches stay committed.
    pub async fn remove_expired(&self) -> Result<u64, CleanupError> {
        self.cleanup_pass(None).await.map(|result| result.removed)
    }

    async fn cleanup_pass(
        &self,
        cancel: Option<&CancellationToken>,
    ) -> Result<CleanupResult, CleanupError> {
        let mut pass = CleanupPass::start("http_idempotency");
        let mut budget = CleanupBudget::start();
        let mut removed = 0;
        loop {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Ok(CleanupResult {
                    removed,
                    outcome: "cancelled",
                });
            }
            let admitted = budget.admit().await;
            // Cancellation can arrive during pacing. A ready prior commit was
            // accounted before this boundary, but no next batch may start.
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Ok(CleanupResult {
                    removed,
                    outcome: "cancelled",
                });
            }
            if !admitted {
                pass.budget_exhausted();
                return Ok(CleanupResult {
                    removed,
                    outcome: "budget_exhausted",
                });
            }
            let batch = budget
                .batch(in_tx_with(
                    &self.pool,
                    READ_COMMITTED,
                    async |tx| -> Result<u64, CleanupError> {
                        // Bounds the batch on the server, so a batch whose client has
                        // gone still ends within 5 s.
                        observed(
                            "set statement timeout",
                            sqlx::query!("SELECT set_config('statement_timeout', $1, true)", "5s")
                                .fetch_one(&mut *tx),
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
                ))
                .await
                .map_err(|_| {
                    tracing::warn!(failure = "timeout", "http_idempotency_cleanup_failed");
                    CleanupError::TimedOut
                })
                .and_then(std::convert::identity)
                .inspect_err(|_| pass.failed())?;
            pass.committed(batch);
            metrics::counter!(CLEANUP_REMOVED_METRIC).increment(batch);
            removed += batch;
            if batch < u64::from(CLEANUP_BATCH_ROWS) {
                pass.completed();
                return Ok(CleanupResult {
                    removed,
                    outcome: "completed",
                });
            }
        }
    }

    /// Periodic cleanup and independent population observation under one owner.
    /// Cleanup follows the shared schedule. Every run counts its outcome; a failed
    /// run waits for the next tick and changes neither readiness nor serving.
    /// Returns when `cancel` fires, dropping a run in flight.
    pub async fn run_cleanup(self, cancel: CancellationToken) {
        if cancel.is_cancelled() {
            return;
        }
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
        let _ = Box::pin(cancel.run_until_cancelled(async {
            tokio::join!(self.cleanup_loop(&cancel), self.observe_population(&cancel));
        }))
        .await;
    }

    async fn cleanup_loop(&self, cancel: &CancellationToken) {
        let mut schedule = CleanupSchedule::new("http_idempotency");
        loop {
            if cancel.is_cancelled() {
                return;
            }
            schedule.next().await;
            if cancel.is_cancelled() {
                return;
            }
            let outcome = match self.cleanup_pass(Some(cancel)).await {
                Ok(result) if result.outcome == "cancelled" => return,
                Ok(result) => result.outcome,
                Err(_) => "failed",
            };
            metrics::counter!(CLEANUP_RUNS_METRIC, "outcome" => outcome).increment(1);
        }
    }

    async fn observe_population(&self, cancel: &CancellationToken) {
        if cancel.is_cancelled() {
            return;
        }
        let mut observer = MaintenanceObserver::start(MaintenancePopulation::HttpIdempotency);
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let attempt = observer.attempt();
            match tokio::time::timeout(MAINTENANCE_OBSERVATION_BUDGET, self.population()).await {
                Ok(Ok((observed_at, oldest))) => {
                    // The observer retains the last good sample on invalid clocks.
                    let _ = attempt.succeeded(observed_at, oldest);
                }
                Ok(Err(failure)) => attempt.failed(failure),
                Err(_) => attempt.failed(MaintenanceFailure::TimedOut),
            }
            if cancel.is_cancelled() {
                return;
            }
            tokio::time::sleep(MAINTENANCE_OBSERVATION_INTERVAL).await;
        }
    }

    async fn population(&self) -> Result<(f64, Option<f64>), MaintenanceFailure> {
        in_tx_with(
            &self.pool,
            TxOptions {
                read_only: true,
                ..READ_COMMITTED
            },
            async |tx| {
                observed(
                    "set maintenance statement timeout",
                    sqlx::query!("SET LOCAL statement_timeout = '2000ms'").execute(&mut *tx),
                )
                .await?;
                observed(
                    "set maintenance lock timeout",
                    sqlx::query!("SET LOCAL lock_timeout = '100ms'").execute(&mut *tx),
                )
                .await?;
                observed(
                    "set maintenance idle transaction timeout",
                    sqlx::query!("SET LOCAL idle_in_transaction_session_timeout = '5000ms'")
                        .execute(&mut *tx),
                )
                .await?;
                let sample = observed(
                    "observe expired idempotency records",
                    sqlx::query!(
                        "SELECT extract(epoch FROM clock.observed_at)::float8 AS \"observed_at!\", \
                         extract(epoch FROM oldest.expires_at)::float8 AS \"oldest?\" \
                         FROM (SELECT statement_timestamp() AS observed_at) AS clock \
                         LEFT JOIN LATERAL ( \
                             SELECT expires_at FROM http_idempotency_records \
                             WHERE expires_at <= clock.observed_at \
                             ORDER BY expires_at LIMIT 1 \
                         ) AS oldest ON true"
                    )
                    .fetch_one(&mut *tx),
                )
                .await?;
                Ok((sample.observed_at, sample.oldest))
            },
        )
        .await
    }
}
