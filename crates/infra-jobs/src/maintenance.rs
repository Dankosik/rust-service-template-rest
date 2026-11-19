use std::time::Duration;

use enum_map::{Enum, EnumMap};
use infra_postgres::{
    Cleanup, CleanupPass, CleanupSchedule, MAINTENANCE_OBSERVATION_INTERVAL, MaintenanceFailure,
    MaintenanceObserver, MaintenancePopulation, PopulationSample, Tx, in_observation_tx, in_tx,
    observed, set_statement_timeout,
};
use metrics::Unit;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::engine::{
    Operation, OperationError, WorkerGroup, backstop, observe_failure, observe_recovery,
};

// template:begin outbox:jobs-maintenance-outbox-module
mod outbox;
// template:end outbox:jobs-maintenance-outbox-module

const LIVE_JOBS_METRIC: &str = "jobs_live_jobs";
pub(crate) const FAILED_JOBS_METRIC: &str = "jobs_failed_jobs";
const OLDEST_AVAILABLE_AGE_METRIC: &str = "jobs_oldest_available_age_seconds";
pub(crate) const OBSERVATION_TIMESTAMP_METRIC: &str = "jobs_observation_timestamp_seconds";
const RETAIN_COMPLETED_FOR: Duration = Duration::from_hours(24);
const RETENTION_BATCH_ROWS: u32 = 500;
const RETENTION_STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);
const POPULATION_TURN_WAIT: Duration = Duration::from_secs(1);
const PER_KIND_STATE_SAMPLE_CAP: i64 = 1_000;
const SAMPLE_STATEMENT_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) async fn remove_expired(primary_group: &WorkerGroup) -> Result<u64, OperationError> {
    let mut pass = CleanupPass::start(Cleanup::Jobs);
    let mut removed = 0u64;
    loop {
        let batch = Box::pin(delete_batch(primary_group))
            .await
            .inspect_err(|_| pass.failed())?;
        pass.committed(batch);
        removed = removed.saturating_add(batch);
        if batch < u64::from(RETENTION_BATCH_ROWS) {
            pass.completed();
            return Ok(removed);
        }
    }
}

pub(crate) async fn run_retention(primary_group: &WorkerGroup, cancel: CancellationToken) {
    let _ = Box::pin(cancel.run_until_cancelled(async {
        let mut schedule = CleanupSchedule::new(Cleanup::Jobs);
        loop {
            schedule.next().await;
            match remove_expired(primary_group).await {
                Ok(_) => observe_recovery(primary_group, Operation::Retention),
                Err(error) => observe_failure(primary_group, Operation::Retention, &error),
            }
        }
    }))
    .await;
}

async fn in_maintenance_tx<T, F>(
    primary_group: &WorkerGroup,
    timeout: Duration,
    work: F,
) -> Result<T, OperationError>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, OperationError>,
{
    let _turn = primary_group.statement_turn.lock().await;
    backstop(async {
        in_tx(&primary_group.pool, async |tx| {
            set_statement_timeout(tx, timeout).await?;
            work(tx).await
        })
        .await
    })
    .await
}

async fn delete_batch(primary_group: &WorkerGroup) -> Result<u64, OperationError> {
    in_maintenance_tx(primary_group, RETENTION_STATEMENT_TIMEOUT, async |tx| {
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
}

pub(crate) async fn run_sampling(
    primary_group: &WorkerGroup,
    kinds: &[&'static str],
    gauges: SampleGauges,
    cancel: CancellationToken,
) {
    let reset = std::mem::DropGuard::new(gauges, |gauges| {
        gauges.reset();
        // template:begin outbox:jobs-maintenance-outbox-reset
        if outbox::is_registered(kinds) {
            outbox::reset_metrics();
        }
        // template:end outbox:jobs-maintenance-outbox-reset
    });
    let _ = Box::pin(cancel.run_until_cancelled(async {
        tokio::join!(
            run_kind_sampling(primary_group, kinds, &reset),
            run_population_sampling(primary_group),
        );
    }))
    .await;
}

async fn run_kind_sampling(
    primary_group: &WorkerGroup,
    kinds: &[&str],
    gauges: &SampleGauges,
) -> ! {
    let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        // template:begin outbox:jobs-maintenance-outbox-sampler
        if outbox::is_registered(kinds) {
            outbox::sample(primary_group).await;
        }
        // template:end outbox:jobs-maintenance-outbox-sampler
        match sample_once(primary_group, kinds).await {
            Ok(sample) => {
                gauges.publish(&sample);
                observe_recovery(primary_group, Operation::Sample);
            }
            Err(error) => {
                observe_failure(primary_group, Operation::Sample, &error);
            }
        }
    }
}

async fn run_population_sampling(primary_group: &WorkerGroup) -> ! {
    let mut observers = [Population::ExpiredCompleted, Population::Failed].map(|population| {
        (
            population,
            MaintenanceObserver::start(population.maintenance_population()),
        )
    });
    loop {
        for (population, observer) in &mut observers {
            observer
                .sample(read_population(primary_group, *population))
                .await;
        }
        tokio::time::sleep(MAINTENANCE_OBSERVATION_INTERVAL).await;
    }
}

#[derive(Clone, Copy)]
enum Population {
    ExpiredCompleted,
    Failed,
}

impl Population {
    const fn maintenance_population(self) -> MaintenancePopulation {
        match self {
            Self::ExpiredCompleted => MaintenancePopulation::Cleanup(Cleanup::Jobs),
            Self::Failed => MaintenancePopulation::FailedJobs,
        }
    }
}

async fn read_population(
    primary_group: &WorkerGroup,
    population: Population,
) -> Result<PopulationSample, MaintenanceFailure> {
    let _turn = tokio::time::timeout(POPULATION_TURN_WAIT, primary_group.statement_turn.lock())
        .await
        .map_err(|_| MaintenanceFailure::Permit)?;
    in_observation_tx(&primary_group.pool, async |tx| {
        Ok(match population {
            Population::Failed => observed(
                "observe failed jobs population",
                sqlx::query_as!(
                    PopulationSample,
                    "SELECT EXTRACT(EPOCH FROM statement_timestamp())::double precision AS \"observed_at_unix_seconds!\", \
                            EXTRACT(EPOCH FROM min(finished_at))::double precision AS \"oldest_at_unix_seconds?\" \
                     FROM background_jobs WHERE state = 'failed'"
                )
                .fetch_one(&mut *tx),
            )
            .await?,
            Population::ExpiredCompleted => observed(
                "observe completed jobs population",
                sqlx::query_as!(
                    PopulationSample,
                    "SELECT EXTRACT(EPOCH FROM statement_timestamp())::double precision AS \"observed_at_unix_seconds!\", \
                            EXTRACT(EPOCH FROM min(finished_at) + $1::interval)::double precision AS \"oldest_at_unix_seconds?\" \
                     FROM background_jobs \
                     WHERE state = 'completed' AND finished_at <= statement_timestamp() - $1::interval",
                    RETAIN_COMPLETED_FOR as _,
                )
                .fetch_one(&mut *tx),
            )
            .await?,
        })
    })
    .await
}

pub(crate) fn init_metrics(kinds: &[&'static str]) -> SampleGauges {
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
        "Seconds since the oldest due pending job or expired running lease became available."
    );
    metrics::describe_gauge!(
        OBSERVATION_TIMESTAMP_METRIC,
        Unit::Seconds,
        "Database Unix timestamp of the last successful jobs observation."
    );
    let gauges = SampleGauges::new(kinds);
    gauges.reset();
    // template:begin outbox:jobs-maintenance-outbox-init
    if outbox::is_registered(kinds) {
        outbox::init_metrics();
    }
    // template:end outbox:jobs-maintenance-outbox-init
    gauges
}

async fn sample_once(
    primary_group: &WorkerGroup,
    kinds: &[&str],
) -> Result<Sample, OperationError> {
    in_maintenance_tx(primary_group, SAMPLE_STATEMENT_TIMEOUT, async |tx| {
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
                            COALESCE(EXTRACT(EPOCH FROM sampled.observed_at - LEAST(available.oldest, expired.oldest)), 0)::double precision \
                                AS \"oldest_available_seconds!\", \
                            EXTRACT(EPOCH FROM sampled.observed_at)::double precision AS \"observed_at!\" \
                     FROM sampled \
                     CROSS JOIN registered \
                     CROSS JOIN LATERAL ( \
                         SELECT count(*) AS count, min(capped.not_before) AS oldest \
                         FROM ( \
                             SELECT job.not_before \
                             FROM background_jobs AS job \
                             WHERE job.kind = registered.kind \
                               AND job.state = 'pending' \
                               AND job.not_before <= sampled.observed_at \
                               AND NOT EXISTS (SELECT 1 FROM background_jobs prior \
                                               WHERE prior.id = job.predecessor_job_id AND prior.state <> 'completed') \
                             ORDER BY job.not_before, job.id \
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
                               AND (job.not_before > sampled.observed_at OR EXISTS ( \
                                   SELECT 1 FROM background_jobs prior \
                                   WHERE prior.id = job.predecessor_job_id AND prior.state <> 'completed')) \
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
                         SELECT job.claim_expires_at AS oldest \
                         FROM background_jobs AS job \
                         WHERE job.kind = registered.kind \
                           AND job.state = 'running' \
                           AND job.claim_expires_at <= sampled.observed_at \
                           AND NOT EXISTS (SELECT 1 FROM background_jobs prior \
                                           WHERE prior.id = job.predecessor_job_id AND prior.state <> 'completed') \
                         ORDER BY job.claim_expires_at \
                         LIMIT 1 \
                     ) AS expired ON true \
                     CROSS JOIN LATERAL ( \
                         SELECT count(*) AS count \
                         FROM ( \
                             SELECT 1 \
                             FROM background_jobs AS job \
                             WHERE job.kind = registered.kind \
                               AND job.state = 'failed' \
                            LIMIT $2 \
                         ) AS capped \
                     ) AS failed",
                    kinds as _,
                    PER_KIND_STATE_SAMPLE_CAP,
                )
                .fetch_all(&mut *tx),
            )
            .await?;
        decode_sample(rows)
    })
    .await
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
    const fn queue_metrics(&self) -> KindQueueMetrics {
        KindQueueMetrics {
            available: self.available,
            scheduled: self.scheduled,
            running: self.running,
            failed: self.failed,
            oldest_available_seconds: self.oldest_available_seconds,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct KindQueueMetrics {
    available: i64,
    scheduled: i64,
    running: i64,
    failed: i64,
    oldest_available_seconds: f64,
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

#[derive(Clone, Copy, Enum)]
enum LiveState {
    Available,
    Scheduled,
    Running,
}

impl LiveState {
    const fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Scheduled => "scheduled",
            Self::Running => "running",
        }
    }
}

struct KindGauges {
    live: EnumMap<LiveState, metrics::Gauge>,
    failed: metrics::Gauge,
    oldest_available_age: metrics::Gauge,
}

impl KindGauges {
    fn new(kind: &'static str) -> Self {
        Self {
            live: EnumMap::from_fn(
                |state: LiveState| metrics::gauge!(LIVE_JOBS_METRIC, "kind" => kind, "state" => state.label()),
            ),
            failed: metrics::gauge!(FAILED_JOBS_METRIC, "kind" => kind),
            oldest_available_age: metrics::gauge!(OLDEST_AVAILABLE_AGE_METRIC, "kind" => kind),
        }
    }

    fn publish(&self, queue_metrics: KindQueueMetrics) {
        self.live[LiveState::Available].set(gauge_count(queue_metrics.available));
        self.live[LiveState::Scheduled].set(gauge_count(queue_metrics.scheduled));
        self.live[LiveState::Running].set(gauge_count(queue_metrics.running));
        self.failed.set(gauge_count(queue_metrics.failed));
        self.oldest_available_age
            .set(queue_metrics.oldest_available_seconds);
    }

    fn reset(&self) {
        self.publish(KindQueueMetrics::default());
    }
}

pub(crate) struct SampleGauges {
    by_kind: foldhash::HashMap<&'static str, KindGauges>,
    observation_timestamp: metrics::Gauge,
}

impl SampleGauges {
    fn new(kinds: &[&'static str]) -> Self {
        Self {
            by_kind: kinds
                .iter()
                .map(|&kind| (kind, KindGauges::new(kind)))
                .collect(),
            observation_timestamp: metrics::gauge!(OBSERVATION_TIMESTAMP_METRIC),
        }
    }

    fn publish(&self, sample: &Sample) {
        for row in &sample.rows {
            if let Some(gauges) = self.by_kind.get(row.kind.as_str()) {
                gauges.publish(row.queue_metrics());
            }
        }
        self.observation_timestamp.set(sample.observed_at);
    }

    fn reset(&self) {
        for gauges in self.by_kind.values() {
            gauges.reset();
        }
        self.observation_timestamp.set(0.0);
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a metric gauge is f64; counts stay far below 2^52"
)]
fn gauge_count(count: i64) -> f64 {
    count as f64
}
