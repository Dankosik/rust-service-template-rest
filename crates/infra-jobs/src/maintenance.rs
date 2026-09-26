//! The startup check, retention, and the live-job gauges.

use std::sync::Arc;
use std::time::Duration;

use infra_postgres::{TxError, connection, in_tx};
use sqlx::Row;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::engine::{
    Operation, OperationError, Shared, StartupError, backstop, observe_failure, observe_recovery,
};

/// Gauge of live jobs. Labels `kind`, `state` (`available`, `scheduled`, `running`).
pub const LIVE_JOBS_METRIC: &str = "jobs_live_jobs";
/// Age of the oldest available job. Label `kind`. Zero when none.
pub const OLDEST_AVAILABLE_AGE_METRIC: &str = "jobs_oldest_available_age_seconds";
/// Database timestamp of the last successful observation.
pub const OBSERVATION_TIMESTAMP_METRIC: &str = "jobs_observation_timestamp_seconds";
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
/// Bound on the startup check, from acquire through the session query.
pub const STARTUP_CHECK_BUDGET: Duration = Duration::from_secs(5);
/// Whether the current session has the worker's required defaults. Migration-history
/// admission owns schema compatibility; this check keeps only live session properties.
const STARTUP_CHECK: &str = "SELECT current_setting('server_encoding') AS server_encoding, \
     NOT pg_is_in_recovery() AND current_setting('transaction_read_only') = 'off' AS writable, \
     current_setting('default_transaction_isolation') = 'read committed' AS read_committed";

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
            LIMIT 1000 \
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
            LIMIT 1000 \
         ) AS capped \
     ) AS scheduled \
     CROSS JOIN LATERAL ( \
         SELECT count(*) AS count \
         FROM ( \
             SELECT 1 \
             FROM background_jobs AS job \
             WHERE job.kind = registered.kind \
               AND job.state = 'running' \
            LIMIT 1000 \
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

// `SET LOCAL` lasts until the transaction ends, so PostgreSQL restores the
// session timeout itself on commit, rollback, or a dropped connection.
const RETENTION_STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '1000ms'";
const SAMPLE_STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '2000ms'";

/// Check UTF8 server encoding and a writable session, bounded to 5 s.
///
/// # Errors
///
/// [`StartupError::UnsupportedEncoding`] when PostgreSQL is not UTF8,
/// [`StartupError::NotWritable`] when `writable` is false, and
/// [`StartupError::UnsupportedIsolation`] when the pool default is not read
/// committed, and [`StartupError::Unavailable`] for anything else, including
/// the bound.
pub(crate) async fn check_startup(shared: &Shared) -> Result<(), StartupError> {
    let check = async {
        let mut connection = shared
            .pool
            .acquire()
            .await
            .map_err(|_| StartupError::Unavailable)?;
        let row = sqlx::query(STARTUP_CHECK)
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| StartupError::Unavailable)?;
        let encoding: String = row
            .try_get("server_encoding")
            .map_err(|_| StartupError::Unavailable)?;
        if encoding != "UTF8" {
            return Err(StartupError::UnsupportedEncoding);
        }
        let writable: bool = row
            .try_get("writable")
            .map_err(|_| StartupError::Unavailable)?;
        if !writable {
            return Err(StartupError::NotWritable);
        }
        let read_committed: bool = row
            .try_get("read_committed")
            .map_err(|_| StartupError::Unavailable)?;
        if !read_committed {
            return Err(StartupError::UnsupportedIsolation);
        }
        Ok(())
    };
    match tokio::time::timeout(STARTUP_CHECK_BUDGET, check).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(refusal)) => Err(refusal),
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
                    observe_failure(&shared, Operation::Sample, error);
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
    backstop(async {
        in_tx(&shared.pool, async |tx| -> Result<u64, Failed> {
            let conn = connection(tx);
            sqlx::query(RETENTION_STATEMENT_TIMEOUT)
                .execute(&mut *conn)
                .await?;
            Ok(sqlx::query(statement)
                .bind(age)
                .bind(RETENTION_BATCH_ROWS)
                .execute(&mut *conn)
                .await?
                .rows_affected())
        })
        .await
        .map_err(|Failed(error)| error)
    })
    .await
}

async fn sample_once(shared: &Shared) -> Result<Sample, OperationError> {
    let Ok(_permit) = shared.permit.acquire().await else {
        return Err(OperationError::Acquire);
    };
    let kinds: Vec<&str> = shared.registry.names().collect();
    backstop(async {
        in_tx(&shared.pool, async |tx| -> Result<Sample, Failed> {
            let conn = connection(tx);
            sqlx::query(SAMPLE_STATEMENT_TIMEOUT)
                .execute(&mut *conn)
                .await?;
            let rows = sqlx::query(SAMPLE)
                .bind(kinds)
                .fetch_all(&mut *conn)
                .await?;
            decode_sample(&rows).map_err(Failed)
        })
        .await
        .map_err(|Failed(error)| error)
    })
    .await
}

/// The error of a maintenance transaction closure.
struct Failed(OperationError);

impl From<TxError> for Failed {
    fn from(error: TxError) -> Self {
        Self(match error {
            TxError::Acquire(_) => OperationError::Acquire,
            TxError::Begin(_) | TxError::CommitFailed(_) | TxError::CommitUnknown(_) => {
                OperationError::Statement
            }
        })
    }
}

impl From<sqlx::Error> for Failed {
    fn from(_error: sqlx::Error) -> Self {
        Self(OperationError::Statement)
    }
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

fn decode_sample(rows: &[sqlx::postgres::PgRow]) -> Result<Sample, OperationError> {
    let mut decoded = Vec::with_capacity(rows.len());
    let mut observed_at: Option<f64> = None;
    for row in rows {
        let timestamp: f64 = row
            .try_get("observed_at")
            .map_err(|_| OperationError::Statement)?;
        match observed_at {
            Some(existing) if existing.to_bits() != timestamp.to_bits() => {
                return Err(OperationError::Statement);
            }
            Some(_) => {}
            None => observed_at = Some(timestamp),
        }
        decoded.push(SampleRow {
            kind: row.try_get("kind").map_err(|_| OperationError::Statement)?,
            available: row
                .try_get("available")
                .map_err(|_| OperationError::Statement)?,
            scheduled: row
                .try_get("scheduled")
                .map_err(|_| OperationError::Statement)?,
            running: row
                .try_get("running")
                .map_err(|_| OperationError::Statement)?,
            oldest: row
                .try_get("oldest_available_seconds")
                .map_err(|_| OperationError::Statement)?,
        });
    }
    match observed_at {
        Some(observed_at) => Ok(Sample {
            rows: decoded,
            observed_at,
        }),
        None => Err(OperationError::Statement),
    }
}

fn publish_sample(sample: &Sample) {
    for row in &sample.rows {
        set_live(&row.kind, "available", row.available);
        set_live(&row.kind, "scheduled", row.scheduled);
        set_live(&row.kind, "running", row.running);
        metrics::gauge!(OLDEST_AVAILABLE_AGE_METRIC, "kind" => row.kind.clone()).set(row.oldest);
    }
    metrics::gauge!(OBSERVATION_TIMESTAMP_METRIC).set(sample.observed_at);
}

fn set_live(kind: impl Into<String>, state: &'static str, count: i64) {
    let kind = kind.into();
    #[allow(clippy::cast_precision_loss)]
    let value = count as f64;
    metrics::gauge!(LIVE_JOBS_METRIC, "kind" => kind, "state" => state).set(value);
}
