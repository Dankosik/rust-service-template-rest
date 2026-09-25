//! The startup check, retention, and the live-job gauges.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use infra_postgres::{TxError, in_tx_with};
use sqlx::Row;
use sqlx::postgres::PgConnection;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::engine::{
    OpFailed, Operation, OperationError, READ_COMMITTED, Shared, StartupError, backstop,
    observe_failure, observe_recovery,
};

/// Gauge of live jobs. Labels `kind`, `state` (`available`, `scheduled`, `running`).
pub const LIVE_JOBS_METRIC: &str = "jobs_live_jobs";
/// Age of the oldest available job. Label `kind`. Zero when none.
pub const OLDEST_AVAILABLE_AGE_METRIC: &str = "jobs_oldest_available_age_seconds";
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
/// Bound on the startup check, from acquire to the transaction's end.
pub const STARTUP_CHECK_BUDGET: Duration = Duration::from_secs(5);
/// Gauge label for every kind this worker does not register.
pub const UNREGISTERED_KIND: &str = "<unregistered>";

/// The writer check and the table's shape. A missing table or column fails the statement.
const STARTUP_CHECK: &str = "SELECT NOT pg_is_in_recovery() AND current_setting('transaction_read_only') = 'off' AS writable, \
     (SELECT count(*) FROM (SELECT id, kind, payload, unique_key, state, failure_reason, attempts, \
          claim_generation, not_before, claim_expires_at, finished_at, error_summary, trace_context \
      FROM background_jobs LIMIT 0) AS shape) AS shape_rows";

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

const SAMPLE: &str = "SELECT kind, \
            count(*) FILTER (WHERE not_before <= statement_timestamp()) AS available, \
            count(*) FILTER (WHERE not_before > statement_timestamp()) AS scheduled, \
            0::bigint AS running, \
            COALESCE(EXTRACT(EPOCH FROM statement_timestamp() - min(not_before) \
                FILTER (WHERE not_before <= statement_timestamp())), 0)::double precision \
                AS oldest_available_seconds \
     FROM background_jobs \
     WHERE state = 'pending' \
     GROUP BY kind \
     UNION ALL \
     SELECT kind, 0, 0, count(*), 0 \
     FROM background_jobs \
     WHERE state = 'running' \
     GROUP BY kind";

const RETENTION_STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '1000ms'";
const SAMPLE_STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '2000ms'";

/// Check the schema shape and a writable session, bounded to 5 s.
///
/// # Errors
///
/// [`StartupError::SchemaMissing`] for SQLSTATE `42P01` or `42703`,
/// [`StartupError::NotWritable`] when `writable` is false, and
/// [`StartupError::Unavailable`] for anything else, including the bound.
pub(crate) async fn check_startup(shared: &Shared) -> Result<(), StartupError> {
    let check = in_tx_with(
        &shared.pool,
        READ_COMMITTED,
        async |conn: &mut PgConnection| -> Result<(), Refused> {
            let row = sqlx::query(STARTUP_CHECK)
                .fetch_one(&mut *conn)
                .await
                .map_err(|err| Refused(schema_refusal(&err)))?;
            match row.try_get::<bool, _>("writable") {
                Ok(true) => Ok(()),
                Ok(false) => Err(Refused(StartupError::NotWritable)),
                Err(_) => Err(Refused(StartupError::Unavailable)),
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
        let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match sample_once(&shared).await {
                Ok(rows) => {
                    let stats = aggregate(&shared.registry, &rows);
                    publish(&shared, &stats);
                    observe_recovery(&shared, Operation::Sample);
                }
                Err(error) => observe_failure(&shared, Operation::Sample, error),
            }
        }
    }))
    .await;
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
            async |conn| -> Result<u64, OpFailed> {
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

async fn sample_once(shared: &Shared) -> Result<Vec<SampleRow>, OperationError> {
    let Ok(_permit) = shared.permit.acquire().await else {
        return Err(OperationError::Acquire);
    };
    let returned = AtomicBool::new(false);
    backstop(
        &returned,
        in_tx_with(
            &shared.pool,
            READ_COMMITTED,
            async |conn| -> Result<Vec<SampleRow>, OpFailed> {
                sqlx::query(SAMPLE_STATEMENT_TIMEOUT)
                    .execute(&mut *conn)
                    .await?;
                let rows = sqlx::query(SAMPLE).fetch_all(&mut *conn).await?;
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

fn decode_sample(rows: &[sqlx::postgres::PgRow]) -> Result<Vec<SampleRow>, OpFailed> {
    let mut decoded = Vec::with_capacity(rows.len());
    for row in rows {
        decoded.push(SampleRow {
            kind: row.try_get("kind")?,
            available: row.try_get("available")?,
            scheduled: row.try_get("scheduled")?,
            running: row.try_get("running")?,
            oldest: row.try_get("oldest_available_seconds")?,
        });
    }
    Ok(decoded)
}

#[derive(Clone, Copy, Default)]
struct Counts {
    available: i64,
    scheduled: i64,
    running: i64,
    oldest: f64,
}

fn aggregate(registry: &crate::Registry, rows: &[SampleRow]) -> HashMap<&'static str, Counts> {
    let mut stats: HashMap<&'static str, Counts> = HashMap::new();
    for row in rows {
        let label = registry
            .get(&row.kind)
            .map_or(UNREGISTERED_KIND, |registered| registered.name);
        let entry = stats.entry(label).or_default();
        entry.available = entry.available.saturating_add(row.available);
        entry.scheduled = entry.scheduled.saturating_add(row.scheduled);
        entry.running = entry.running.saturating_add(row.running);
        if row.oldest > entry.oldest {
            entry.oldest = row.oldest;
        }
    }
    stats
}

fn publish(shared: &Shared, stats: &HashMap<&'static str, Counts>) {
    for label in shared
        .registry
        .names()
        .chain(std::iter::once(UNREGISTERED_KIND))
    {
        let counts = stats.get(label).copied().unwrap_or_default();
        set_live(label, "available", counts.available);
        set_live(label, "scheduled", counts.scheduled);
        set_live(label, "running", counts.running);
        metrics::gauge!(OLDEST_AVAILABLE_AGE_METRIC, "kind" => label).set(counts.oldest);
    }
}

fn set_live(kind: &'static str, state: &'static str, count: i64) {
    #[allow(clippy::cast_precision_loss)]
    let value = count as f64;
    metrics::gauge!(LIVE_JOBS_METRIC, "kind" => kind, "state" => state).set(value);
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
