//! The startup check, retention, and the live-job gauges.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use infra_postgres::{TxError, connection, in_tx_with};
use sqlx::Row;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::engine::{
    OpFailed, Operation, OperationError, READ_COMMITTED, Shared, StartupError, backstop,
    observe_failure, observe_recovery,
};

/// Gauge of live jobs. Labels `kind`, `state` (`available`, `scheduled`, `running`).
pub const LIVE_JOBS_METRIC: &str = "jobs_live_jobs";
/// Whether a live-job sample reached its per-kind/state cap. Labels `kind`, `state`.
pub const LIVE_JOBS_CENSORED_METRIC: &str = "jobs_live_jobs_censored";
/// The fixed maximum published live-job count.
pub const LIVE_JOBS_SAMPLE_CAP_METRIC: &str = "jobs_live_jobs_sample_cap";
/// Age of the oldest available job. Label `kind`. Zero when none.
pub const OLDEST_AVAILABLE_AGE_METRIC: &str = "jobs_oldest_available_age_seconds";
/// Database timestamp of the last successfully committed observation.
pub const OBSERVATION_TIMESTAMP_METRIC: &str = "jobs_observation_timestamp_seconds";
/// Whether the current observation gauges came from a successful sample.
pub const OBSERVATION_SUCCESS_METRIC: &str = "jobs_observation_success";
/// How long a completed job is kept.
pub const RETAIN_COMPLETED_FOR: Duration = Duration::from_hours(24);
/// How long a failed job is kept.
pub const RETAIN_FAILED_FOR: Duration = Duration::from_hours(7 * 24);
/// Rows one retention batch deletes.
pub const RETENTION_BATCH_ROWS: i64 = 500;
/// How often a worker deletes terminal jobs. The first pass runs at once.
pub const RETENTION_INTERVAL: Duration = Duration::from_secs(60);
/// How often a worker samples the gauges. The first sample runs at once.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);
/// Maximum rows counted for one registered kind and live state.
pub const LIVE_JOBS_SAMPLE_CAP: i64 = 1_000;
const LIVE_JOBS_SAMPLE_CAP_VALUE: f64 = 1_000.0;
/// Bound on the startup check, from acquire to the transaction's end.
pub const STARTUP_CHECK_BUDGET: Duration = Duration::from_secs(5);
/// The writer check and the table's shape. A missing table or column fails the statement.
const STARTUP_CHECK: &str = "SELECT NOT pg_is_in_recovery() AND current_setting('transaction_read_only') = 'off' AS writable, \
     (SELECT count(*) FROM (SELECT id, kind, payload, unique_key, state, failure_reason, attempts, \
          claim_generation, not_before, claim_expires_at, finished_at, error_summary, trace_context, trace_state \
      FROM background_jobs LIMIT 0) AS shape) AS shape_rows, \
     COALESCE((SELECT attribute.atttypid = 'jsonb'::regtype \
          FROM pg_attribute AS attribute \
          WHERE attribute.attrelid = 'background_jobs'::regclass \
            AND attribute.attname = 'payload' \
            AND attribute.attnum > 0 \
            AND NOT attribute.attisdropped), false) AS payload_jsonb, \
     COALESCE((SELECT attribute.atttypid = 'text'::regtype \
          FROM pg_attribute AS attribute \
          WHERE attribute.attrelid = 'background_jobs'::regclass \
            AND attribute.attname = 'unique_key' \
            AND attribute.attnum > 0 \
            AND NOT attribute.attisdropped), false) AS unique_key_text";

const RETAIN_COMPLETED: &str = "DELETE FROM background_jobs \
     WHERE id = ANY (ARRAY( \
         SELECT id FROM background_jobs \
         WHERE state = 'completed' AND finished_at <= statement_timestamp() - $1 \
         ORDER BY finished_at \
         LIMIT $2 \
         FOR UPDATE SKIP LOCKED))";

const RETAIN_FAILED: &str = "DELETE FROM background_jobs \
     WHERE id = ANY (ARRAY( \
         SELECT id FROM background_jobs \
         WHERE state = 'failed' AND finished_at <= statement_timestamp() - $1 \
         ORDER BY finished_at \
         LIMIT $2 \
         FOR UPDATE SKIP LOCKED))";

const SAMPLE: &str = "WITH sampled AS ( \
         SELECT statement_timestamp() AS observed_at \
     ), registered AS ( \
         SELECT name.kind \
         FROM unnest($1::text[]) AS name(kind) \
     ) \
     SELECT registered.kind, \
            available.count AS available, \
            scheduled.count AS scheduled, \
            running.count AS running, \
            COALESCE(EXTRACT(EPOCH FROM sampled.observed_at - oldest.not_before), 0)::double precision \
                AS oldest_available_seconds, \
            EXTRACT(EPOCH FROM sampled.observed_at)::double precision AS observed_at \
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
             LIMIT 1001 \
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
             LIMIT 1001 \
         ) AS capped \
     ) AS scheduled \
     CROSS JOIN LATERAL ( \
         SELECT count(*) AS count \
         FROM ( \
             SELECT 1 \
             FROM background_jobs AS job \
             WHERE job.kind = registered.kind \
               AND job.state = 'running' \
             LIMIT 1001 \
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
     ) AS oldest ON true";

const RETENTION_STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '1000ms'";
const SAMPLE_STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '2000ms'";

/// Check the schema shape and a writable session, bounded to 5 s.
///
/// # Errors
///
/// [`StartupError::SchemaMissing`] for SQLSTATE `42P01` or `42703`,
/// [`StartupError::UnsupportedEncoding`] when PostgreSQL is not UTF8,
/// [`StartupError::NotWritable`] when `writable` is false, and
/// [`StartupError::Unavailable`] for anything else, including the bound.
pub(crate) async fn check_startup(shared: &Shared) -> Result<(), StartupError> {
    let check = in_tx_with(
        &shared.pool,
        READ_COMMITTED,
        async |tx| -> Result<(), Refused> {
            let conn = connection(tx);
            let encoding: String = sqlx::query_scalar("SELECT current_setting('server_encoding')")
                .fetch_one(&mut *conn)
                .await
                .map_err(|_| Refused(StartupError::Unavailable))?;
            if encoding != "UTF8" {
                return Err(Refused(StartupError::UnsupportedEncoding));
            }
            let row = sqlx::query(STARTUP_CHECK)
                .fetch_one(&mut *conn)
                .await
                .map_err(|err| Refused(schema_refusal(&err)))?;
            let payload_jsonb = row
                .try_get::<bool, _>("payload_jsonb")
                .map_err(|_| Refused(StartupError::Unavailable))?;
            let unique_key_text = row
                .try_get::<bool, _>("unique_key_text")
                .map_err(|_| Refused(StartupError::Unavailable))?;
            let writable = row
                .try_get::<bool, _>("writable")
                .map_err(|_| Refused(StartupError::Unavailable))?;
            if !payload_jsonb || !unique_key_text {
                Err(Refused(StartupError::SchemaMissing))
            } else if !writable {
                Err(Refused(StartupError::NotWritable))
            } else {
                Ok(())
            }
        },
    );
    match tokio::time::timeout(STARTUP_CHECK_BUDGET, check).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(Refused(refusal))) => Err(refusal),
        Err(_elapsed) => Err(StartupError::Unavailable),
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
    let completed = delete_until(shared, RETAIN_COMPLETED, RETAIN_COMPLETED_FOR).await?;
    let failed = delete_until(shared, RETAIN_FAILED, RETAIN_FAILED_FOR).await?;
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
                Err(error) => observe_failure(&shared, Operation::Retention, error),
            }
        }
    }))
    .await;
}

/// One gauge sample every 10 s, the first at once, until `cancel` fires.
pub(crate) async fn run_sampling(shared: Arc<Shared>, cancel: CancellationToken) {
    let _ = Box::pin(cancel.run_until_cancelled(async {
        describe_sampling_metrics();
        let mut last_success_timestamp = 0.0;
        publish_unavailable(&shared, last_success_timestamp);
        let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match sample_once(&shared).await {
                Ok(sample) => {
                    last_success_timestamp = sample.observed_at;
                    publish_sample(&sample);
                    observe_recovery(&shared, Operation::Sample);
                }
                Err(error) => {
                    publish_unavailable(&shared, last_success_timestamp);
                    observe_failure(&shared, Operation::Sample, error);
                }
            }
        }
    }))
    .await;
}

fn describe_sampling_metrics() {
    metrics::describe_gauge!(
        LIVE_JOBS_METRIC,
        "Per-process capped sample of live jobs by registered kind and state."
    );
    metrics::describe_gauge!(
        LIVE_JOBS_CENSORED_METRIC,
        "Whether a per-process live-job sample reached its cap by registered kind and state."
    );
    metrics::describe_gauge!(
        LIVE_JOBS_SAMPLE_CAP_METRIC,
        "Maximum count published by one per-process live-job sample."
    );
    metrics::describe_gauge!(
        OLDEST_AVAILABLE_AGE_METRIC,
        "Age in seconds of the oldest available job in one per-process sample."
    );
    metrics::describe_gauge!(
        OBSERVATION_TIMESTAMP_METRIC,
        "Database Unix timestamp of the last successful jobs observation."
    );
    metrics::describe_gauge!(
        OBSERVATION_SUCCESS_METRIC,
        "Whether the current jobs observation gauges came from a successful sample."
    );
}

async fn delete_until(
    shared: &Shared,
    statement: &'static str,
    age: Duration,
) -> Result<u64, OperationError> {
    let mut removed = 0u64;
    let limit = u64::try_from(RETENTION_BATCH_ROWS).unwrap_or(0);
    loop {
        let batch = delete_batch(shared, statement, age).await?;
        removed = removed.saturating_add(batch);
        if batch < limit {
            return Ok(removed);
        }
    }
}

async fn delete_batch(
    shared: &Shared,
    statement: &'static str,
    age: Duration,
) -> Result<u64, OperationError> {
    let Ok(_permit) = shared.permit.acquire().await else {
        return Err(OperationError::Acquire);
    };
    let returned = AtomicBool::new(false);
    backstop(
        &returned,
        in_tx_with(
            &shared.pool,
            READ_COMMITTED,
            async |tx| -> Result<u64, OpFailed> {
                let conn = connection(tx);
                sqlx::query(RETENTION_STATEMENT_TIMEOUT)
                    .execute(&mut *conn)
                    .await?;
                let deleted = sqlx::query(statement)
                    .bind(age)
                    .bind(RETENTION_BATCH_ROWS)
                    .execute(&mut *conn)
                    .await?
                    .rows_affected();
                returned.store(true, Ordering::SeqCst);
                Ok(deleted)
            },
        ),
    )
    .await
}

async fn sample_once(shared: &Shared) -> Result<Sample, OperationError> {
    let Ok(_permit) = shared.permit.acquire().await else {
        return Err(OperationError::Acquire);
    };
    let returned = AtomicBool::new(false);
    backstop(
        &returned,
        in_tx_with(
            &shared.pool,
            READ_COMMITTED,
            async |tx| -> Result<Sample, OpFailed> {
                let conn = connection(tx);
                sqlx::query(SAMPLE_STATEMENT_TIMEOUT)
                    .execute(&mut *conn)
                    .await?;
                let kinds: Vec<&str> = shared.registry.names().collect();
                let rows = sqlx::query(SAMPLE)
                    .bind(kinds)
                    .fetch_all(&mut *conn)
                    .await?;
                let decoded = decode_sample(&rows)?;
                returned.store(true, Ordering::SeqCst);
                Ok(decoded)
            },
        ),
    )
    .await
}

struct SampleRow {
    kind: String,
    available: i64,
    scheduled: i64,
    running: i64,
    oldest: f64,
}

struct Sample {
    rows: Vec<SampleRow>,
    observed_at: f64,
}

fn decode_sample(rows: &[sqlx::postgres::PgRow]) -> Result<Sample, OpFailed> {
    let mut decoded = Vec::with_capacity(rows.len());
    let mut observed_at: Option<f64> = None;
    for row in rows {
        let timestamp: f64 = row.try_get("observed_at")?;
        match observed_at {
            Some(existing) if existing.to_bits() != timestamp.to_bits() => {
                return Err(OpFailed(OperationError::Statement));
            }
            Some(_) => {}
            None => observed_at = Some(timestamp),
        }
        decoded.push(SampleRow {
            kind: row.try_get("kind")?,
            available: row.try_get("available")?,
            scheduled: row.try_get("scheduled")?,
            running: row.try_get("running")?,
            oldest: row.try_get("oldest_available_seconds")?,
        });
    }
    match observed_at {
        Some(observed_at) => Ok(Sample {
            rows: decoded,
            observed_at,
        }),
        None => Err(OpFailed(OperationError::Statement)),
    }
}

fn publish_sample(sample: &Sample) {
    metrics::gauge!(LIVE_JOBS_SAMPLE_CAP_METRIC).set(LIVE_JOBS_SAMPLE_CAP_VALUE);
    for row in &sample.rows {
        set_live(&row.kind, "available", row.available);
        set_live(&row.kind, "scheduled", row.scheduled);
        set_live(&row.kind, "running", row.running);
        metrics::gauge!(OLDEST_AVAILABLE_AGE_METRIC, "kind" => row.kind.clone()).set(row.oldest);
    }
    metrics::gauge!(OBSERVATION_TIMESTAMP_METRIC).set(sample.observed_at);
    metrics::gauge!(OBSERVATION_SUCCESS_METRIC).set(1.0);
}

fn publish_unavailable(shared: &Shared, last_success_timestamp: f64) {
    metrics::gauge!(LIVE_JOBS_SAMPLE_CAP_METRIC).set(LIVE_JOBS_SAMPLE_CAP_VALUE);
    metrics::gauge!(OBSERVATION_TIMESTAMP_METRIC).set(last_success_timestamp);
    metrics::gauge!(OBSERVATION_SUCCESS_METRIC).set(0.0);
    for kind in shared.registry.names() {
        set_live_unavailable(kind, "available");
        set_live_unavailable(kind, "scheduled");
        set_live_unavailable(kind, "running");
        metrics::gauge!(OLDEST_AVAILABLE_AGE_METRIC, "kind" => kind).set(f64::NAN);
    }
}

fn set_live(kind: impl Into<String>, state: &'static str, count: i64) {
    let kind = kind.into();
    let censored = count > LIVE_JOBS_SAMPLE_CAP;
    #[allow(clippy::cast_precision_loss)]
    let published = count.min(LIVE_JOBS_SAMPLE_CAP) as f64;
    metrics::gauge!(LIVE_JOBS_METRIC, "kind" => kind.clone(), "state" => state).set(published);
    metrics::gauge!(LIVE_JOBS_CENSORED_METRIC, "kind" => kind, "state" => state).set(if censored {
        1.0
    } else {
        0.0
    });
}

fn set_live_unavailable(kind: impl Into<String>, state: &'static str) {
    let kind = kind.into();
    metrics::gauge!(LIVE_JOBS_METRIC, "kind" => kind.clone(), "state" => state).set(f64::NAN);
    metrics::gauge!(LIVE_JOBS_CENSORED_METRIC, "kind" => kind, "state" => state).set(f64::NAN);
}

struct Refused(StartupError);

impl From<TxError> for Refused {
    fn from(_err: TxError) -> Self {
        Self(StartupError::Unavailable)
    }
}

fn schema_refusal(err: &sqlx::Error) -> StartupError {
    match sqlstate(err).as_deref() {
        Some("42P01" | "42703") => StartupError::SchemaMissing,
        _ => StartupError::Unavailable,
    }
}

fn sqlstate(err: &sqlx::Error) -> Option<Cow<'_, str>> {
    err.as_database_error()?.code()
}
