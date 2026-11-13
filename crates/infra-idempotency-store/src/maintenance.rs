//! The startup check and the bounded cleanup of expired records.

use std::time::Duration;

use infra_postgres::{
    Cleanup, CleanupError, CleanupPass, MaintenanceFailure, PassEnd, PopulationSample, Tx,
    failure_cause, in_observation_tx, observed, run_scheduled_cleanup, sqlstate,
};
use tokio_util::sync::CancellationToken;

use crate::{READ_COMMITTED, Store};

const STARTUP_CHECK_BUDGET: Duration = Duration::from_secs(5);

const CLEANUP_BATCH_ROWS: u32 = 500;

/// Runs of periodic cleanup, by `outcome` (`completed`, `failed`).
pub const CLEANUP_RUNS_METRIC: &str = "http_idempotency_cleanup_runs_total";

/// Expired records the cleanup deleted, counted per committed batch.
pub const CLEANUP_REMOVED_METRIC: &str = "http_idempotency_cleanup_removed_records_total";

/// Why an active idempotency boundary cannot start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartupError {
    #[error("the PostgreSQL session is not writable")]
    NotWritable,
    /// Anything else, including the check's bound.
    #[error("the idempotency store is unavailable")]
    Unavailable,
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
    /// deletes fewer. Return confirmed deletions, which do not certify that
    /// the population is empty. Each batch runs in its own `READ COMMITTED`
    /// transaction and skips locked records.
    ///
    /// # Errors
    ///
    /// The failure class of the batch that failed, logged with a bounded
    /// cause. Earlier batches stay committed. A timeout leaves the batch's
    /// commit outcome unconfirmed.
    pub async fn remove_expired(&self) -> Result<u64, CleanupError> {
        self.cleanup_pass(None).await.map(|(removed, _)| removed)
    }

    async fn cleanup_pass(
        &self,
        cancel: Option<&CancellationToken>,
    ) -> Result<(u64, PassEnd), CleanupError> {
        CleanupPass::start(Cleanup::HttpIdempotency)
            .run_until_short_batch(
                &self.pool,
                READ_COMMITTED,
                cancel,
                CLEANUP_BATCH_ROWS,
                CLEANUP_REMOVED_METRIC,
                Self::delete_expired_batch,
            )
            .await
    }

    async fn delete_expired_batch(tx: &mut Tx<'_>) -> Result<u64, sqlx::Error> {
        // The tuple locator is consumed under its row lock within this statement.
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
            .execute(tx),
        )
        .await?;
        Ok(deleted.rows_affected())
    }

    /// Periodic cleanup and independent population observation under one owner.
    /// Cleanup follows the shared schedule. Every run that ends without
    /// cancellation counts its outcome; a failed run waits for the next tick
    /// and changes neither readiness nor serving.
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
        run_scheduled_cleanup(
            Cleanup::HttpIdempotency,
            CLEANUP_RUNS_METRIC,
            &cancel,
            || async { self.cleanup_pass(Some(&cancel)).await.map(|(_, end)| end) },
            || self.sample_expired_population(),
        )
        .await;
    }

    async fn sample_expired_population(&self) -> Result<PopulationSample, MaintenanceFailure> {
        in_observation_tx(&self.pool, async |tx| {
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
            Ok(PopulationSample {
                observed_at_unix_seconds: sample.observed_at,
                oldest_at_unix_seconds: sample.oldest,
            })
        })
        .await
    }
}
