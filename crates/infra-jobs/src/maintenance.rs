//! The startup check, retention, and the live-job gauges.

use std::sync::Arc;
use std::time::Duration;

use infra_postgres::{in_tx, observed};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::engine::{
    Operation, OperationError, Shared, StartupError, backstop, observe_failure, observe_recovery,
};

/// Gauge of live jobs. Labels `kind`, `state` (`available`, `scheduled`, `running`).
pub(crate) const LIVE_JOBS_METRIC: &str = "jobs_live_jobs";
/// Age of the oldest available job. Label `kind`. Zero when none.
pub(crate) const OLDEST_AVAILABLE_AGE_METRIC: &str = "jobs_oldest_available_age_seconds";
/// Database timestamp of the last successful observation.
pub(crate) const OBSERVATION_TIMESTAMP_METRIC: &str = "jobs_observation_timestamp_seconds";
/// How long a completed job is kept.
pub(crate) const RETAIN_COMPLETED_FOR: Duration = Duration::from_hours(24);
/// How long a failed job is kept.
pub(crate) const RETAIN_FAILED_FOR: Duration = Duration::from_hours(7 * 24);
/// Rows one retention batch deletes.
pub(crate) const RETENTION_BATCH_ROWS: i64 = 500;
/// How often a worker deletes terminal jobs. The first pass runs at once.
pub(crate) const RETENTION_INTERVAL: Duration = Duration::from_secs(60);
/// How often a worker samples the gauges. The first sample runs at once.
pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);
/// Maximum rows counted for one registered kind and live state.
pub(crate) const LIVE_JOBS_SAMPLE_CAP: i64 = 1_000;
/// Bound on the startup check, from acquire through the session query.
pub(crate) const STARTUP_CHECK_BUDGET: Duration = Duration::from_secs(5);
/// Check UTF8 server encoding and a writable session, bounded to 5 s.
///
/// # Errors
///
/// [`StartupError::UnsupportedEncoding`] when PostgreSQL is not UTF8,
/// [`StartupError::NotWritable`] when `writable` is false, and
/// [`StartupError::UnsupportedIsolation`] when the pool default is not read
/// committed, and [`StartupError::Unavailable`] for anything else, including
/// the bound; that one is logged with a bounded cause.
pub(crate) async fn check_startup(shared: &Shared) -> Result<(), StartupError> {
    let session = async {
        let mut connection = shared.pool.acquire().await?;
        // Whether the current session has the worker's required defaults. Migration-history
        // admission owns schema compatibility; this check keeps only live session properties.
        let session = observed(
            "check jobs session",
            sqlx::query!("SELECT current_setting('server_encoding') AS \"server_encoding!\", \
     NOT pg_is_in_recovery() AND current_setting('transaction_read_only') = 'off' AS \"writable!\", \
     current_setting('default_transaction_isolation') = 'read committed' AS \"read_committed!\"")
            .fetch_one(&mut *connection),
        )
        .await
        ?;
        Ok::<_, sqlx::Error>((session.server_encoding, session.writable, session.read_committed))
    };
    match tokio::time::timeout(STARTUP_CHECK_BUDGET, session).await {
        Ok(Ok((encoding, _, _))) if encoding != "UTF8" => Err(StartupError::UnsupportedEncoding),
        Ok(Ok((_, false, _))) => Err(StartupError::NotWritable),
        Ok(Ok((_, _, false))) => Err(StartupError::UnsupportedIsolation),
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => {
            tracing::warn!(
                sqlstate = infra_postgres::sqlstate(&err).as_deref(),
                cause = infra_postgres::failure_cause(&err),
                "jobs_startup_check_failed"
            );
            Err(StartupError::Unavailable)
        }
        Err(_elapsed) => {
            tracing::warn!(cause = "timeout", "jobs_startup_check_failed");
            Err(StartupError::Unavailable)
        }
    }
}

/// Delete expired terminal jobs in batches of 500 and return how many were deleted.
///
/// Completed batches run until one deletes fewer than 500, then failed batches
/// the same way. The first failed batch is returned; earlier batches stay committed.
///
/// # Errors
///
/// [`OperationError`] from the batch that failed.
pub(crate) async fn remove_expired(shared: &Shared) -> Result<u64, OperationError> {
    let completed = delete_until(shared, Finished::Completed, RETAIN_COMPLETED_FOR).await?;
    let failed = delete_until(shared, Finished::Failed, RETAIN_FAILED_FOR).await?;
    Ok(completed.saturating_add(failed))
}

/// One retention pass every 60 s, the first at once, until `cancel` fires.
pub(crate) async fn run_retention(shared: Arc<Shared>, cancel: CancellationToken) {
    let _ = Box::pin(cancel.run_until_cancelled(async {
        let mut ticker = tokio::time::interval(RETENTION_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match remove_expired(&shared).await {
                Ok(_) => observe_recovery(&shared, Operation::Retention),
                Err(error) => observe_failure(&shared, Operation::Retention, &error),
            }
        }
    }))
    .await;
}

/// One gauge sample every 10 s, the first at once, until `cancel` fires.
pub(crate) async fn run_sampling(shared: Arc<Shared>, cancel: CancellationToken) {
    let _ = Box::pin(cancel.run_until_cancelled(async {
        let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match sample_once(&shared).await {
                Ok(sample) => {
                    publish_sample(&sample);
                    observe_recovery(&shared, Operation::Sample);
                }
                Err(error) => {
                    observe_failure(&shared, Operation::Sample, &error);
                }
            }
        }
    }))
    .await;
}

/// Describe the queue-observation gauges and publish neutral pre-sample values.
/// The worker composition root calls this once at startup.
pub(crate) fn init_metrics(shared: &Shared) {
    metrics::describe_gauge!(
        LIVE_JOBS_METRIC,
        "Per-process capped depth of live jobs by registered kind and state."
    );
    metrics::describe_gauge!(
        OLDEST_AVAILABLE_AGE_METRIC,
        "Age in seconds of the oldest available job in one per-process sample."
    );
    metrics::describe_gauge!(
        OBSERVATION_TIMESTAMP_METRIC,
        "Database Unix timestamp of the last successful jobs observation."
    );
    for kind in shared.registry.names() {
        set_live(kind, "available", 0);
        set_live(kind, "scheduled", 0);
        set_live(kind, "running", 0);
        metrics::gauge!(OLDEST_AVAILABLE_AGE_METRIC, "kind" => kind).set(0.0);
    }
    metrics::gauge!(OBSERVATION_TIMESTAMP_METRIC).set(0.0);
}

/// Which terminal state a retention batch deletes.
#[derive(Clone, Copy)]
enum Finished {
    Completed,
    Failed,
}

async fn delete_until(
    shared: &Shared,
    finished: Finished,
    age: Duration,
) -> Result<u64, OperationError> {
    let mut removed = 0u64;
    let limit = u64::try_from(RETENTION_BATCH_ROWS).unwrap_or(0);
    loop {
        let batch = delete_batch(shared, finished, age).await?;
        removed = removed.saturating_add(batch);
        if batch < limit {
            return Ok(removed);
        }
    }
}

async fn delete_batch(
    shared: &Shared,
    finished: Finished,
    age: Duration,
) -> Result<u64, OperationError> {
    let Ok(_permit) = shared.permit.acquire().await else {
        // The engine semaphore is never closed.
        return Err(OperationError::Acquire(sqlx::Error::PoolClosed));
    };
    backstop(async {
        in_tx(&shared.pool, async |tx| -> Result<u64, OperationError> {
            // `SET LOCAL` lasts until the transaction ends, so PostgreSQL restores the
            // session timeout itself on commit, rollback, or a dropped connection.
            observed(
                "set statement timeout",
                sqlx::query!("SET LOCAL statement_timeout = '1000ms'").execute(&mut *tx),
            )
            .await?;
            // The state stays a literal: a bound state cannot prove the partial
            // `background_jobs_terminal` predicate, so a generic plan would
            // scan the table.
            let delete = match finished {
                Finished::Completed => sqlx::query!(
                    "DELETE FROM background_jobs \
                     WHERE id = ANY (ARRAY( \
                         SELECT id FROM background_jobs \
                         WHERE state = 'completed' AND finished_at <= statement_timestamp() - $1::interval \
                         ORDER BY finished_at \
                         LIMIT $2 \
                         FOR UPDATE SKIP LOCKED))",
                    age as _,
                    RETENTION_BATCH_ROWS,
                ),
                Finished::Failed => sqlx::query!(
                    "DELETE FROM background_jobs \
                     WHERE id = ANY (ARRAY( \
                         SELECT id FROM background_jobs \
                         WHERE state = 'failed' AND finished_at <= statement_timestamp() - $1::interval \
                         ORDER BY finished_at \
                         LIMIT $2 \
                         FOR UPDATE SKIP LOCKED))",
                    age as _,
                    RETENTION_BATCH_ROWS,
                ),
            };
            Ok(observed("delete finished jobs", delete.execute(&mut *tx))
                .await?
                .rows_affected())
        })
        .await
    })
    .await
}

async fn sample_once(shared: &Shared) -> Result<Sample, OperationError> {
    let Ok(_permit) = shared.permit.acquire().await else {
        // The engine semaphore is never closed.
        return Err(OperationError::Acquire(sqlx::Error::PoolClosed));
    };
    let kinds: Vec<&str> = shared.registry.names().collect();
    backstop(async {
        in_tx(&shared.pool, async |tx| -> Result<Sample, OperationError> {
            observed(
                "set statement timeout",
                sqlx::query!("SET LOCAL statement_timeout = '2000ms'").execute(&mut *tx),
            )
            .await?;
            let rows = observed(
                "sample jobs",
                sqlx::query_as!(
                    SampleRow,
                    "WITH sampled AS ( \
                         SELECT statement_timestamp() AS observed_at \
                     ), registered AS ( \
                         SELECT name.kind \
                         FROM unnest($1::text[]) AS name(kind) \
                     ) \
                     SELECT registered.kind AS \"kind!\", \
                            available.count AS \"available!\", \
                            scheduled.count AS \"scheduled!\", \
                            running.count AS \"running!\", \
                            COALESCE(EXTRACT(EPOCH FROM sampled.observed_at - oldest.not_before), 0)::double precision \
                                AS \"oldest_available_seconds!\", \
                            EXTRACT(EPOCH FROM sampled.observed_at)::double precision AS \"observed_at!\" \
                     FROM sampled \
                     CROSS JOIN registered \
                     CROSS JOIN LATERAL ( \
                         SELECT count(*) AS count \
                         FROM ( \
                             SELECT 1 \
                             FROM background_jobs AS job \
                             WHERE job.kind = registered.kind \
                               AND job.state = 'pending' \
                               AND job.not_before <= sampled.observed_at \
                            LIMIT $2 \
                         ) AS capped \
                     ) AS available \
                     CROSS JOIN LATERAL ( \
                         SELECT count(*) AS count \
                         FROM ( \
                             SELECT 1 \
                             FROM background_jobs AS job \
                             WHERE job.kind = registered.kind \
                               AND job.state = 'pending' \
                               AND job.not_before > sampled.observed_at \
                            LIMIT $2 \
                         ) AS capped \
                     ) AS scheduled \
                     CROSS JOIN LATERAL ( \
                         SELECT count(*) AS count \
                         FROM ( \
                             SELECT 1 \
                             FROM background_jobs AS job \
                             WHERE job.kind = registered.kind \
                               AND job.state = 'running' \
                            LIMIT $2 \
                         ) AS capped \
                     ) AS running \
                     LEFT JOIN LATERAL ( \
                         SELECT job.not_before \
                         FROM background_jobs AS job \
                         WHERE job.kind = registered.kind \
                           AND job.state = 'pending' \
                           AND job.not_before <= sampled.observed_at \
                         ORDER BY job.not_before, job.id \
                         LIMIT 1 \
                     ) AS oldest ON true",
                    &kinds as _,
                    LIVE_JOBS_SAMPLE_CAP,
                )
                .fetch_all(&mut *tx),
            )
            .await?;
            decode_sample(rows)
        })
        .await
    })
    .await
}

struct SampleRow {
    kind: String,
    available: i64,
    scheduled: i64,
    running: i64,
    oldest_available_seconds: f64,
    observed_at: f64,
}

struct Sample {
    rows: Vec<SampleRow>,
    observed_at: f64,
}

fn decode_sample(rows: Vec<SampleRow>) -> Result<Sample, OperationError> {
    let Some(observed_at) = rows.first().map(|row| row.observed_at) else {
        return Err(OperationError::Statement(sqlx::Error::Decode(
            "jobs sample returned no rows".into(),
        )));
    };
    Ok(Sample { rows, observed_at })
}

fn publish_sample(sample: &Sample) {
    for row in &sample.rows {
        set_live(&row.kind, "available", row.available);
        set_live(&row.kind, "scheduled", row.scheduled);
        set_live(&row.kind, "running", row.running);
        metrics::gauge!(OLDEST_AVAILABLE_AGE_METRIC, "kind" => row.kind.clone())
            .set(row.oldest_available_seconds);
    }
    metrics::gauge!(OBSERVATION_TIMESTAMP_METRIC).set(sample.observed_at);
}

fn set_live(kind: impl Into<String>, state: &'static str, count: i64) {
    let kind = kind.into();
    #[allow(clippy::cast_precision_loss)]
    let value = count as f64;
    metrics::gauge!(LIVE_JOBS_METRIC, "kind" => kind, "state" => state).set(value);
}
