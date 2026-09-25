//! CLAIM and the claim loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use infra_postgres::{connection, in_tx_with};
use sqlx::Row;
use tokio::sync::{OwnedSemaphorePermit, SemaphorePermit};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::attempt::{self, Facts, LostReason, Outcome};
use crate::engine::{
    self, CLAIM_TTL, OPERATION_BACKSTOP, OpFailed, Operation, OperationError, POLL_INTERVAL,
    READ_COMMITTED, Shared, backstop, observe_failure, observe_recovery, release_collected,
};
use crate::kind::JobId;
use crate::lease::{self, RowState};

/// Claim due pending jobs and expired running jobs, up to the free slots.
const CLAIM: &str = "WITH policy AS ( \
         SELECT policy.kind, policy.max_attempts \
         FROM unnest($1::text[], $2::smallint[]) AS policy (kind, max_attempts) \
     ), \
     available AS ( \
         SELECT candidate.id, candidate.not_before \
         FROM policy \
         CROSS JOIN LATERAL ( \
             SELECT job.id, job.not_before \
             FROM background_jobs AS job \
             WHERE job.state = 'pending' \
               AND job.kind = policy.kind \
               AND job.not_before <= statement_timestamp() \
             ORDER BY job.not_before, job.id \
             LIMIT $3 \
             FOR UPDATE SKIP LOCKED \
         ) AS candidate \
     ), \
     expired AS ( \
         SELECT job.id, job.not_before \
         FROM background_jobs AS job \
         WHERE job.state = 'running' \
           AND job.claim_expires_at <= statement_timestamp() \
           AND job.kind = ANY ($1::text[]) \
         ORDER BY job.not_before, job.id \
         LIMIT $3 \
         FOR UPDATE SKIP LOCKED \
     ), \
     picked AS ( \
         SELECT candidates.id \
         FROM ( \
             SELECT available.id, available.not_before FROM available \
             UNION ALL \
             SELECT expired.id, expired.not_before FROM expired \
         ) AS candidates \
         ORDER BY candidates.not_before, candidates.id \
         LIMIT $3 \
     ) \
     UPDATE background_jobs AS job \
     SET state = CASE WHEN job.attempts >= policy.max_attempts THEN 'failed' ELSE 'running' END, \
         failure_reason = CASE WHEN job.attempts >= policy.max_attempts THEN 'exhausted' END, \
         finished_at = CASE WHEN job.attempts >= policy.max_attempts THEN statement_timestamp() END, \
         claim_expires_at = CASE WHEN job.attempts >= policy.max_attempts THEN NULL \
                                 ELSE statement_timestamp() + $4 END, \
         attempts = CASE WHEN job.attempts >= policy.max_attempts THEN job.attempts \
                         ELSE job.attempts + 1 END, \
         error_summary = CASE WHEN job.attempts >= policy.max_attempts \
                              THEN COALESCE(job.error_summary, 'attempt budget spent') \
                              ELSE job.error_summary END, \
         claim_generation = nextval('background_jobs_claim_generation') \
     FROM policy \
     WHERE job.id = ANY (ARRAY(SELECT picked.id FROM picked)) \
       AND policy.kind = job.kind \
     RETURNING job.id::text AS id, job.kind, job.state, job.attempts, job.claim_generation, \
               CASE WHEN job.state = 'running' THEN job.payload END AS payload, \
               job.trace_context, job.error_summary";

/// A row CLAIM set `running`.
#[derive(Debug)]
pub(crate) struct Claimed {
    pub(crate) id: JobId,
    pub(crate) generation: i64,
    pub(crate) kind: &'static str,
    pub(crate) attempt: u16,
    pub(crate) payload: Vec<u8>,
    pub(crate) trace_context: Option<String>,
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
        let round = send_claim(&shared, as_i64(requested)).await;
        drop(permit);
        wait_for_tick = finish_round(&shared, &mut slots, requested, round).await;
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
    Unknown { sent: Instant, rows: Vec<Drawn> },
    Failed(OperationError),
}

async fn send_claim(shared: &Shared, requested: i64) -> ClaimRound {
    let (names, max_attempts) = policy_binds(&shared.registry);
    let kept = Mutex::new(None);
    let returned = AtomicBool::new(false);
    let result = backstop(
        &returned,
        in_tx_with(
            &shared.pool,
            READ_COMMITTED,
            async |tx| -> Result<(), OpFailed> {
                let conn = connection(tx);
                let sent = Instant::now();
                let rows = sqlx::query(CLAIM)
                    .bind(&names)
                    .bind(&max_attempts)
                    .bind(requested)
                    .bind(CLAIM_TTL)
                    .fetch_all(&mut *conn)
                    .await?;
                let decoded = decode_claims(&rows, &shared.registry)?;
                *lock(&kept) = Some((sent, decoded));
                returned.store(true, Ordering::SeqCst);
                Ok(())
            },
        ),
    )
    .await;
    let stored = lock(&kept).take();
    match result {
        Ok(()) => stored_round(stored, true),
        Err(OperationError::CommitUnknown) => stored_round(stored, false),
        Err(error) => ClaimRound::Failed(error),
    }
}

fn stored_round(stored: Option<(Instant, Vec<Drawn>)>, known: bool) -> ClaimRound {
    let (sent, rows) = stored.unwrap_or_else(|| (Instant::now(), Vec::new()));
    if known {
        ClaimRound::Known { sent, rows }
    } else {
        ClaimRound::Unknown { sent, rows }
    }
}

fn policy_binds(registry: &crate::Registry) -> (Vec<&str>, Vec<i16>) {
    let mut names = Vec::new();
    let mut max_attempts = Vec::new();
    for registered in registry.iter() {
        names.push(registered.name);
        let attempts = i16::try_from(registered.policy.max_attempts).unwrap_or(i16::MAX);
        max_attempts.push(attempts);
    }
    (names, max_attempts)
}

fn as_i64(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(1)
}

async fn finish_round(
    shared: &Arc<Shared>,
    slots: &mut OwnedSemaphorePermit,
    requested: usize,
    round: ClaimRound,
) -> bool {
    match round {
        ClaimRound::Known { sent, rows } => {
            observe_recovery(shared, Operation::Claim);
            let filled = rows.len() == requested;
            let deadline = lease::local_deadline(sent);
            release_handed(shared, process_rows(shared, slots, rows, deadline)).await;
            !filled
        }
        ClaimRound::Unknown { sent, rows } => {
            observe_failure(shared, Operation::Claim, OperationError::CommitUnknown);
            let deadline = lease::local_deadline(sent);
            let claimed = attribute_unknown(shared, rows, deadline).await;
            release_handed(shared, process_rows(shared, slots, claimed, deadline)).await;
            true
        }
        ClaimRound::Failed(error) => {
            observe_failure(shared, Operation::Claim, error);
            true
        }
    }
}

async fn release_handed(shared: &Shared, handed: Vec<engine::Collected>) {
    if handed.is_empty() {
        return;
    }
    let deadline = Instant::now()
        .checked_add(OPERATION_BACKSTOP)
        .unwrap_or_else(Instant::now);
    release_collected(shared, handed, deadline).await;
}

async fn attribute_unknown(shared: &Shared, rows: Vec<Drawn>, deadline: Instant) -> Vec<Drawn> {
    if rows.is_empty() || Instant::now() >= deadline {
        return Vec::new();
    }
    let pending = rows;
    loop {
        if Instant::now() >= deadline {
            return Vec::new();
        }
        match reconcile_kept(shared, &pending, deadline).await {
            KeptTry::Ready(found) => {
                return pending
                    .into_iter()
                    .filter(|row| was_claimed(row, &found))
                    .collect();
            }
            KeptTry::Retry => sleep_capped(deadline, Duration::from_secs(1)).await,
            KeptTry::Stop => return Vec::new(),
        }
    }
}

enum KeptTry {
    Ready(Vec<RowState>),
    Retry,
    Stop,
}

enum Step {
    Closed,
    Done(Result<Vec<RowState>, OperationError>),
}

async fn reconcile_kept(shared: &Shared, pending: &[Drawn], deadline: Instant) -> KeptTry {
    let id_list: Vec<JobId> = pending.iter().map(|row| row.id).collect();
    let attempt = tokio::time::timeout_at(deadline, async {
        let Ok(_permit) = shared.permit.acquire().await else {
            return Step::Closed;
        };
        Step::Done(lease::reconcile(shared, &id_list).await)
    })
    .await;
    match attempt {
        Err(_) | Ok(Step::Closed) => KeptTry::Stop,
        Ok(Step::Done(Ok(rows))) => {
            observe_recovery(shared, Operation::Reconcile);
            KeptTry::Ready(rows)
        }
        Ok(Step::Done(Err(error))) => {
            observe_failure(shared, Operation::Reconcile, error);
            KeptTry::Retry
        }
    }
}

fn was_claimed(row: &Drawn, found: &[RowState]) -> bool {
    found
        .iter()
        .any(|state| state.id == row.id && state.generation == row.generation)
}

fn process_rows(
    shared: &Arc<Shared>,
    slots: &mut OwnedSemaphorePermit,
    rows: Vec<Drawn>,
    deadline: Instant,
) -> Vec<engine::Collected> {
    let mut handed = Vec::new();
    for row in rows {
        if row.state == DrawnState::Failed {
            record_exhausted(&row);
            continue;
        }
        if Instant::now() >= deadline || row.payload.is_none() {
            record_lost(&row);
            continue;
        }
        let Some(slot) = slots.split(1) else {
            handed.push(handed_back(&row, deadline));
            continue;
        };
        let Some(payload) = row.payload else {
            record_lost(&row);
            continue;
        };
        let claimed = Claimed {
            id: row.id,
            generation: row.generation,
            kind: row.kind,
            attempt: row.attempts,
            payload,
            trace_context: row.trace_context,
            slot,
        };
        if let Err(returned) = shared
            .attempts
            .admit(claimed, deadline, |claimed, handles| {
                shared
                    .attempt_tracker
                    .spawn(attempt::supervise(Arc::clone(shared), claimed, handles))
                    .abort_handle()
            })
        {
            handed.push(handed_back_claimed(&returned, deadline));
        }
    }
    handed
}

fn record_exhausted(row: &Drawn) {
    attempt::record(
        Outcome::Exhausted,
        &Facts {
            id: row.id,
            kind: row.kind,
            attempt: row.attempts,
            summary: row.error_summary.as_deref(),
            failure: None,
            retry_in: None,
            lost: None,
            ran: None,
        },
    );
}

fn record_lost(row: &Drawn) {
    attempt::record(
        Outcome::Lost,
        &Facts {
            id: row.id,
            kind: row.kind,
            attempt: row.attempts,
            summary: None,
            failure: None,
            retry_in: None,
            lost: Some(LostReason::Extension),
            ran: None,
        },
    );
}

fn handed_back(row: &Drawn, deadline: Instant) -> engine::Collected {
    engine::Collected {
        id: row.id,
        kind: row.kind,
        generation: row.generation,
        attempt: row.attempts,
        intended: None,
        ran: None,
        deadline,
    }
}

fn handed_back_claimed(claimed: &Claimed, deadline: Instant) -> engine::Collected {
    engine::Collected {
        id: claimed.id,
        kind: claimed.kind,
        generation: claimed.generation,
        attempt: claimed.attempt,
        intended: None,
        ran: None,
        deadline,
    }
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
    attempts: u16,
    payload: Option<Vec<u8>>,
    trace_context: Option<String>,
    error_summary: Option<String>,
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
    let attempts = u16::try_from(attempts_i16).map_err(|_| OpFailed(OperationError::Statement))?;
    let generation: i64 = row.try_get("claim_generation")?;
    let payload: Option<Vec<u8>> = row.try_get("payload")?;
    if state == DrawnState::Running && payload.is_none() {
        return Err(OpFailed(OperationError::Statement));
    }
    Ok(Drawn {
        id,
        generation,
        kind,
        state,
        attempts,
        payload,
        trace_context: row.try_get("trace_context")?,
        error_summary: row.try_get("error_summary")?,
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

async fn sleep_capped(deadline: Instant, delay: Duration) {
    let until = match Instant::now().checked_add(delay) {
        Some(at) if at < deadline => at,
        _ => deadline,
    };
    tokio::time::sleep_until(until).await;
}
