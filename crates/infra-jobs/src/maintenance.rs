//! Completed retention and registered-job gauges.

use std::sync::Arc;
use std::time::Duration;

use infra_postgres::{
    Cleanup, CleanupPass, CleanupSchedule, MAINTENANCE_OBSERVATION_INTERVAL, MaintenanceFailure,
    MaintenanceObserver, MaintenancePopulation, PopulationSample, in_observation_tx, in_tx,
    observed,
};
use metrics::Unit;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::engine::{
    Operation, OperationError, Shared, backstop, observe_failure, observe_recovery,
};
use infra_postgres::set_statement_timeout;

/// Gauge of live jobs. Labels `kind`, `state` (`available`, `scheduled`, `running`).
const LIVE_JOBS_METRIC: &str = "jobs_live_jobs";
/// Gauge of retained failed jobs. Label `kind`.
pub(crate) const FAILED_JOBS_METRIC: &str = "jobs_failed_jobs";
/// Age of the oldest available job. Label `kind`. Zero when none.
const OLDEST_AVAILABLE_AGE_METRIC: &str = "jobs_oldest_available_age_seconds";
/// Database timestamp of the last successful observation.
pub(crate) const OBSERVATION_TIMESTAMP_METRIC: &str = "jobs_observation_timestamp_seconds";
const RETAIN_COMPLETED_FOR: Duration = Duration::from_hours(24);
const RETENTION_BATCH_ROWS: u32 = 500;
/// The hang guard on one retention batch: a slow batch is waited for.
const RETENTION_STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);
/// How long a population read waits for the engine's statement turn.
const POPULATION_TURN_WAIT: Duration = Duration::from_secs(1);
const PER_KIND_STATE_SAMPLE_CAP: i64 = 1_000;
/// The statement bound of one registered-kind sample.
const SAMPLE_STATEMENT_TIMEOUT: Duration = Duration::from_secs(2);

/// Delete expired completed jobs in batches of [`RETENTION_BATCH_ROWS`] and
/// return how many were deleted.
///
/// Batches run until one is short. The first batch error is returned; earlier
/// batches stay committed. Failed jobs remain until recovery. A short batch
/// does not prove empty inventory: locked eligible rows can remain.
///
/// # Errors
///
/// [`OperationError`] from the batch that failed.
pub(crate) async fn remove_expired(shared: &Shared) -> Result<u64, OperationError> {
    remove_expired_with_cancel(shared, None)
        .await
        .map(|(removed, _)| removed)
}

/// How a retention pass ended; [`CleanupPass`] records why a pass finished.
enum PassEnd {
    Finished,
    Cancelled,
}

async fn remove_expired_with_cancel(
    shared: &Shared,
    cancel: Option<&CancellationToken>,
) -> Result<(u64, PassEnd), OperationError> {
    let mut pass = CleanupPass::start(Cleanup::Jobs);
    let mut removed = 0u64;
    loop {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Ok((removed, PassEnd::Cancelled));
        }
        let batch = Box::pin(delete_batch(shared))
            .await
            .inspect_err(|_| pass.failed())?;
        pass.committed(batch);
        removed = removed.saturating_add(batch);
        if batch < u64::from(RETENTION_BATCH_ROWS) {
            pass.completed();
            return Ok((removed, PassEnd::Finished));
        }
    }
}

/// Run retention under the selected schedule until `cancel` fires.
pub(crate) async fn run_retention(shared: Arc<Shared>, cancel: CancellationToken) {
    let _ = Box::pin(cancel.run_until_cancelled(async {
        let mut schedule = CleanupSchedule::new(Cleanup::Jobs);
        while !cancel.is_cancelled() {
            schedule.next().await;
            if cancel.is_cancelled() {
                return;
            }
            match remove_expired_with_cancel(&shared, Some(&cancel)).await {
                Ok((_, PassEnd::Cancelled)) => return,
                Ok((_, PassEnd::Finished)) => observe_recovery(&shared, Operation::Retention),
                Err(error) => observe_failure(&shared, Operation::Retention, &error),
            }
        }
    }))
    .await;
}

/// Independently poll registered-kind and global maintenance samples under
/// the existing once-per-process owner, until `cancel` fires.
pub(crate) async fn run_sampling(shared: Arc<Shared>, cancel: CancellationToken) {
    let _ = Box::pin(cancel.run_until_cancelled(async {
        tokio::join!(
            run_kind_sampling(&shared, &cancel),
            run_population_sampling(&shared, &cancel),
        );
    }))
    .await;
}

async fn run_kind_sampling(shared: &Shared, cancel: &CancellationToken) {
    let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    while !cancel.is_cancelled() {
        ticker.tick().await;
        if cancel.is_cancelled() {
            return;
        }
        match sample_once(shared).await {
            Ok(Some(sample)) => {
                if shared
                    .peers
                    .publish_if_current(&sample.revision, || publish_sample(&sample))
                {
                    observe_recovery(shared, Operation::Sample);
                }
            }
            Ok(None) => {}
            Err(error) => {
                observe_failure(shared, Operation::Sample, &error);
            }
        }
    }
}

async fn run_population_sampling(shared: &Shared, cancel: &CancellationToken) {
    let mut observers = [Population::Completed, Population::Failed].map(|population| {
        (
            population,
            MaintenanceObserver::start(population.observer()),
        )
    });
    while !cancel.is_cancelled() {
        for (population, observer) in &mut observers {
            observer.sample(read_population(shared, *population)).await;
            if cancel.is_cancelled() {
                return;
            }
        }
        tokio::time::sleep(MAINTENANCE_OBSERVATION_INTERVAL).await;
    }
}

#[derive(Clone, Copy)]
enum Population {
    /// Completed jobs past retention; `oldest` is when the oldest one expired.
    Completed,
    /// Retained failed jobs; `oldest` is when the oldest one failed.
    Failed,
}

impl Population {
    const fn observer(self) -> MaintenancePopulation {
        match self {
            Self::Completed => MaintenancePopulation::Cleanup(Cleanup::Jobs),
            Self::Failed => MaintenancePopulation::FailedJobs,
        }
    }
}

async fn read_population(
    shared: &Shared,
    population: Population,
) -> Result<PopulationSample, MaintenanceFailure> {
    let _turn = tokio::time::timeout(POPULATION_TURN_WAIT, shared.statement_turn.lock())
        .await
        .map_err(|_| MaintenanceFailure::Permit)?;
    in_observation_tx(&shared.pool, async |tx| {
        Ok(match population {
            Population::Failed => observed(
                "observe failed jobs population",
                sqlx::query_as!(
                    PopulationSample,
                    "WITH sampled AS (SELECT statement_timestamp() AS observed_at) \
                     SELECT EXTRACT(EPOCH FROM sampled.observed_at)::double precision AS \"observed_at!\", \
                            EXTRACT(EPOCH FROM oldest.finished_at)::double precision AS \"oldest?\" \
                     FROM sampled LEFT JOIN LATERAL ( \
                         SELECT finished_at FROM background_jobs \
                         WHERE state = 'failed' \
                         ORDER BY finished_at LIMIT 1 \
                     ) AS oldest ON true"
                )
                .fetch_one(&mut *tx),
            )
            .await?,
            Population::Completed => observed(
                "observe completed jobs population",
                sqlx::query_as!(
                    PopulationSample,
                    "WITH sampled AS (SELECT statement_timestamp() AS observed_at) \
                     SELECT EXTRACT(EPOCH FROM sampled.observed_at)::double precision AS \"observed_at!\", \
                            EXTRACT(EPOCH FROM oldest.finished_at + $1::interval)::double precision AS \"oldest?\" \
                     FROM sampled LEFT JOIN LATERAL ( \
                         SELECT finished_at FROM background_jobs \
                         WHERE state = 'completed' AND finished_at <= sampled.observed_at - $1::interval \
                         ORDER BY finished_at LIMIT 1 \
                     ) AS oldest ON true",
                    RETAIN_COMPLETED_FOR as _,
                )
                .fetch_one(&mut *tx),
            )
            .await?,
        })
    })
    .await
}

/// Describe the queue-observation gauges and publish neutral pre-sample values.
/// [`Engine::start`](crate::Engine::start) calls this for the engine that owns
/// the process duties.
pub(crate) fn init_metrics(kinds: &[&'static str]) {
    metrics::describe_gauge!(
        LIVE_JOBS_METRIC,
        Unit::Count,
        "Per-process capped depth of live jobs by registered kind and state."
    );
    metrics::describe_gauge!(
        FAILED_JOBS_METRIC,
        Unit::Count,
        "Per-process capped depth of retained failed jobs by registered kind."
    );
    metrics::describe_gauge!(
        OLDEST_AVAILABLE_AGE_METRIC,
        Unit::Seconds,
        "Age in seconds of the oldest available job in one per-process sample."
    );
    metrics::describe_gauge!(
        OBSERVATION_TIMESTAMP_METRIC,
        Unit::Seconds,
        "Database Unix timestamp of the last successful jobs observation."
    );
    for &kind in kinds {
        reset_kind_metrics(kind);
    }
    invalidate_sample();
}

pub(crate) fn reset_kind_metrics(kind: &'static str) {
    publish_kind(kind, KindDepths::default());
}

pub(crate) fn invalidate_sample() {
    metrics::gauge!(OBSERVATION_TIMESTAMP_METRIC).set(0.0);
}

async fn delete_batch(shared: &Shared) -> Result<u64, OperationError> {
    let _turn = shared.statement_turn.lock().await;
    backstop(async {
        in_tx(&shared.pool, async |tx| -> Result<u64, OperationError> {
            set_statement_timeout(tx, RETENTION_STATEMENT_TIMEOUT).await?;
            // The state stays a literal: a bound state cannot prove the partial
            // `background_jobs_terminal` predicate, so a generic plan would
            // scan the table.
            let delete = sqlx::query!(
                "DELETE FROM background_jobs \
                 WHERE id = ANY (ARRAY( \
                     SELECT id FROM background_jobs \
                     WHERE state = 'completed' AND finished_at <= statement_timestamp() - $1::interval \
                     ORDER BY finished_at \
                     LIMIT $2 \
                     FOR UPDATE SKIP LOCKED))",
                RETAIN_COMPLETED_FOR as _,
                i64::from(RETENTION_BATCH_ROWS),
            );
            Ok(observed("delete finished jobs", delete.execute(&mut *tx))
                .await?
                .rows_affected())
        })
        .await
    })
    .await
}

async fn sample_once(shared: &Shared) -> Result<Option<Sample>, OperationError> {
    let _turn = shared.statement_turn.lock().await;
    let (kinds, revision) = shared.peers.sampling_kinds();
    if kinds.is_empty() {
        return Ok(None);
    }
    backstop(async {
        in_tx(&shared.pool, async |tx| -> Result<Sample, OperationError> {
            set_statement_timeout(tx, SAMPLE_STATEMENT_TIMEOUT).await?;
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
                            failed.count AS \"failed!\", \
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
                     CROSS JOIN LATERAL ( \
                         SELECT count(*) AS count \
                         FROM ( \
                             SELECT 1 \
                             FROM background_jobs AS job \
                             WHERE job.kind = registered.kind \
                               AND job.state = 'failed' \
                            LIMIT $2 \
                         ) AS capped \
                     ) AS failed \
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
                    PER_KIND_STATE_SAMPLE_CAP,
                )
                .fetch_all(&mut *tx),
            )
            .await?;
            decode_sample(rows, revision)
        })
        .await
    })
    .await
    .map(Some)
}

struct SampleRow {
    kind: String,
    available: i64,
    scheduled: i64,
    running: i64,
    failed: i64,
    oldest_available_seconds: f64,
    observed_at: f64,
}

impl SampleRow {
    const fn depths(&self) -> KindDepths {
        KindDepths {
            available: self.available,
            scheduled: self.scheduled,
            running: self.running,
            failed: self.failed,
            oldest_available_seconds: self.oldest_available_seconds,
        }
    }
}

/// One kind's sampled queue depths, as its gauges publish them.
#[derive(Clone, Copy, Default)]
struct KindDepths {
    available: i64,
    scheduled: i64,
    running: i64,
    failed: i64,
    oldest_available_seconds: f64,
}

struct Sample {
    revision: Arc<()>,
    rows: Vec<SampleRow>,
    observed_at: f64,
}

fn decode_sample(rows: Vec<SampleRow>, revision: Arc<()>) -> Result<Sample, OperationError> {
    let Some(observed_at) = rows.first().map(|row| row.observed_at) else {
        return Err(OperationError::Statement(sqlx::Error::Decode(
            "jobs sample returned no rows".into(),
        )));
    };
    Ok(Sample {
        revision,
        rows,
        observed_at,
    })
}

fn publish_sample(sample: &Sample) {
    for row in &sample.rows {
        publish_kind(&row.kind, row.depths());
    }
    metrics::gauge!(OBSERVATION_TIMESTAMP_METRIC).set(sample.observed_at);
}

/// Every per-kind series a sample sets, the one list registration zeroes too.
fn publish_kind(kind: &str, depths: KindDepths) {
    for (state, count) in [
        ("available", depths.available),
        ("scheduled", depths.scheduled),
        ("running", depths.running),
    ] {
        metrics::gauge!(LIVE_JOBS_METRIC, "kind" => kind.to_owned(), "state" => state)
            .set(gauge_count(count));
    }
    metrics::gauge!(FAILED_JOBS_METRIC, "kind" => kind.to_owned()).set(gauge_count(depths.failed));
    metrics::gauge!(OLDEST_AVAILABLE_AGE_METRIC, "kind" => kind.to_owned())
        .set(depths.oldest_available_seconds);
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a metric gauge is f64; counts stay far below 2^52"
)]
fn gauge_count(count: i64) -> f64 {
    count as f64
}
