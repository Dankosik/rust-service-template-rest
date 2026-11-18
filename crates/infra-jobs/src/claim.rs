use std::num::NonZeroU32;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use infra_postgres::observed;
use metrics::Unit;
use sqlx::postgres::PgConnection;
use tokio::sync::{MutexGuard, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::attempt::{self, AttemptId};
use crate::engine::{
    CANCEL_MARGIN, LEASE_RESERVE, Operation, OperationError, POLL_INTERVAL, Shared, backstop,
    observe_failure, observe_recovery,
};
use crate::kind::{JobId, Registered, saturating_micros};
use crate::trace_context::Carrier;

pub const CLAIM_DURATION_METRIC: &str = "jobs_claim_duration_seconds";
pub(crate) const CLAIM_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 8.0, 12.0,
];
pub const QUEUE_WAIT_METRIC: &str = "jobs_queue_wait_seconds";
pub(crate) const QUEUE_WAIT_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0,
    1800.0, 3600.0, 10_800.0, 21_600.0, 43_200.0, 86_400.0,
];

pub(crate) struct Claimed {
    pub(crate) registered: Arc<Registered>,
    pub(crate) attempt: AttemptId,
    pub(crate) generation: i64,
    pub(crate) payload: Result<Vec<u8>, crate::JobError>,
    pub(crate) trace: Carrier,
    pub(crate) slot: OwnedSemaphorePermit,
    pub(crate) kind_slot: Option<KindSlot>,
}

#[derive(Debug)]
pub(crate) struct KindSlot {
    permit: Option<OwnedSemaphorePermit>,
    wake: Arc<Notify>,
}

impl KindSlot {
    fn take(running_slots: &Arc<Semaphore>, wake: &Arc<Notify>) -> Option<Self> {
        let permit = Arc::clone(running_slots).try_acquire_owned().ok()?;
        Some(Self {
            permit: Some(permit),
            wake: Arc::clone(wake),
        })
    }
}

impl Drop for KindSlot {
    fn drop(&mut self) {
        let Some(permit) = self.permit.take() else {
            return;
        };
        drop(permit);
        self.wake.notify_one();
    }
}

pub(crate) fn admission_slots(limit: NonZeroU32) -> Semaphore {
    let permits = usize::try_from(limit.get()).unwrap_or(Semaphore::MAX_PERMITS);
    Semaphore::new(permits.min(Semaphore::MAX_PERMITS))
}

pub(crate) fn describe_metrics() {
    metrics::describe_histogram!(
        CLAIM_DURATION_METRIC,
        Unit::Seconds,
        "Claim request duration through acknowledgement"
    );
    metrics::describe_histogram!(
        QUEUE_WAIT_METRIC,
        Unit::Seconds,
        "Time from a job's current not-before to the start of its successful claim statement"
    );
}

pub(crate) async fn run_claim_loop(
    shared: Arc<Shared>,
    stop: CancellationToken,
    failure: CancellationToken,
) {
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let policy = PolicyBinds::new(&shared.registry);
    let mut next = Next::Idle;
    let mut last_claim = Instant::now();
    loop {
        match next {
            Next::Now => {}
            Next::Soon => {
                if cooldown(&stop, last_claim).await.is_break() {
                    return;
                }
            }
            Next::Idle => {
                if wait_for_wake_or_tick(&shared, &stop, &mut ticker)
                    .await
                    .is_break()
                    || cooldown(&stop, last_claim).await.is_break()
                {
                    return;
                }
            }
        }
        let Some(mut slots) = take_slots(&shared, &stop).await else {
            return;
        };
        let requested = slots.num_permits();
        let round = {
            let Some(_turn) = statement_turn(&shared, &stop).await else {
                return;
            };
            if stop.is_cancelled() {
                return;
            }
            last_claim = Instant::now();
            send_claim(&shared, &policy, slot_count(requested)).await
        };
        next = finish_round(&shared, &mut slots, requested, round, &stop, &failure);
    }
}

#[derive(Clone, Copy)]
enum Next {
    Now,
    Soon,
    Idle,
}

pub(crate) const CLAIM_COOLDOWN: Duration = Duration::from_millis(25);

async fn cooldown(stop: &CancellationToken, last_claim: Instant) -> ControlFlow<()> {
    tokio::select! {
        biased;
        () = stop.cancelled() => ControlFlow::Break(()),
        () = tokio::time::sleep_until(last_claim + CLAIM_COOLDOWN) => ControlFlow::Continue(()),
    }
}

async fn wait_for_wake_or_tick(
    shared: &Shared,
    stop: &CancellationToken,
    ticker: &mut tokio::time::Interval,
) -> ControlFlow<()> {
    tokio::select! {
        biased;
        () = stop.cancelled() => return ControlFlow::Break(()),
        () = shared.claim_wake.notified() => ticker.reset(),
        _ = ticker.tick() => {}
    }
    if stop.is_cancelled() {
        ControlFlow::Break(())
    } else {
        ControlFlow::Continue(())
    }
}

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

async fn statement_turn<'a>(
    shared: &'a Shared,
    stop: &CancellationToken,
) -> Option<MutexGuard<'a, ()>> {
    tokio::select! {
        biased;
        () = stop.cancelled() => None,
        turn = shared.statement_turn.lock() => Some(turn),
    }
}

enum ClaimRound {
    Known {
        claim_started_at: Instant,
        rows: Vec<Drawn>,
    },
    Failed(OperationError),
}

async fn send_claim(shared: &Shared, policy: &PolicyBinds<'_>, requested: i64) -> ClaimRound {
    let claim_started_at = Instant::now();
    let result = backstop(async {
        let mut connection = infra_postgres::acquire(&shared.pool, "claim jobs")
            .await
            .map_err(OperationError::Acquire)?;
        Ok(claim(&mut connection, shared.worker_id, policy, requested).await?)
    })
    .await;
    metrics::histogram!(CLAIM_DURATION_METRIC).record(claim_started_at.elapsed().as_secs_f64());
    match result {
        Ok(rows) => ClaimRound::Known {
            claim_started_at,
            rows,
        },
        Err(error) => ClaimRound::Failed(error),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one statement, and its binds beside it"
)]
async fn claim(
    connection: &mut PgConnection,
    worker_id: uuid::Uuid,
    policy: &PolicyBinds<'_>,
    requested: i64,
) -> Result<Vec<Drawn>, sqlx::Error> {
    let claim_limits: smallvec::SmallVec<[i64; 8]> = policy
        .registry
        .iter()
        .map(|registered| {
            registered
                .running_slots
                .as_ref()
                .map_or(requested, |slots| {
                    requested.min(slot_count(slots.available_permits()))
                })
        })
        .collect();
    let rescued_summary = "lease expired; rescued";
    observed(
        "claim jobs",
        sqlx::query_as!(
            ClaimRow,
            "WITH policy AS ( \
                 SELECT policy.kind, policy.max_attempts, policy.lease_offset_micros, \
                        policy.claim_limit, policy.recover_at_exhaustion \
                 FROM unnest($1::text[], $2::smallint[], $3::bigint[], $6::bigint[], $8::bool[]) \
                     AS policy (kind, max_attempts, lease_offset_micros, claim_limit, recover_at_exhaustion) \
             ), \
             candidates AS ( \
                 SELECT candidate.id, candidate.not_before, candidate.attempts, candidate.state, \
                        candidate.claim_expires_at, policy.max_attempts, \
                        policy.lease_offset_micros, policy.kind, policy.claim_limit, policy.recover_at_exhaustion \
                 FROM policy \
                 CROSS JOIN LATERAL ( \
                     SELECT job.id, job.not_before, job.attempts, job.state, \
                            job.claim_expires_at \
                     FROM background_jobs AS job \
                     WHERE job.state = 'pending' \
                       AND job.kind = policy.kind \
                       AND job.not_before <= statement_timestamp() \
                       AND NOT EXISTS (SELECT 1 FROM background_jobs AS predecessor \
                                       WHERE predecessor.id = job.predecessor_job_id \
                                         AND predecessor.state <> 'completed') \
                     ORDER BY job.not_before, job.id \
                     LIMIT policy.claim_limit \
                     FOR UPDATE SKIP LOCKED \
                 ) AS candidate \
                 UNION ALL \
                 SELECT candidate.id, candidate.not_before, candidate.attempts, candidate.state, \
                        candidate.claim_expires_at, policy.max_attempts, \
                        policy.lease_offset_micros, policy.kind, policy.claim_limit, policy.recover_at_exhaustion \
                 FROM policy \
                 CROSS JOIN LATERAL ( \
                     SELECT job.id, job.not_before, job.attempts, job.state, \
                            job.claim_expires_at \
                     FROM background_jobs AS job \
                     WHERE job.state = 'running' \
                       AND job.kind = policy.kind \
                       AND job.claim_expires_at <= statement_timestamp() \
                       AND NOT EXISTS (SELECT 1 FROM background_jobs AS predecessor \
                                       WHERE predecessor.id = job.predecessor_job_id \
                                         AND predecessor.state <> 'completed') \
                     ORDER BY job.not_before, job.id \
                     LIMIT policy.claim_limit \
                     FOR UPDATE SKIP LOCKED \
                 ) AS candidate \
             ), \
             ranked AS ( \
                 SELECT candidates.id, candidates.not_before, candidates.attempts, \
                        candidates.state, candidates.claim_expires_at, \
                        candidates.max_attempts, candidates.lease_offset_micros, \
                        candidates.claim_limit, candidates.recover_at_exhaustion, \
                        row_number() OVER ( \
                            PARTITION BY candidates.kind \
                            ORDER BY candidates.not_before, candidates.id \
                        ) AS position \
                 FROM candidates \
             ), \
             picked AS ( \
                 SELECT ranked.id, ranked.attempts, ranked.lease_offset_micros, ranked.max_attempts, \
                        ranked.recover_at_exhaustion, \
                        ranked.attempts >= ranked.max_attempts AND NOT ranked.recover_at_exhaustion AS exhausted, \
                        ranked.state = 'running' \
                            AND ranked.claim_expires_at <= statement_timestamp() AS rescued \
                 FROM ranked \
                 WHERE ranked.position <= ranked.claim_limit \
                 ORDER BY ranked.not_before, ranked.id \
                 LIMIT $4 \
             ) \
             UPDATE background_jobs AS job \
             SET state = CASE WHEN picked.exhausted THEN 'failed' ELSE 'running' END, \
                 failure_reason = CASE WHEN picked.exhausted THEN 'exhausted' END, \
                 finished_at = CASE WHEN picked.exhausted THEN statement_timestamp() END, \
                 claim_expires_at = CASE WHEN picked.exhausted THEN NULL \
                                         ELSE statement_timestamp() \
                                              + picked.lease_offset_micros \
                                                * interval '1 microsecond' END, \
                 attempts = CASE WHEN picked.exhausted THEN picked.attempts \
                                 ELSE LEAST(picked.attempts + 1, picked.max_attempts) END, \
                 attempted_by = CASE WHEN picked.exhausted THEN job.attempted_by \
                                     ELSE $5 END, \
                 error_summary = CASE WHEN picked.rescued \
                                      THEN $7::text \
                                      WHEN picked.exhausted \
                                      THEN COALESCE(job.error_summary, 'attempt budget spent') \
                                      ELSE job.error_summary END, \
                 errors = CASE WHEN picked.rescued \
                               THEN (CASE WHEN jsonb_array_length(job.errors) >= $9 \
                                          THEN job.errors - 0 ELSE job.errors END) || jsonb_build_object( \
                                        'attempt', picked.attempts, 'at', statement_timestamp(), \
                                        'error', $7::text) \
                               ELSE job.errors END, \
                 claim_generation = nextval('background_jobs_claim_generation') \
             FROM picked \
             WHERE job.id = picked.id \
                AND ((job.state = 'pending' AND job.not_before <= statement_timestamp()) \
                     OR (job.state = 'running' AND job.claim_expires_at <= statement_timestamp())) \
             RETURNING job.id, job.kind, (job.state = 'failed') AS \"exhausted!\", job.attempts, \
                       job.claim_generation, \
                       CASE WHEN job.state <> 'running' THEN NULL \
                            WHEN picked.recover_at_exhaustion THEN outbox_verified_payload( \
                                job.id, job.unique_key, job.predecessor_job_id, job.payload_text, job.payload) \
                            ELSE COALESCE(job.payload_text, job.payload::text) END AS payload, \
                       job.trace_context AS trace_parent, job.trace_state, job.error_summary, \
                       EXTRACT(EPOCH FROM statement_timestamp() - job.not_before)::double precision \
                           AS \"queue_wait_seconds!\"",
            &policy.names as _,
            &policy.max_attempts,
            &policy.lease_offsets_micros,
            requested,
            worker_id,
            &claim_limits[..],
            rescued_summary,
            &policy.recover_at_exhaustion[..],
            i32::from(crate::kind::MAX_ATTEMPTS),
        )
        .try_map(|row| row.into_drawn(policy.registry))
        .fetch_all(&mut *connection),
    )
    .await
}

struct PolicyBinds<'a> {
    registry: &'a crate::Kinds,
    names: Vec<&'static str>,
    max_attempts: Vec<i16>,
    lease_offsets_micros: Vec<i64>,
    recover_at_exhaustion: Vec<bool>,
}

impl<'a> PolicyBinds<'a> {
    fn new(registry: &'a crate::Kinds) -> Self {
        let (names, max_attempts, lease_offsets_micros, recover_at_exhaustion) = registry
            .iter()
            .map(|registered| {
                (
                    registered.name,
                    i16::try_from(registered.policy.max_attempts).unwrap_or(i16::MAX),
                    saturating_micros(lease_offset(registered.policy.timeout)),
                    registered.recover_at_exhaustion,
                )
            })
            .collect();
        Self {
            registry,
            names,
            max_attempts,
            lease_offsets_micros,
            recover_at_exhaustion,
        }
    }
}

fn lease_offset(timeout: Duration) -> Duration {
    timeout.saturating_add(LEASE_RESERVE)
}

fn slot_count(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(1)
}

fn finish_round(
    shared: &Arc<Shared>,
    slots: &mut OwnedSemaphorePermit,
    requested: usize,
    round: ClaimRound,
    stop: &CancellationToken,
    failure: &CancellationToken,
) -> Next {
    match round {
        ClaimRound::Known {
            claim_started_at,
            rows,
        } => {
            observe_recovery(shared, Operation::Claim);
            let next = if rows.len() == requested {
                Next::Now
            } else if rows.is_empty() {
                Next::Idle
            } else {
                Next::Soon
            };
            dispatch_known(shared, slots, rows, claim_started_at, stop, failure);
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
    claim_started_at: Instant,
    stop: &CancellationToken,
    failure: &CancellationToken,
) {
    for row in rows {
        match row {
            Drawn::Exhausted {
                attempt,
                error_summary,
            } => {
                attempt::record_exhausted(&attempt, error_summary.as_deref());
            }
            Drawn::Running {
                registered,
                attempt,
                generation,
                payload,
                trace,
                lease_offset,
                queue_wait_seconds,
            } => {
                let deadline = local_deadline(claim_started_at, lease_offset);
                let Some(slot) = slots.split(1) else {
                    tracing::error!(
                        job.id = %attempt.job_id,
                        job.kind = attempt.kind,
                        "job_claim_exceeded_slots"
                    );
                    break;
                };
                let kind_slot = registered
                    .running_slots
                    .as_ref()
                    .and_then(|slots| KindSlot::take(slots, &shared.claim_wake));
                registered
                    .metrics()
                    .queue_wait
                    .record(queue_wait_seconds.max(0.0));
                let guard = crate::engine::SupervisorGuard::new(
                    Arc::clone(&shared.lifecycle.drain),
                    stop.clone(),
                    failure.clone(),
                    attempt.job_id,
                    attempt.kind,
                );
                let claimed = Claimed {
                    registered,
                    attempt,
                    generation,
                    payload,
                    trace,
                    slot,
                    kind_slot,
                };
                tokio::spawn(attempt::supervise(
                    Arc::clone(shared),
                    claimed,
                    deadline,
                    guard,
                ));
            }
        }
    }
}

fn local_deadline(claim_started_at: Instant, lease_offset: Duration) -> Instant {
    claim_started_at
        .checked_add(lease_offset)
        .and_then(|deadline| deadline.checked_sub(CANCEL_MARGIN))
        .unwrap_or(claim_started_at)
}

enum Drawn {
    Running {
        registered: Arc<Registered>,
        attempt: AttemptId,
        generation: i64,
        payload: Result<Vec<u8>, crate::JobError>,
        trace: Carrier,
        lease_offset: Duration,
        queue_wait_seconds: f64,
    },
    Exhausted {
        attempt: AttemptId,
        error_summary: Option<String>,
    },
}

struct ClaimRow {
    id: uuid::Uuid,
    kind: String,
    exhausted: bool,
    attempts: i16,
    claim_generation: i64,
    payload: Option<String>,
    trace_parent: Option<String>,
    trace_state: Option<String>,
    error_summary: Option<String>,
    queue_wait_seconds: f64,
}

impl ClaimRow {
    fn into_drawn(self, registry: &crate::Kinds) -> Result<Drawn, sqlx::Error> {
        let Some(registered) = registry.get(&self.kind) else {
            return Err(ClaimDecodeError::UnknownKind.into_sqlx());
        };
        let attempt = AttemptId {
            job_id: JobId(self.id),
            kind: registered.name,
            number: u16::try_from(self.attempts)
                .map_err(|_| ClaimDecodeError::AttemptOutOfRange.into_sqlx())?,
        };
        if self.exhausted {
            return Ok(Drawn::Exhausted {
                attempt,
                error_summary: self.error_summary,
            });
        }
        let payload = match self.payload {
            Some(payload) => Ok(payload.into_bytes()),
            None if registered.recover_at_exhaustion => {
                Err(crate::JobError::permanent("outbox_receipt_mismatch"))
            }
            None => return Err(ClaimDecodeError::MissingPayload.into_sqlx()),
        };
        Ok(Drawn::Running {
            lease_offset: lease_offset(registered.policy.timeout),
            registered: Arc::clone(registered),
            attempt,
            generation: self.claim_generation,
            payload,
            trace: Carrier::from_stored(self.trace_parent, self.trace_state),
            queue_wait_seconds: self.queue_wait_seconds,
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum ClaimDecodeError {
    #[error("unknown job kind")]
    UnknownKind,
    #[error("attempt does not fit")]
    AttemptOutOfRange,
    #[error("running claim has no payload")]
    MissingPayload,
}

impl ClaimDecodeError {
    fn into_sqlx(self) -> sqlx::Error {
        sqlx::Error::Decode(Box::new(self))
    }

    const fn as_str(&self) -> &'static str {
        match self {
            Self::UnknownKind => "unknown_kind",
            Self::AttemptOutOfRange => "attempt_out_of_range",
            Self::MissingPayload => "missing_payload",
        }
    }
}

pub(crate) fn decode_reason(error: &sqlx::Error) -> Option<&'static str> {
    let sqlx::Error::Decode(source) = error else {
        return None;
    };
    source
        .downcast_ref::<ClaimDecodeError>()
        .map(ClaimDecodeError::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_kind_slot_release_notifies_the_claim_loop() {
        let running = Arc::new(Semaphore::new(2));
        let wake = Arc::new(Notify::new());
        let first = KindSlot::take(&running, &wake).expect("a free place");
        let second = KindSlot::take(&running, &wake).expect("a free place");
        assert!(KindSlot::take(&running, &wake).is_none());

        drop(first);
        assert_eq!(running.available_permits(), 1);
        tokio::time::timeout(Duration::from_secs(1), wake.notified())
            .await
            .expect("the release of a full kind wakes the loop");

        drop(second);
        assert_eq!(running.available_permits(), 2);
        tokio::time::timeout(Duration::from_secs(1), wake.notified())
            .await
            .expect("a release also wakes the loop when another place was free");
    }
}
