use super::{
    Operation, OperationError, SAMPLE_STATEMENT_TIMEOUT, WorkerGroup, in_maintenance_tx,
    observe_operation, observed,
};
use crate::outbox::KIND;
use metrics::Unit;

const OLDEST_UNCONFIRMED_AGE_METRIC: &str = "jobs_outbox_oldest_unconfirmed_age_seconds";
const OLDEST_BLOCKED_AGE_METRIC: &str = "jobs_outbox_oldest_blocked_age_seconds";
const OUTBOX_OBSERVATION_TIMESTAMP_METRIC: &str = "jobs_outbox_observation_timestamp_seconds";
const LAST_ATTEMPT_SUCCESS_METRIC: &str = "jobs_outbox_last_attempt_success";
const ORDERED_SAMPLE_CAP: i64 = 1_000;

pub(super) struct OutboxGauges {
    oldest_unconfirmed: metrics::Gauge,
    oldest_blocked: metrics::Gauge,
    observation_timestamp: metrics::Gauge,
    last_attempt_success: metrics::Gauge,
}

impl OutboxGauges {
    pub(super) fn init_if_registered(kinds: &[&str]) -> Option<Self> {
        if !kinds.contains(&KIND) {
            return None;
        }
        metrics::describe_gauge!(
            OLDEST_UNCONFIRMED_AGE_METRIC,
            Unit::Seconds,
            "Age since creation of the oldest retained outbox job without recorded completion."
        );
        metrics::describe_gauge!(
            OLDEST_BLOCKED_AGE_METRIC,
            Unit::Seconds,
            "Age since creation of the oldest outbox event blocked by an unfinished predecessor. NaN indicates no blocker was found within a capped sample."
        );
        metrics::describe_gauge!(
            OUTBOX_OBSERVATION_TIMESTAMP_METRIC,
            Unit::Seconds,
            "Database Unix timestamp of the last successful outbox observation."
        );
        metrics::describe_gauge!(
            LAST_ATTEMPT_SUCCESS_METRIC,
            "Whether the most recent outbox observation succeeded."
        );
        Some(Self {
            oldest_unconfirmed: metrics::gauge!(OLDEST_UNCONFIRMED_AGE_METRIC),
            oldest_blocked: metrics::gauge!(OLDEST_BLOCKED_AGE_METRIC),
            observation_timestamp: metrics::gauge!(OUTBOX_OBSERVATION_TIMESTAMP_METRIC),
            last_attempt_success: metrics::gauge!(LAST_ATTEMPT_SUCCESS_METRIC),
        })
    }

    pub(super) fn reset(&self) {
        self.oldest_unconfirmed.set(f64::NAN);
        self.oldest_blocked.set(f64::NAN);
        self.observation_timestamp.set(0.0);
        self.last_attempt_success.set(0.0);
    }

    pub(super) async fn sample(&self, primary_group: &WorkerGroup) {
        let result = read_outbox_sample(primary_group).await;
        match observe_operation(primary_group, Operation::SampleOutbox, result) {
            Some(sample) => {
                self.oldest_unconfirmed.set(sample.oldest_unconfirmed_age);
                self.oldest_blocked.set(sample.oldest_blocked_age);
                self.observation_timestamp.set(sample.observed_at);
                self.last_attempt_success.set(1.0);
            }
            None => self.last_attempt_success.set(0.0),
        }
    }
}

struct OutboxSample {
    oldest_unconfirmed_age: f64,
    oldest_blocked_age: f64,
    observed_at: f64,
}

async fn read_outbox_sample(primary_group: &WorkerGroup) -> Result<OutboxSample, OperationError> {
    in_maintenance_tx(primary_group, SAMPLE_STATEMENT_TIMEOUT, async |tx| {
        Ok(observed!(
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
