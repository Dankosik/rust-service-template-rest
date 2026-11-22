use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use infra_postgres::observed;
use metrics::Unit;
use sqlx::postgres::PgConnection;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, MissedTickBehavior};

use crate::attempt::{self, AttemptId, ClaimFence};
use crate::engine::{
    CANCEL_MARGIN, EngineSignals, LEASE_RESERVE, Operation, OperationError, POLL_INTERVAL,
    WorkerGroup, backstop, observe_failure, observe_recovery,
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

pub(crate) struct RunningJob {
    pub(crate) registered: Arc<Registered>,
    pub(crate) attempt: AttemptId,
    pub(crate) fence: ClaimFence,
    pub(crate) payload: Result<Vec<u8>, crate::JobError>,
    pub(crate) trace: Carrier,
}

pub(crate) struct Claimed {
    pub(crate) job: RunningJob,
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
        drop(self.permit.take());
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

pub(crate) async fn run_claim_loop(group: Arc<WorkerGroup>, signals: Arc<EngineSignals>) {
    let stop = &signals.stop;
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let policy = PolicyBinds::new(&group.registry);
    let mut wait = ClaimWait::WakeOrTick;
    let mut last_claim = Instant::now();
    loop {
        let Some((slots, turn)) = stop
            .run_until_cancelled(async {
                match wait {
                    ClaimWait::Immediate => {}
                    ClaimWait::Cooldown => {
                        tokio::time::sleep_until(last_claim + CLAIM_COOLDOWN).await;
                    }
                    ClaimWait::WakeOrTick => {
                        wait_for_wake_or_tick(&group, &mut ticker).await;
                        tokio::time::sleep_until(last_claim + CLAIM_COOLDOWN).await;
                    }
                }
                let slots = take_slots(&group).await?;
                let turn = group.statement_turn.lock().await;
                Some((slots, turn))
            })
            .await
            .flatten()
        else {
            return;
        };
        let requested = slots.num_permits();
        let round = {
            let _turn = turn;
            if stop.is_cancelled() {
                return;
            }
            last_claim = Instant::now();
            send_claim(&group, &policy, slot_count(requested)).await
        };
        wait = match round {
            Ok(ClaimRound {
                claim_started_at,
                outcomes,
            }) => {
                observe_recovery(&group, Operation::Claim);
                let next_wait = ClaimWait::after_claim(outcomes.len(), requested);
                dispatch_outcomes(&group, slots, outcomes, claim_started_at, &signals);
                next_wait
            }
            Err(error) => {
                observe_failure(&group, Operation::Claim, &error);
                ClaimWait::WakeOrTick
            }
        };
    }
}

#[derive(Clone, Copy)]
enum ClaimWait {
    Immediate,
    Cooldown,
    WakeOrTick,
}

impl ClaimWait {
    const fn after_claim(claimed: usize, requested: usize) -> Self {
        if claimed == requested {
            Self::Immediate
        } else if claimed == 0 {
            Self::WakeOrTick
        } else {
            Self::Cooldown
        }
    }
}

pub(crate) const CLAIM_COOLDOWN: Duration = Duration::from_millis(25);

async fn wait_for_wake_or_tick(group: &WorkerGroup, ticker: &mut tokio::time::Interval) {
    tokio::select! {
        biased;
        () = group.claim_wake.notified() => ticker.reset(),
        _ = ticker.tick() => {}
    }
}

async fn take_slots(group: &WorkerGroup) -> Option<OwnedSemaphorePermit> {
    let mut permits = Arc::clone(&group.slots).acquire_owned().await.ok()?;
    if let Ok(count) = u32::try_from(group.slots.available_permits())
        && count > 0
        && let Ok(more) = Arc::clone(&group.slots).try_acquire_many_owned(count)
    {
        permits.merge(more);
    }
    Some(permits)
}

struct ClaimRound {
    claim_started_at: Instant,
    outcomes: Vec<ClaimOutcome>,
}

async fn send_claim(
    group: &WorkerGroup,
    policy: &PolicyBinds<'_>,
    requested: i64,
) -> Result<ClaimRound, OperationError> {
    let claim_started_at = Instant::now();
    let result = backstop(async {
        let mut connection = group.acquire("claim jobs").await?;
        Ok(claim(&mut connection, group.worker_id, policy, requested).await?)
    })
    .await;
    metrics::histogram!(CLAIM_DURATION_METRIC).record(claim_started_at.elapsed());
    result.map(|outcomes| ClaimRound {
        claim_started_at,
        outcomes,
    })
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
) -> Result<Vec<ClaimOutcome>, sqlx::Error> {
    let claim_limits = policy.claim_limits(requested);
    let rescued_summary = "lease expired; rescued";
    observed!(
        "claim jobs",
        sqlx::query_as!(
            ClaimRow,
            "WITH policy AS ( \
                 SELECT policy.kind, policy.max_attempts, policy.lease_offset_micros, \
                        policy.claim_limit, policy.is_outbox \
                 FROM unnest($1::text[], $2::smallint[], $3::bigint[], $6::bigint[], $8::bool[]) \
                     AS policy (kind, max_attempts, lease_offset_micros, claim_limit, is_outbox) \
             ), \
             candidates AS ( \
                 SELECT candidate.id, candidate.not_before, candidate.attempts, candidate.state, \
                        candidate.claim_expires_at, policy.max_attempts, \
                        policy.lease_offset_micros, policy.kind, policy.claim_limit, policy.is_outbox \
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
                        policy.lease_offset_micros, policy.kind, policy.claim_limit, policy.is_outbox \
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
                        candidates.claim_limit, candidates.is_outbox, \
                        row_number() OVER ( \
                            PARTITION BY candidates.kind \
                            ORDER BY candidates.not_before, candidates.id \
                        ) AS position \
                 FROM candidates \
             ), \
             picked AS ( \
                 SELECT ranked.id, ranked.attempts, ranked.lease_offset_micros, ranked.max_attempts, \
                        ranked.is_outbox, \
                        ranked.attempts >= ranked.max_attempts AND NOT ranked.is_outbox AS exhausted, \
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
                 claim_generation = nextval('background_jobs_claim_generation'), \
                 claim_token = gen_random_uuid() \
             FROM picked \
             WHERE job.id = picked.id \
                AND ((job.state = 'pending' AND job.not_before <= statement_timestamp()) \
                     OR (job.state = 'running' AND job.claim_expires_at <= statement_timestamp())) \
             RETURNING job.id, job.kind, (job.state = 'failed') AS \"exhausted!\", job.attempts, \
                       job.claim_generation, job.claim_token AS \"claim_token!\", \
                       CASE WHEN job.state <> 'running' THEN NULL \
                            WHEN picked.is_outbox THEN outbox_verified_payload( \
                                job.id, job.unique_key, job.predecessor_job_id, job.payload_text, job.payload) \
                            ELSE COALESCE(job.payload_text, job.payload::text) END AS payload, \
                       job.trace_context AS trace_parent, job.trace_state, \
                       CASE WHEN job.state = 'failed' THEN job.error_summary END AS error_summary, \
                       EXTRACT(EPOCH FROM statement_timestamp() - job.not_before)::double precision \
                           AS \"queue_wait_seconds!\"",
            &policy.names as _,
            &policy.max_attempts,
            &policy.lease_offsets_micros,
            requested,
            worker_id,
            &claim_limits[..],
            rescued_summary,
            &policy.is_outbox[..],
            i32::from(crate::kind::MAX_ATTEMPTS),
        )
        .try_map(|row| row.into_outcome(policy.registry).map_err(ClaimDecodeError::into_sqlx))
        .fetch_all(&mut *connection),
    )
    .await
}

struct PolicyBinds<'a> {
    registry: &'a crate::Kinds,
    names: Vec<&'static str>,
    max_attempts: Vec<i16>,
    lease_offsets_micros: Vec<i64>,
    is_outbox: Vec<bool>,
}

impl<'a> PolicyBinds<'a> {
    fn new(registry: &'a crate::Kinds) -> Self {
        let (names, max_attempts, lease_offsets_micros, is_outbox) = registry
            .iter()
            .map(|registered| {
                (
                    registered.name,
                    i16::try_from(registered.policy.max_attempts).unwrap_or(i16::MAX),
                    saturating_micros(lease_offset(registered.policy.timeout)),
                    registered.mode.is_outbox(),
                )
            })
            .collect();
        Self {
            registry,
            names,
            max_attempts,
            lease_offsets_micros,
            is_outbox,
        }
    }

    fn claim_limits(&self, requested: i64) -> smallvec::SmallVec<[i64; 8]> {
        self.registry
            .iter()
            .map(|registered| {
                registered
                    .running_slots
                    .as_ref()
                    .map_or(requested, |slots| {
                        requested.min(slot_count(slots.available_permits()))
                    })
            })
            .collect()
    }
}

fn lease_offset(timeout: Duration) -> Duration {
    timeout.saturating_add(LEASE_RESERVE)
}

fn slot_count(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(1)
}

fn dispatch_outcomes(
    group: &Arc<WorkerGroup>,
    mut slots: OwnedSemaphorePermit,
    outcomes: Vec<ClaimOutcome>,
    claim_started_at: Instant,
    signals: &Arc<EngineSignals>,
) {
    for outcome in outcomes {
        match outcome {
            ClaimOutcome::Exhausted {
                registered,
                attempt,
                error_summary,
            } => {
                attempt::observe_exhausted(registered.metrics(), &attempt, &error_summary);
            }
            ClaimOutcome::Running {
                job,
                lease_offset,
                queue_wait_seconds,
            } => {
                let supervisor_deadline = supervisor_deadline(claim_started_at, lease_offset);
                let Some(slot) = slots.split(1) else {
                    tracing::error!(
                        job.id = %job.attempt.job_id,
                        job.kind = job.attempt.kind,
                        "job_claim_exceeded_slots"
                    );
                    break;
                };
                let kind_slot = job
                    .registered
                    .running_slots
                    .as_ref()
                    .and_then(|running_slots| KindSlot::take(running_slots, &group.claim_wake));
                job.registered
                    .metrics()
                    .queue_wait
                    .record(queue_wait_seconds.max(0.0));
                let guard = crate::engine::SupervisorGuard::new(
                    Arc::clone(&group.lifecycle.drain),
                    Arc::clone(signals),
                    job.attempt.job_id,
                    job.attempt.kind,
                );
                let claimed = Claimed {
                    job,
                    slot,
                    kind_slot,
                };
                tokio::spawn(attempt::supervise(
                    Arc::clone(group),
                    claimed,
                    supervisor_deadline,
                    guard,
                ));
            }
        }
    }
}

fn supervisor_deadline(claim_started_at: Instant, lease_offset: Duration) -> Instant {
    claim_started_at
        .checked_add(lease_offset)
        .and_then(|deadline| deadline.checked_sub(CANCEL_MARGIN))
        .unwrap_or(claim_started_at)
}

enum ClaimOutcome {
    Running {
        job: RunningJob,
        lease_offset: Duration,
        queue_wait_seconds: f64,
    },
    Exhausted {
        registered: Arc<Registered>,
        attempt: AttemptId,
        error_summary: String,
    },
}

struct ClaimRow {
    id: uuid::Uuid,
    kind: String,
    exhausted: bool,
    attempts: i16,
    claim_generation: i64,
    claim_token: uuid::Uuid,
    payload: Option<String>,
    trace_parent: Option<String>,
    trace_state: Option<String>,
    error_summary: Option<String>,
    queue_wait_seconds: f64,
}

impl ClaimRow {
    fn into_outcome(self, registry: &crate::Kinds) -> Result<ClaimOutcome, ClaimDecodeError> {
        let registered = registry
            .get(&self.kind)
            .ok_or(ClaimDecodeError::UnknownKind)?;
        let attempt = AttemptId {
            job_id: JobId(self.id),
            kind: registered.name,
            number: u16::try_from(self.attempts)
                .map_err(|_| ClaimDecodeError::AttemptOutOfRange)?,
        };
        if self.exhausted {
            return Ok(ClaimOutcome::Exhausted {
                registered: Arc::clone(registered),
                attempt,
                error_summary: self.error_summary.unwrap_or_default(),
            });
        }
        let payload = match self.payload {
            Some(payload) => Ok(payload.into_bytes()),
            None => Err(registered
                .mode
                .missing_payload_error()
                .ok_or(ClaimDecodeError::MissingPayload)?),
        };
        Ok(ClaimOutcome::Running {
            lease_offset: lease_offset(registered.policy.timeout),
            job: RunningJob {
                registered: Arc::clone(registered),
                fence: ClaimFence {
                    job_id: attempt.job_id,
                    generation: self.claim_generation,
                    token: self.claim_token,
                },
                attempt,
                payload,
                trace: Carrier::from_stored(self.trace_parent, self.trace_state),
            },
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
