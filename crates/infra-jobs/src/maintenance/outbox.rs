use super::{
    Operation, OperationError, SAMPLE_STATEMENT_TIMEOUT, Shared, in_maintenance_tx,
    observe_failure, observe_recovery, observed,
};
use crate::outbox::KIND;
use metrics::Unit;

const OLDEST_UNCONFIRMED: &str = "jobs_outbox_oldest_unconfirmed_age_seconds";
const OLDEST_BLOCKED: &str = "jobs_outbox_oldest_blocked_age_seconds";
const OBSERVED_AT: &str = "jobs_outbox_observation_timestamp_seconds";
const LAST_ATTEMPT_SUCCESS: &str = "jobs_outbox_last_attempt_success";
const ORDERED_SAMPLE_CAP: i64 = 1_000;

pub(super) fn is_registered(kinds: &[&str]) -> bool {
    kinds.contains(&KIND)
}

pub(super) fn init_metrics() {
    metrics::describe_gauge!(
        OLDEST_UNCONFIRMED,
        Unit::Seconds,
        "Age since creation of the oldest retained outbox job without recorded completion."
    );
    metrics::describe_gauge!(
        OLDEST_BLOCKED,
        Unit::Seconds,
        "Age since creation of the oldest outbox event blocked by an unfinished predecessor. NaN indicates no blocker was found within a capped sample."
    );
    metrics::describe_gauge!(
        OBSERVED_AT,
        Unit::Seconds,
        "Database Unix timestamp of the last successful outbox observation."
    );
    metrics::describe_gauge!(
        LAST_ATTEMPT_SUCCESS,
        "Whether the most recent outbox observation succeeded."
    );
    reset_metrics();
}

pub(super) fn reset_metrics() {
    metrics::gauge!(OLDEST_UNCONFIRMED).set(f64::NAN);
    metrics::gauge!(OLDEST_BLOCKED).set(f64::NAN);
    metrics::gauge!(OBSERVED_AT).set(0.0);
    metrics::gauge!(LAST_ATTEMPT_SUCCESS).set(0.0);
}

pub(super) async fn sample(shared: &Shared) {
    match read(shared).await {
        Ok(sample) => {
            metrics::gauge!(OLDEST_UNCONFIRMED).set(sample.oldest_unconfirmed_age);
            metrics::gauge!(OLDEST_BLOCKED).set(sample.oldest_blocked_age);
            metrics::gauge!(OBSERVED_AT).set(sample.observed_at);
            metrics::gauge!(LAST_ATTEMPT_SUCCESS).set(1.0);
            observe_recovery(shared, Operation::SampleOutbox);
        }
        Err(error) => {
            metrics::gauge!(LAST_ATTEMPT_SUCCESS).set(0.0);
            observe_failure(shared, Operation::SampleOutbox, &error);
        }
    }
}

struct OutboxSample {
    oldest_unconfirmed_age: f64,
    oldest_blocked_age: f64,
    observed_at: f64,
}

async fn read(shared: &Shared) -> Result<OutboxSample, OperationError> {
    in_maintenance_tx(shared, SAMPLE_STATEMENT_TIMEOUT, async |tx| {
        Ok(observed(
            "sample outbox",
            sqlx::query_as!(
                OutboxSample,
                r#"
                    WITH ordered_candidates AS MATERIALIZED (
                        SELECT created_at, predecessor_job_id
                        FROM background_jobs
                        WHERE kind = 'publish_domain_event'
                          AND state = 'pending' AND predecessor_job_id IS NOT NULL
                        ORDER BY created_at
                        LIMIT ($1::bigint + 1)
                    ), oldest_blocked AS (
                        SELECT candidate.created_at
                        FROM ordered_candidates AS candidate
                        CROSS JOIN LATERAL (
                            SELECT 1 FROM background_jobs AS predecessor
                            WHERE predecessor.id = candidate.predecessor_job_id
                              AND predecessor.state <> 'completed'
                            LIMIT 1
                        ) AS blocker
                        ORDER BY candidate.created_at
                        LIMIT 1
                    )
                    SELECT
                        GREATEST(0, EXTRACT(EPOCH FROM statement_timestamp() - (
                            SELECT created_at
                            FROM background_jobs
                            WHERE kind = 'publish_domain_event' AND state <> 'completed'
                            ORDER BY created_at
                            LIMIT 1
                        )))::double precision AS "oldest_unconfirmed_age!",
                        COALESCE(
                            (SELECT GREATEST(0, EXTRACT(EPOCH FROM statement_timestamp() - created_at))
                             FROM oldest_blocked),
                            CASE WHEN (SELECT count(*) FROM ordered_candidates) > $1
                                 THEN 'NaN'::double precision ELSE 0 END
                        )::double precision AS "oldest_blocked_age!",
                        EXTRACT(EPOCH FROM statement_timestamp())::double precision AS "observed_at!"
                    "#,
                ORDERED_SAMPLE_CAP,
            )
            .fetch_one(&mut *tx),
        )
        .await?)
    })
    .await
}
