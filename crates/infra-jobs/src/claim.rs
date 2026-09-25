//! CLAIM and the claim loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use infra_postgres::in_tx_with;
use sqlx::Row;
use tokio::sync::{OwnedSemaphorePermit, SemaphorePermit};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::attempt;
use crate::engine::{
    CANCEL_MARGIN, LEASE_RESERVE, OpFailed, Operation, OperationError, POLL_INTERVAL,
    READ_COMMITTED, Shared, backstop, observe_failure, observe_recovery,
};
use crate::kind::JobId;

/// Claim due pending jobs and expired running jobs, up to the free slots.
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
         ) AS candidate \
     ), \
     selected AS MATERIALIZED ( \
         SELECT id \
         FROM candidates \
         ORDER BY not_before, id \
         LIMIT $4 \
     ), \
     locked AS ( \
         SELECT job.id, job.claim_generation, policy.max_attempts, policy.timeout_micros \
         FROM selected \
         JOIN background_jobs AS job ON job.id = selected.id \
         JOIN policy ON policy.kind = job.kind \
         WHERE (job.state = 'pending' AND job.not_before <= statement_timestamp()) \
            OR (job.state = 'running' AND job.claim_expires_at <= statement_timestamp()) \
         FOR UPDATE OF job SKIP LOCKED \
     ) \
     UPDATE background_jobs AS job \
     SET state = CASE WHEN job.attempts >= locked.max_attempts THEN 'failed' ELSE 'running' END, \
         failure_reason = CASE WHEN job.attempts >= locked.max_attempts THEN 'exhausted' END, \
         finished_at = CASE WHEN job.attempts >= locked.max_attempts THEN statement_timestamp() END, \
         claim_expires_at = CASE WHEN job.attempts >= locked.max_attempts THEN NULL \
                                 ELSE statement_timestamp() \
                                      + locked.timeout_micros * interval '1 microsecond' \
                                      + interval '60 seconds' END, \
         attempts = CASE WHEN job.attempts >= locked.max_attempts THEN job.attempts \
                         ELSE job.attempts + 1 END, \
         error_summary = CASE WHEN job.attempts >= locked.max_attempts \
                              THEN COALESCE(job.error_summary, 'attempt budget spent') \
                              ELSE job.error_summary END, \
         claim_generation = nextval('background_jobs_claim_generation') \
     FROM locked \
     WHERE job.id = locked.id \
       AND job.claim_generation = locked.claim_generation \
       AND ((job.state = 'pending' AND job.not_before <= statement_timestamp()) \
            OR (job.state = 'running' AND job.claim_expires_at <= statement_timestamp())) \
     RETURNING job.id::text AS id, job.kind, job.state, job.attempts, job.claim_generation, \
               locked.timeout_micros, \
               CASE WHEN job.state = 'running' THEN job.payload::text END AS payload, \
               job.trace_context, job.trace_state, job.error_summary, \
               CASE WHEN job.state = 'running' THEN random() END AS jitter";

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
    pub(crate) jitter: f64,
    pub(crate) slot: OwnedSemaphorePermit,
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
        let round = tokio::select! {
            biased;
            () = stop.cancelled() => return,
            round = send_claim(&shared, as_i64(requested)) => round,
        };
        drop(permit);
        wait_for_tick = finish_round(&shared, &stop, &mut slots, requested, round);
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
    Unknown,
    Failed(OperationError),
}

async fn send_claim(shared: &Shared, requested: i64) -> ClaimRound {
    let (names, max_attempts, timeouts) = policy_binds(&shared.registry);
    let returned = AtomicBool::new(false);
    let result = backstop(
        &returned,
        in_tx_with(
            &shared.pool,
            READ_COMMITTED,
            async |conn| -> Result<(Instant, Vec<Drawn>), OpFailed> {
                let sent = Instant::now();
                let rows = sqlx::query(CLAIM)
                    .bind(&names)
                    .bind(&max_attempts)
                    .bind(&timeouts)
                    .bind(requested)
                    .fetch_all(&mut *conn)
                    .await?;
                let decoded = decode_claims(&rows, &shared.registry)?;
                returned.store(true, Ordering::SeqCst);
                Ok((sent, decoded))
            },
        ),
    )
    .await;
    match result {
        Ok((sent, rows)) => ClaimRound::Known { sent, rows },
        Err(OperationError::CommitUnknown) => ClaimRound::Unknown,
        Err(error) => ClaimRound::Failed(error),
    }
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
    stop: &CancellationToken,
    slots: &mut OwnedSemaphorePermit,
    requested: usize,
    round: ClaimRound,
) -> bool {
    match round {
        ClaimRound::Known { sent, rows } => {
            observe_recovery(shared, Operation::Claim);
            let filled = rows.len() == requested;
            dispatch_known(shared, stop, slots, rows, sent);
            !filled
        }
        ClaimRound::Unknown => {
            observe_failure(shared, Operation::Claim, OperationError::CommitUnknown);
            true
        }
        ClaimRound::Failed(error) => {
            observe_failure(shared, Operation::Claim, error);
            true
        }
    }
}

fn dispatch_known(
    shared: &Arc<Shared>,
    stop: &CancellationToken,
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
        if stop.is_cancelled() || Instant::now() >= deadline {
            return;
        }
        let Some(slot) = slots.split(1) else {
            return;
        };
        let Some(payload) = row.payload else {
            return;
        };
        let Some(jitter) = row.jitter else {
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
            jitter,
            slot,
        };
        if stop.is_cancelled() {
            return;
        }
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
    jitter: Option<f64>,
}

fn decode_claims(
    rows: &[sqlx::postgres::PgRow],
    registry: &crate::Registry,
) -> Result<Vec<Drawn>, OpFailed> {
    let mut decoded = Vec::with_capacity(rows.len());
    for row in rows {
        decoded.push(decode_claim(row, registry)?);
    }
    Ok(decoded)
}

fn decode_claim(
    row: &sqlx::postgres::PgRow,
    registry: &crate::Registry,
) -> Result<Drawn, OpFailed> {
    let id_text: String = row.try_get("id")?;
    let id = JobId::parse(&id_text).ok_or(OpFailed(OperationError::Statement))?;
    let kind_text: String = row.try_get("kind")?;
    let kind = registry
        .get(&kind_text)
        .map(|registered| registered.name)
        .ok_or(OpFailed(OperationError::Statement))?;
    let state_text: String = row.try_get("state")?;
    let state = match state_text.as_str() {
        "running" => DrawnState::Running,
        "failed" => DrawnState::Failed,
        _ => return Err(OpFailed(OperationError::Statement)),
    };
    let attempts_i16: i16 = row.try_get("attempts")?;
    let attempt = u16::try_from(attempts_i16).map_err(|_| OpFailed(OperationError::Statement))?;
    let timeout_micros: i64 = row.try_get("timeout_micros")?;
    if timeout_micros < 0 {
        return Err(OpFailed(OperationError::Statement));
    }
    let payload: Option<String> = row.try_get("payload")?;
    let jitter: Option<f64> = row.try_get("jitter")?;
    if state == DrawnState::Running && (payload.is_none() || jitter.is_none()) {
        return Err(OpFailed(OperationError::Statement));
    }
    Ok(Drawn {
        id,
        generation: row.try_get("claim_generation")?,
        kind,
        state,
        attempt,
        timeout_micros,
        payload,
        trace_context: row.try_get("trace_context")?,
        trace_state: row.try_get("trace_state")?,
        error_summary: row.try_get("error_summary")?,
        jitter,
    })
}
