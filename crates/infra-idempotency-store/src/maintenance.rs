use std::time::Duration;

use infra_postgres::{
    Cleanup, CleanupError, CleanupPass, CleanupPassResult, MaintenanceFailure, PopulationSample,
    Tx, failure_cause, in_observation_tx, observed, run_scheduled_cleanup, sqlstate,
};
use tokio_util::sync::CancellationToken;

use crate::{READ_COMMITTED, Store};

const STARTUP_CHECK_BUDGET: Duration = Duration::from_secs(5);

const CLEANUP_BATCH_ROWS: u32 = 500;

pub const CLEANUP_RUNS_METRIC: &str = "http_idempotency_cleanup_runs_total";

pub const CLEANUP_REMOVED_METRIC: &str = "http_idempotency_cleanup_removed_records_total";

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartupError {
    #[error("the PostgreSQL session is not writable")]
    NotWritable,
    #[error("the idempotency store is unavailable")]
    Unavailable,
}

impl Store {
    pub async fn check_startup(&self) -> Result<(), StartupError> {
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

    pub async fn remove_expired(&self) -> Result<u64, CleanupError> {
        self.cleanup_pass(None).await.map(|pass| pass.removed_rows)
    }

    async fn cleanup_pass(
        &self,
        cancel: Option<&CancellationToken>,
    ) -> Result<CleanupPassResult, CleanupError> {
        CleanupPass::start(Cleanup::HttpIdempotency)
            .run_until_short_batch(
                &self.pool,
                READ_COMMITTED,
                cancel,
                CLEANUP_BATCH_ROWS,
                CLEANUP_REMOVED_METRIC,
                "http_idempotency_cleanup_failed",
                Self::delete_expired_batch,
            )
            .await
    }

    async fn delete_expired_batch(tx: &mut Tx<'_>, batch_limit: u32) -> Result<u64, sqlx::Error> {
        let deleted = observed(
            "delete expired idempotency records",
            sqlx::query!(
                "WITH batch AS ( \
                         SELECT ctid FROM http_idempotency_records \
                         WHERE expires_at <= statement_timestamp() \
                         ORDER BY expires_at LIMIT $1 FOR UPDATE SKIP LOCKED) \
                         DELETE FROM http_idempotency_records AS r USING batch \
                         WHERE r.ctid = batch.ctid AND r.expires_at <= statement_timestamp()",
                i64::from(batch_limit),
            )
            .execute(tx),
        )
        .await?;
        Ok(deleted.rows_affected())
    }

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
            || async { self.cleanup_pass(Some(&cancel)).await.map(|pass| pass.end) },
            || self.sample_expired_population(),
        )
        .await;
    }

    async fn sample_expired_population(&self) -> Result<PopulationSample, MaintenanceFailure> {
        in_observation_tx(&self.pool, async |tx| {
            observed(
                "observe expired idempotency records",
                sqlx::query_as!(
                    PopulationSample,
                    "SELECT extract(epoch FROM statement_timestamp())::float8 AS \"observed_at_unix_seconds!\", \
                            extract(epoch FROM min(expires_at))::float8 AS \"oldest_at_unix_seconds?\" \
                     FROM http_idempotency_records WHERE expires_at <= statement_timestamp()"
                )
                .fetch_one(&mut *tx),
            )
            .await
        })
        .await
    }
}
