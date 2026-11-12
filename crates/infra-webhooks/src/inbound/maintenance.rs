//! Expired receipt cleanup and the observation of its backlog.

use std::time::Duration;

use infra_postgres::{
    Cleanup, CleanupError, CleanupPass, MaintenanceFailure, PassEnd, PopulationSample, TxOptions,
    in_observation_tx, observed, run_scheduled_cleanup,
};
use tokio_util::sync::CancellationToken;

use super::Receiver;

/// A sender retries one message ID with fresh timestamps for its whole retry
/// horizon. Standard Webhooks senders retry for more than a day; this
/// template's nominal outbound backoff runs about six and a half days. The
/// fixed admission window does not cover every retry floor, outage, redrive,
/// or restore; consumers retain business effect identity for their replay lifetime.
/// The spec's 5-minute example only covers replay of one signed request.
const RECEIPT_RETENTION: Duration = Duration::from_hours(14 * 24);

#[expect(
    clippy::cast_possible_wrap,
    reason = "fourteen days of seconds is far inside i64"
)]
const RETENTION_SECONDS: i64 = RECEIPT_RETENTION.as_secs() as i64;

const CLEANUP_BATCH_ROWS: u32 = 500;

/// Periodic receipt cleanup runs, by `completed` or `failed`.
pub const CLEANUP_RUNS_METRIC: &str = "webhook_receipt_cleanup_runs_total";

/// Expired receipts the cleanup deleted, counted per committed batch.
pub const CLEANUP_REMOVED_METRIC: &str = "webhook_receipt_cleanup_removed_receipts_total";

impl Receiver {
    /// Delete expired receipts in batches of at most 500, returning confirmed
    /// progress after a short batch. Each batch is its own transaction and
    /// skips receipts another cleanup holds. An admission's
    /// `ON CONFLICT DO NOTHING` locks no existing receipt.
    /// Confirmed counts do not certify an empty expired population.
    ///
    /// # Errors
    ///
    /// The failure class of the batch that failed, logged with a bounded
    /// cause. Earlier batches stay committed. A timeout leaves the batch's
    /// commit outcome unconfirmed.
    pub async fn remove_expired(&self) -> Result<u64, CleanupError> {
        self.remove_expired_pass(&CancellationToken::new())
            .await
            .map(|(removed, _)| removed)
    }

    async fn remove_expired_pass(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(u64, PassEnd), CleanupError> {
        let mut pass = CleanupPass::start(Cleanup::WebhookReceipts);
        let mut removed = 0;
        loop {
            if cancel.is_cancelled() {
                return Ok((removed, PassEnd::Cancelled));
            }
            let batch = pass
                .run_batch(&self.pool, TxOptions::default(), async |tx| {
                    let deleted = observed(
                        "delete expired webhook receipts",
                        sqlx::query!(
                            "DELETE FROM webhook_receipts WHERE (endpoint_id, message_id) IN \
                         (SELECT endpoint_id, message_id FROM webhook_receipts \
                         WHERE received_at < statement_timestamp() - $1::bigint * interval '1 second' \
                         ORDER BY received_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
                            RETENTION_SECONDS,
                            i64::from(CLEANUP_BATCH_ROWS),
                        )
                        .execute(tx),
                    )
                    .await?;
                    Ok(deleted.rows_affected())
                })
                .await?;
            metrics::counter!(CLEANUP_REMOVED_METRIC).increment(batch);
            removed += batch;
            if batch < u64::from(CLEANUP_BATCH_ROWS) {
                pass.completed();
                return Ok((removed, PassEnd::Completed));
            }
        }
    }

    /// Run periodic cleanup and independent population observation in the same
    /// tracked owner. Neither failure changes readiness. Cancellation drops
    /// both loops, including any transaction in flight.
    pub async fn run_cleanup(self, cancel: CancellationToken) {
        metrics::describe_counter!(
            CLEANUP_RUNS_METRIC,
            metrics::Unit::Count,
            "Runs of the expired webhook receipt cleanup, by outcome."
        );
        metrics::describe_counter!(
            CLEANUP_REMOVED_METRIC,
            metrics::Unit::Count,
            "Expired webhook receipts the cleanup deleted."
        );
        run_scheduled_cleanup(
            Cleanup::WebhookReceipts,
            CLEANUP_RUNS_METRIC,
            &cancel,
            || async { self.remove_expired_pass(&cancel).await.map(|(_, end)| end) },
            || self.sample_population(),
        )
        .await;
    }

    async fn sample_population(&self) -> Result<PopulationSample, MaintenanceFailure> {
        in_observation_tx(&self.pool, async |tx| {
            let sample = observed(
                "observe expired webhook receipts",
                sqlx::query!(
                    r#"SELECT extract(epoch FROM observation.observed_at)::float8 AS "observed_at!",
                                  extract(epoch FROM oldest.received_at + $1::bigint * interval '1 second')::float8 AS "oldest?"
                           FROM (SELECT statement_timestamp() AS observed_at) observation
                           LEFT JOIN LATERAL (
                               SELECT received_at FROM webhook_receipts
                               WHERE received_at < observation.observed_at - $1::bigint * interval '1 second'
                               ORDER BY received_at LIMIT 1
                           ) oldest ON true"#,
                    RETENTION_SECONDS,
                )
                .fetch_one(&mut *tx),
            )
            .await?;
            Ok(PopulationSample {
                observed_at: sample.observed_at,
                oldest: sample.oldest,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_metrics::CounterKeys;

    #[tokio::test]
    async fn a_cleanup_run_without_a_database_counts_as_failed_and_removes_nothing() {
        let keys = CounterKeys::default();
        let _local = metrics::set_default_local_recorder(&keys);
        // Nothing listens on port 1, so every acquire ends at the pool's bound.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("postgres://127.0.0.1:1/unreachable")
            .expect("a lazy pool opens no connection");
        let receiver = Receiver::new(pool, []);
        assert_eq!(receiver.remove_expired().await, Err(CleanupError::Acquire));

        let failed = async {
            loop {
                let counted = keys.0.lock().expect("keys").iter().any(|key| {
                    key.name() == CLEANUP_RUNS_METRIC
                        && key
                            .labels()
                            .any(|label| (label.key(), label.value()) == ("outcome", "failed"))
                });
                if counted {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::select! {
            () = receiver.clone().run_cleanup(CancellationToken::new()) => {
                panic!("the cleanup task ends only on cancel")
            }
            () = failed => {}
        }
        assert!(
            keys.0
                .lock()
                .expect("keys")
                .iter()
                .all(|key| key.name() != CLEANUP_REMOVED_METRIC),
            "a failed batch removed nothing"
        );
    }
}
