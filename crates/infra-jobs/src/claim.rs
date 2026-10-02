//! CLAIM and the claim loop.

use std::sync::Arc;
use std::time::Duration;

use sqlx::FromRow;
use sqlx::postgres::{PgListener, PgPool, PgPoolOptions};
use tokio::sync::{OwnedSemaphorePermit, SemaphorePermit};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::attempt;
use crate::engine::{
    CANCEL_MARGIN, LEASE_RESERVE, Operation, OperationError, POLL_INTERVAL, Shared, backstop,
    observe_failure, observe_recovery,
};
use crate::kind::JobId;

/// Claim request duration through its database acknowledgement. No labels.
pub const CLAIM_DURATION_METRIC: &str = "jobs_claim_duration_seconds";
/// Histogram buckets for [`CLAIM_DURATION_METRIC`], in seconds.
pub const CLAIM_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 8.0, 12.0,
];
/// Queue time between a job's current `not_before` and its acknowledged claim. Label `kind`.
pub const QUEUE_WAIT_METRIC: &str = "jobs_queue_wait_seconds";
/// Histogram buckets for [`QUEUE_WAIT_METRIC`], in seconds.
pub const QUEUE_WAIT_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0,
    1800.0, 3600.0, 10_800.0, 21_600.0, 43_200.0, 86_400.0,
];

/// Claim due pending jobs and expired running jobs, up to the free slots.
///
/// Each per-kind scan locks rows while it reads them (`SKIP LOCKED` inside the
/// lateral), so a row another session holds is skipped and the scan moves on.
/// Choosing ids first and locking them afterwards hands concurrent workers the
/// same few ids: the losers claim nothing until the next poll.
const CLAIM: &str = "WITH policy AS ( \
         SELECT policy.kind, policy.max_attempts, policy.timeout_micros \
         FROM unnest($1::text[], $2::smallint[], $3::bigint[]) \
             AS policy (kind, max_attempts, timeout_micros) \
     ), \
     candidates AS ( \
         SELECT candidate.id, candidate.not_before \
         FROM policy \
         CROSS JOIN LATERAL ( \
             SELECT job.id, job.not_before \
             FROM background_jobs AS job \
             WHERE job.state = 'pending' \
               AND job.kind = policy.kind \
               AND job.not_before <= statement_timestamp() \
             ORDER BY job.not_before, job.id \
             LIMIT $4 \
             FOR UPDATE SKIP LOCKED \
         ) AS candidate \
         UNION ALL \
         SELECT candidate.id, candidate.not_before \
         FROM policy \
         CROSS JOIN LATERAL ( \
             SELECT job.id, job.not_before \
             FROM background_jobs AS job \
             WHERE job.state = 'running' \
               AND job.kind = policy.kind \
               AND job.claim_expires_at <= statement_timestamp() \
             ORDER BY job.not_before, job.id \
             LIMIT $4 \
             FOR UPDATE SKIP LOCKED \
         ) AS candidate \
     ), \
     picked AS ( \
         SELECT candidates.id \
         FROM candidates \
         ORDER BY candidates.not_before, candidates.id \
         LIMIT $4 \
     ) \
     UPDATE background_jobs AS job \
     SET state = CASE WHEN job.attempts >= policy.max_attempts THEN 'failed' ELSE 'running' END, \
         failure_reason = CASE WHEN job.attempts >= policy.max_attempts THEN 'exhausted' END, \
         finished_at = CASE WHEN job.attempts >= policy.max_attempts THEN statement_timestamp() END, \
         claim_expires_at = CASE WHEN job.attempts >= policy.max_attempts THEN NULL \
                                 ELSE statement_timestamp() \
                                      + policy.timeout_micros * interval '1 microsecond' \
                                      + $6::bigint * interval '1 microsecond' END, \
         attempts = CASE WHEN job.attempts >= policy.max_attempts THEN job.attempts \
                         ELSE job.attempts + 1 END, \
         attempted_by = CASE WHEN job.attempts >= policy.max_attempts THEN job.attempted_by \
                             ELSE $5 END, \
         error_summary = CASE WHEN job.state = 'running' \
                                   AND job.claim_expires_at <= statement_timestamp() \
                              THEN 'lease expired; rescued' \
                              WHEN job.attempts >= policy.max_attempts \
                              THEN COALESCE(job.error_summary, 'attempt budget spent') \
                              ELSE job.error_summary END, \
         errors = CASE WHEN job.state = 'running' \
                            AND job.claim_expires_at <= statement_timestamp() \
                       THEN job.errors || jsonb_build_object( \
                                'attempt', job.attempts, 'at', statement_timestamp(), \
                                'error', 'lease expired; rescued') \
                       ELSE job.errors END, \
         claim_generation = nextval('background_jobs_claim_generation') \
     FROM policy \
     WHERE job.id = ANY (ARRAY(SELECT picked.id FROM picked)) \
       AND policy.kind = job.kind \
       AND ((job.state = 'pending' AND job.not_before <= statement_timestamp()) \
            OR (job.state = 'running' AND job.claim_expires_at <= statement_timestamp())) \
     RETURNING job.id, job.kind, (job.state = 'failed') AS exhausted, job.attempts, \
               job.claim_generation, \
               CASE WHEN job.state = 'running' THEN job.payload::text END AS payload, \
               job.trace_context, job.trace_state, job.error_summary, \
               EXTRACT(EPOCH FROM statement_timestamp() - job.not_before)::double precision \
                   AS queue_wait_seconds";

/// A row CLAIM set `running`.
#[derive(Debug)]
pub(crate) struct Claimed {
    pub(crate) id: JobId,
    pub(crate) generation: i64,
    pub(crate) kind: &'static str,
    pub(crate) attempt: u16,
    pub(crate) payload: Vec<u8>,
    pub(crate) trace_context: Option<String>,
    pub(crate) trace_state: Option<String>,
    pub(crate) slot: OwnedSemaphorePermit,
}

/// Describe claim metrics once during engine startup.
pub(crate) fn describe_metrics() {
    metrics::describe_histogram!(
        CLAIM_DURATION_METRIC,
        "Claim request duration through acknowledgement"
    );
    metrics::describe_histogram!(
        QUEUE_WAIT_METRIC,
        "Time from a job's current not-before to its acknowledged claim"
    );
}

/// Claim until `stop` fires. Closes the attempt tracker on every exit.
pub(crate) async fn run_claim_loop(shared: Arc<Shared>, stop: CancellationToken) {
    let _close = CloseTracker(&shared.attempt_tracker);
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut next = Next::Idle;
    let mut last_claim = Instant::now();
    loop {
        match next {
            Next::Now => {}
            Next::Soon => {
                if !cooldown(&stop, last_claim).await {
                    return;
                }
            }
            Next::Idle => {
                if !await_tick(&shared, &stop, &mut ticker).await
                    || !cooldown(&stop, last_claim).await
                {
                    return;
                }
            }
        }
        let Some(mut slots) = take_slots(&shared, &stop).await else {
            return;
        };
        let requested = slots.num_permits();
        let Some(permit) = engine_permit(&shared, &stop).await else {
            return;
        };
        if stop.is_cancelled() {
            return;
        }
        last_claim = Instant::now();
        let round = send_claim(&shared, as_i64(requested)).await;
        next = finish_round(&shared, &mut slots, requested, round);
        drop(permit);
    }
}

/// When the claim loop sends its next claim.
#[derive(Clone, Copy)]
enum Next {
    /// The last round filled every free slot: more work is likely due.
    Now,
    /// The last round found work: poll again after the cooldown.
    Soon,
    /// The last round found nothing: wait for a wake or the poll tick.
    Idle,
}

/// The shortest gap between the starts of two claims that do not follow a full round.
const CLAIM_COOLDOWN: Duration = Duration::from_millis(25);

async fn cooldown(stop: &CancellationToken, last_claim: Instant) -> bool {
    tokio::select! {
        biased;
        () = stop.cancelled() => false,
        () = tokio::time::sleep_until(last_claim + CLAIM_COOLDOWN) => true,
    }
}

struct CloseTracker<'a>(&'a TaskTracker);

impl Drop for CloseTracker<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

async fn await_tick(
    shared: &Shared,
    stop: &CancellationToken,
    ticker: &mut tokio::time::Interval,
) -> bool {
    tokio::select! {
        biased;
        () = stop.cancelled() => return false,
        () = shared.wake.notified() => ticker.reset(),
        _ = ticker.tick() => {}
    }
    !stop.is_cancelled()
}

/// Wake the claim loop of every engine in this process that registers the
/// kind of a due job enqueue committed.
///
/// The listener holds one connection of its own, outside the engine pool,
/// opened with the pool's connect options. Polling stays the recovery path:
/// a lost connection or notification only delays a claim until the next tick.
pub(crate) async fn run_listener(shared: Arc<Shared>, cancel: CancellationToken) {
    let pool = listener_pool(&shared.pool);
    let _ = Box::pin(cancel.run_until_cancelled(listen(&shared, &pool))).await;
    pool.close().await;
}

fn listener_pool(engine_pool: &PgPool) -> PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_lazy_with(engine_pool.connect_options().as_ref().clone())
}

/// Give the listener's pool the engine pool's current connect options before
/// it opens a connection. The engine pool is the one a rotated password
/// reaches (`infra_postgres::refresh_password_periodically`); the options
/// copied when the listener started would be refused after a rotation.
fn follow_connect_options(engine_pool: &PgPool, listener_pool: &PgPool) {
    listener_pool.set_connect_options(engine_pool.connect_options().as_ref().clone());
}

async fn listen(shared: &Shared, pool: &PgPool) {
    'subscribe: loop {
        follow_connect_options(&shared.pool, pool);
        match subscribe(pool).await {
            Ok(mut listener) => {
                observe_recovery(shared, Operation::Listen);
                // Anything committed while no listener was attached is due now.
                shared.wake_peers(None);
                loop {
                    match listener.try_recv().await {
                        Ok(Some(notification)) => {
                            shared.wake_peers(Some(notification.payload()));
                        }
                        // The connection was lost: subscribe again at once.
                        Ok(None) => continue 'subscribe,
                        Err(error) => {
                            observe_failure(shared, Operation::Listen, &error.into());
                            break;
                        }
                    }
                }
            }
            Err(error) => observe_failure(shared, Operation::Listen, &error.into()),
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The listener leaves reconnecting to [`listen`], which first takes the
/// engine pool's current connect options; the driver's own reconnect would
/// use the ones this connection was opened with.
async fn subscribe(pool: &PgPool) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.eager_reconnect(false);
    listener.listen(WAKE_CHANNEL).await?;
    Ok(listener)
}

/// The channel enqueue notifies with the kind name of a job due at once.
pub(crate) const WAKE_CHANNEL: &str = "background_jobs";

async fn take_slots(shared: &Shared, stop: &CancellationToken) -> Option<OwnedSemaphorePermit> {
    let one = tokio::select! {
        biased;
        () = stop.cancelled() => return None,
        permit = Arc::clone(&shared.slots).acquire_owned() => permit.ok()?,
    };
    let extra = shared.slots.available_permits();
    let mut permits = one;
    if let Ok(count) = u32::try_from(extra)
        && count > 0
        && let Ok(more) = Arc::clone(&shared.slots).try_acquire_many_owned(count)
    {
        permits.merge(more);
    }
    Some(permits)
}

async fn engine_permit<'a>(
    shared: &'a Shared,
    stop: &CancellationToken,
) -> Option<SemaphorePermit<'a>> {
    tokio::select! {
        biased;
        () = stop.cancelled() => None,
        permit = shared.permit.acquire() => permit.ok(),
    }
}

enum ClaimRound {
    Known { sent: Instant, rows: Vec<Drawn> },
    Failed(OperationError),
}

async fn send_claim(shared: &Shared, requested: i64) -> ClaimRound {
    let sent = Instant::now();
    let (names, max_attempts, timeouts) = policy_binds(&shared.registry);
    let result = backstop(async {
        let mut connection = shared
            .pool
            .acquire()
            .await
            .map_err(OperationError::Acquire)?;
        let rows = sqlx::query(CLAIM)
            .bind(&names)
            .bind(&max_attempts)
            .bind(&timeouts)
            .bind(requested)
            .bind(shared.worker_id)
            .bind(lease_reserve_micros())
            .try_map(|row| ClaimRow::from_row(&row)?.into_drawn(&shared.registry))
            .fetch_all(&mut *connection)
            .await?;
        Ok((sent, rows))
    })
    .await;
    metrics::histogram!(CLAIM_DURATION_METRIC).record(sent.elapsed().as_secs_f64());
    match result {
        Ok((sent, rows)) => ClaimRound::Known { sent, rows },
        Err(error) => ClaimRound::Failed(error),
    }
}

fn lease_reserve_micros() -> i64 {
    i64::try_from(LEASE_RESERVE.as_micros()).unwrap_or(i64::MAX)
}

fn policy_binds(registry: &crate::Registry) -> (Vec<&str>, Vec<i16>, Vec<i64>) {
    let mut names = Vec::new();
    let mut max_attempts = Vec::new();
    let mut timeouts = Vec::new();
    for registered in registry.iter() {
        names.push(registered.name);
        max_attempts.push(i16::try_from(registered.policy.max_attempts).unwrap_or(i16::MAX));
        timeouts.push(timeout_micros(registered.policy.timeout));
    }
    (names, max_attempts, timeouts)
}

fn timeout_micros(timeout: Duration) -> i64 {
    i64::try_from(timeout.as_micros()).unwrap_or(i64::MAX)
}

fn as_i64(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(1)
}

fn finish_round(
    shared: &Arc<Shared>,
    slots: &mut OwnedSemaphorePermit,
    requested: usize,
    round: ClaimRound,
) -> Next {
    match round {
        ClaimRound::Known { sent, rows } => {
            observe_recovery(shared, Operation::Claim);
            let next = if rows.len() == requested {
                Next::Now
            } else if rows.is_empty() {
                Next::Idle
            } else {
                Next::Soon
            };
            dispatch_known(shared, slots, rows, sent);
            next
        }
        ClaimRound::Failed(error) => {
            observe_failure(shared, Operation::Claim, &error);
            Next::Idle
        }
    }
}

fn dispatch_known(
    shared: &Arc<Shared>,
    slots: &mut OwnedSemaphorePermit,
    rows: Vec<Drawn>,
    sent: Instant,
) {
    for row in rows {
        match row {
            Drawn::Exhausted {
                id,
                kind,
                attempt,
                error_summary,
            } => {
                attempt::record_exhausted(id, kind, attempt, error_summary.as_deref());
            }
            Drawn::Running {
                id,
                generation,
                kind,
                attempt,
                payload,
                trace_context,
                trace_state,
                timeout,
                queue_wait,
            } => {
                let deadline = local_deadline(sent, timeout);
                let Some(slot) = slots.split(1) else {
                    tracing::error!(job.id = %id, "job_claim_exceeded_slots");
                    break;
                };
                let claimed = Claimed {
                    id,
                    generation,
                    kind,
                    attempt,
                    payload: payload.into_bytes(),
                    trace_context,
                    trace_state,
                    slot,
                };
                match crate::attempt::kind_metrics(shared, kind) {
                    Some(handles) => handles.queue_wait.record(queue_wait.max(0.0)),
                    None => metrics::histogram!(QUEUE_WAIT_METRIC, "kind" => kind)
                        .record(queue_wait.max(0.0)),
                }
                shared.attempt_tracker.spawn(attempt::supervise(
                    Arc::clone(shared),
                    claimed,
                    deadline,
                ));
            }
        }
    }
}

fn local_deadline(sent: Instant, timeout: Duration) -> Instant {
    sent.checked_add(timeout)
        .and_then(|deadline| deadline.checked_add(LEASE_RESERVE))
        .and_then(|deadline| deadline.checked_sub(CANCEL_MARGIN))
        .unwrap_or(sent)
}

enum Drawn {
    Running {
        id: JobId,
        generation: i64,
        kind: &'static str,
        attempt: u16,
        payload: String,
        trace_context: Option<String>,
        trace_state: Option<String>,
        /// Registered policy timeout, the same value CLAIM bound.
        timeout: Duration,
        queue_wait: f64,
    },
    Exhausted {
        id: JobId,
        kind: &'static str,
        attempt: u16,
        error_summary: Option<String>,
    },
}

#[derive(sqlx::FromRow)]
struct ClaimRow<'a> {
    id: uuid::Uuid,
    kind: &'a str,
    exhausted: bool,
    attempts: i16,
    claim_generation: i64,
    payload: Option<String>,
    trace_context: Option<String>,
    trace_state: Option<String>,
    error_summary: Option<&'a str>,
    queue_wait_seconds: f64,
}

impl ClaimRow<'_> {
    fn into_drawn(self, registry: &crate::Registry) -> Result<Drawn, sqlx::Error> {
        let id = JobId(self.id);
        let Some(registered) = registry.get(self.kind) else {
            return Err(decode("unknown job kind"));
        };
        let attempt = u16::try_from(self.attempts).map_err(|_| decode("attempt does not fit"))?;
        if self.exhausted {
            return Ok(Drawn::Exhausted {
                id,
                kind: registered.name,
                attempt,
                error_summary: self.error_summary.map(str::to_owned),
            });
        }
        let Some(payload) = self.payload else {
            return Err(decode("running claim has no payload"));
        };
        Ok(Drawn::Running {
            id,
            generation: self.claim_generation,
            kind: registered.name,
            attempt,
            payload,
            trace_context: self.trace_context,
            trace_state: self.trace_state,
            timeout: registered.policy.timeout,
            queue_wait: self.queue_wait_seconds,
        })
    }
}

fn decode(message: &'static str) -> sqlx::Error {
    sqlx::Error::Decode(message.into())
}

#[cfg(test)]
mod tests {
    use sqlx::ConnectOptions as _;
    use sqlx::postgres::PgConnectOptions;

    use super::*;

    fn password(pool: &PgPool) -> Option<String> {
        pool.connect_options()
            .to_url_lossy()
            .password()
            .map(str::to_owned)
    }

    #[tokio::test]
    async fn the_listener_connects_with_the_engine_pools_rotated_password() {
        let options: PgConnectOptions = "postgres://app:first@127.0.0.1:1/app".parse().unwrap();
        let engine_pool = PgPoolOptions::new().connect_lazy_with(options.clone());
        let listener = listener_pool(&engine_pool);
        assert_eq!(password(&listener).as_deref(), Some("first"));

        engine_pool.set_connect_options(options.password("second"));
        follow_connect_options(&engine_pool, &listener);
        assert_eq!(password(&listener).as_deref(), Some("second"));
    }
}
