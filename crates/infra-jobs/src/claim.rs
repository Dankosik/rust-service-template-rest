//! CLAIM and the claim loop.

use std::sync::Arc;
use std::time::Duration;

use sqlx::Row;
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
                                      + interval '60 seconds' END, \
         attempts = CASE WHEN job.attempts >= policy.max_attempts THEN job.attempts \
                         ELSE job.attempts + 1 END, \
         attempted_by = CASE WHEN job.attempts >= policy.max_attempts THEN job.attempted_by \
                             ELSE $5::uuid END, \
         error_summary = CASE WHEN job.state = 'running' \
                                   AND job.claim_expires_at <= statement_timestamp() \
                              THEN 'lease expired; rescued' \
                              WHEN job.attempts >= policy.max_attempts \
                              THEN COALESCE(job.error_summary, 'attempt budget spent') \
                              ELSE job.error_summary END, \
         claim_generation = nextval('background_jobs_claim_generation') \
     FROM policy \
     WHERE job.id = ANY (ARRAY(SELECT picked.id FROM picked)) \
       AND policy.kind = job.kind \
       AND ((job.state = 'pending' AND job.not_before <= statement_timestamp()) \
            OR (job.state = 'running' AND job.claim_expires_at <= statement_timestamp())) \
     RETURNING job.id, job.kind, job.state, job.attempts, job.claim_generation, \
               policy.timeout_micros, \
               CASE WHEN job.state = 'running' THEN job.payload::text END AS payload, \
               job.trace_context, job.trace_state, job.error_summary, \
               EXTRACT(EPOCH FROM statement_timestamp())::double precision AS claimed_at, \
               EXTRACT(EPOCH FROM job.not_before)::double precision AS not_before_epoch";

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
    let mut wait_for_tick = true;
    loop {
        if wait_for_tick && !await_tick(&stop, &mut ticker).await {
            return;
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
        let round = send_claim(&shared, as_i64(requested)).await;
        wait_for_tick = finish_round(&shared, &mut slots, requested, round);
        drop(permit);
    }
}

struct CloseTracker<'a>(&'a TaskTracker);

impl Drop for CloseTracker<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

async fn await_tick(stop: &CancellationToken, ticker: &mut tokio::time::Interval) -> bool {
    tokio::select! {
        biased;
        () = stop.cancelled() => return false,
        _ = ticker.tick() => {}
    }
    !stop.is_cancelled()
}

async fn take_slots(shared: &Shared, stop: &CancellationToken) -> Option<OwnedSemaphorePermit> {
    let one = tokio::select! {
        biased;
        () = stop.cancelled() => return None,
        permit = Arc::clone(&shared.slots).acquire_many_owned(1) => permit.ok()?,
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
            .map_err(|_| OperationError::Acquire)?;
        let rows = sqlx::query(CLAIM)
            .bind(&names)
            .bind(&max_attempts)
            .bind(&timeouts)
            .bind(requested)
            .bind(shared.worker_id)
            .fetch_all(&mut *connection)
            .await
            .map_err(statement_error)?;
        let decoded = decode_claims(&rows, &shared.registry)?;
        Ok((sent, decoded))
    })
    .await;
    metrics::histogram!(CLAIM_DURATION_METRIC).record(sent.elapsed().as_secs_f64());
    match result {
        Ok((sent, rows)) => ClaimRound::Known { sent, rows },
        Err(error) => ClaimRound::Failed(error),
    }
}

fn statement_error(_error: sqlx::Error) -> OperationError {
    OperationError::Statement
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
) -> bool {
    match round {
        ClaimRound::Known { sent, rows } => {
            observe_recovery(shared, Operation::Claim);
            let filled = rows.len() == requested;
            dispatch_known(shared, slots, rows, sent);
            !filled
        }
        ClaimRound::Failed(error) => {
            observe_failure(shared, Operation::Claim, error);
            true
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
        if row.state == DrawnState::Failed {
            attempt::record_exhausted(row.id, row.kind, row.attempt, row.error_summary.as_deref());
            continue;
        }
        let deadline = local_deadline(sent, row.timeout_micros);
        if Instant::now() >= deadline {
            continue;
        }
        let Some(slot) = slots.split(1) else {
            return;
        };
        let Some(payload) = row.payload else {
            return;
        };
        let claimed = Claimed {
            id: row.id,
            generation: row.generation,
            kind: row.kind,
            attempt: row.attempt,
            payload: payload.into_bytes(),
            trace_context: row.trace_context,
            trace_state: row.trace_state,
            slot,
        };
        metrics::histogram!(QUEUE_WAIT_METRIC, "kind" => row.kind)
            .record((row.claimed_at - row.not_before_epoch).max(0.0));
        shared
            .attempt_tracker
            .spawn(attempt::supervise(Arc::clone(shared), claimed, deadline));
    }
}

fn local_deadline(sent: Instant, timeout_micros: i64) -> Instant {
    let timeout = Duration::from_micros(u64::try_from(timeout_micros).unwrap_or_default());
    sent.checked_add(timeout)
        .and_then(|deadline| deadline.checked_add(LEASE_RESERVE))
        .and_then(|deadline| deadline.checked_sub(CANCEL_MARGIN))
        .unwrap_or(sent)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DrawnState {
    Running,
    Failed,
}

struct Drawn {
    id: JobId,
    generation: i64,
    kind: &'static str,
    state: DrawnState,
    attempt: u16,
    timeout_micros: i64,
    payload: Option<String>,
    trace_context: Option<String>,
    trace_state: Option<String>,
    error_summary: Option<String>,
    claimed_at: f64,
    not_before_epoch: f64,
}

fn decode_claims(
    rows: &[sqlx::postgres::PgRow],
    registry: &crate::Registry,
) -> Result<Vec<Drawn>, OperationError> {
    let mut decoded = Vec::with_capacity(rows.len());
    for row in rows {
        decoded.push(decode_claim(row, registry)?);
    }
    Ok(decoded)
}

fn decode_claim(
    row: &sqlx::postgres::PgRow,
    registry: &crate::Registry,
) -> Result<Drawn, OperationError> {
    let id = JobId::from_uuid(row.try_get("id").map_err(statement_error)?);
    let kind_text: String = row.try_get("kind").map_err(statement_error)?;
    let kind = registry
        .get(&kind_text)
        .map(|registered| registered.name)
        .ok_or(OperationError::Statement)?;
    let state_text: String = row.try_get("state").map_err(statement_error)?;
    let state = match state_text.as_str() {
        "running" => DrawnState::Running,
        "failed" => DrawnState::Failed,
        _ => return Err(OperationError::Statement),
    };
    let attempts_i16: i16 = row.try_get("attempts").map_err(statement_error)?;
    let attempt = u16::try_from(attempts_i16).map_err(|_| OperationError::Statement)?;
    let timeout_micros: i64 = row.try_get("timeout_micros").map_err(statement_error)?;
    if timeout_micros < 0 {
        return Err(OperationError::Statement);
    }
    let payload: Option<String> = row.try_get("payload").map_err(statement_error)?;
    if state == DrawnState::Running && payload.is_none() {
        return Err(OperationError::Statement);
    }
    Ok(Drawn {
        id,
        generation: row.try_get("claim_generation").map_err(statement_error)?,
        kind,
        state,
        attempt,
        timeout_micros,
        payload,
        trace_context: row.try_get("trace_context").map_err(statement_error)?,
        trace_state: row.try_get("trace_state").map_err(statement_error)?,
        error_summary: row.try_get("error_summary").map_err(statement_error)?,
        claimed_at: row.try_get("claimed_at").map_err(statement_error)?,
        not_before_epoch: row.try_get("not_before_epoch").map_err(statement_error)?,
    })
}
