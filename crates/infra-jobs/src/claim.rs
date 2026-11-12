//! CLAIM and the claim loop.

use std::sync::Arc;
use std::time::Duration;

use infra_postgres::observed;
use metrics::Unit;
use sqlx::postgres::PgConnection;
use tokio::sync::{MutexGuard, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::attempt::{self, AttemptId};
use crate::engine::{
    CANCEL_MARGIN, LEASE_RESERVE, Operation, OperationError, POLL_INTERVAL, PeerRegistration,
    Shared, backstop, observe_failure, observe_recovery,
};
use crate::kind::{JobId, Registered};
use crate::trace_context::Carrier;

/// Claim request duration through its database acknowledgement. No labels.
pub const CLAIM_DURATION_METRIC: &str = "jobs_claim_duration_seconds";
pub(crate) const CLAIM_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 8.0, 12.0,
];
/// Queue time from `not_before` to the start of the successful claim statement. Label `kind`.
pub const QUEUE_WAIT_METRIC: &str = "jobs_queue_wait_seconds";
pub(crate) const QUEUE_WAIT_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0,
    1800.0, 3600.0, 10_800.0, 21_600.0, 43_200.0, 86_400.0,
];

pub(crate) struct Claimed {
    pub(crate) registered: Arc<Registered>,
    pub(crate) attempt: AttemptId,
    /// The claim's fence: every outcome write matches it.
    pub(crate) generation: i64,
    pub(crate) payload: Vec<u8>,
    pub(crate) trace: Carrier,
    pub(crate) slot: OwnedSemaphorePermit,
    pub(crate) kind_slot: Option<KindSlot>,
}

/// One attempt's place under its kind's `max_running`.
///
/// A full kind leaves the claim loop idle with free engine slots, so the
/// release that frees a place in a full kind wakes the loop instead of
/// leaving the kind's due jobs to the next poll.
#[derive(Debug)]
pub(crate) struct KindSlot {
    permit: Option<OwnedSemaphorePermit>,
    wake: Arc<Notify>,
}

impl KindSlot {
    /// The claim loop is the only taker and asked for no more than the free
    /// part, so a place is free for every row it drew.
    fn take(running: &Arc<Semaphore>, wake: &Arc<Notify>) -> Option<Self> {
        let permit = Arc::clone(running).try_acquire_owned().ok()?;
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
        let was_full = permit.semaphore().available_permits() == 0;
        // Released before the wake, so the woken loop sees the free place.
        drop(permit);
        if was_full {
            self.wake.notify_one();
        }
    }
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

/// Claim until `stop` fires. Closes the attempt tracker on every exit.
pub(crate) async fn run_claim_loop(
    shared: Arc<Shared>,
    stop: CancellationToken,
    failure: CancellationToken,
    peer: PeerRegistration,
) {
    let _close = CloseTracker(&shared.attempt_tracker);
    // Deregister before releasing waiters on the attempt tracker.
    let _peer = peer;
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
        // The turn covers the statement's pooled connection and ends with the
        // round it produced; dispatching those rows happens without it.
        let round = {
            let Some(_turn) = statement_turn(&shared, &stop).await else {
                return;
            };
            if stop.is_cancelled() {
                return;
            }
            last_claim = Instant::now();
            send_claim(&shared, slot_count(requested)).await
        };
        next = finish_round(&shared, &mut slots, requested, round, &stop, &failure);
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
pub(crate) const CLAIM_COOLDOWN: Duration = Duration::from_millis(25);

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
    Known { sent: Instant, rows: Vec<Drawn> },
    Failed(OperationError),
}

async fn send_claim(shared: &Shared, requested: i64) -> ClaimRound {
    let sent = Instant::now();
    let result = backstop(async {
        let mut connection = infra_postgres::acquire(&shared.pool, "claim jobs")
            .await
            .map_err(OperationError::Acquire)?;
        Ok(claim(&mut connection, shared, requested).await?)
    })
    .await;
    metrics::histogram!(CLAIM_DURATION_METRIC).record(sent.elapsed().as_secs_f64());
    match result {
        Ok(rows) => ClaimRound::Known { sent, rows },
        Err(error) => ClaimRound::Failed(error),
    }
}

/// Claim due pending jobs and expired running jobs, up to the free slots.
///
/// The same statement settles what a claim finds: a job whose attempt budget
/// is spent fails as `exhausted` instead of running, a rescued expired lease
/// records why, and every running claim spends one attempt and takes a new
/// claim generation. A snooze or release gives the attempt back
/// ([`crate::attempt`]).
///
/// Each per-kind scan locks rows while it reads them (`SKIP LOCKED` inside the
/// lateral), so a row another session holds is skipped and the scan moves on.
/// Choosing ids first and locking them afterwards hands concurrent workers the
/// same few ids: the losers claim nothing until the next poll.
///
/// A kind's `claim_limit` is the free slots, or the free part of its
/// `max_running` when that is smaller. It bounds each scan and, through the
/// rank, the kind's due and expired rows together.
///
/// The two state literals are the access path as much as the filter:
/// `state = 'pending'` and `state = 'running'` prove the partial
/// `background_jobs_pending` and `background_jobs_running` predicates, so each
/// scan reads its kind's own index. A bind or a computed state would forfeit
/// that proof and leave a sequential scan. The fenced
/// [`complete_query`](crate::attempt::complete_query) write wants its primary
/// key instead, so it says the equivalent `claim_expires_at IS NOT NULL`, which
/// the table's CHECK makes equal to `state = 'running'`.
#[expect(
    clippy::too_many_lines,
    reason = "one statement, and its binds beside it"
)]
async fn claim(
    connection: &mut PgConnection,
    shared: &Shared,
    requested: i64,
) -> Result<Vec<Drawn>, sqlx::Error> {
    let binds = policy_binds(&shared.registry, requested);
    observed(
        "claim jobs",
        sqlx::query_as!(
            ClaimRow,
            "WITH policy AS ( \
                 SELECT policy.kind, policy.max_attempts, policy.lease_offset_micros, \
                        policy.claim_limit \
                 FROM unnest($1::text[], $2::smallint[], $3::bigint[], $6::bigint[]) \
                     AS policy (kind, max_attempts, lease_offset_micros, claim_limit) \
             ), \
             candidates AS ( \
                 SELECT candidate.id, candidate.not_before, candidate.attempts, candidate.state, \
                        candidate.claim_expires_at, policy.max_attempts, \
                        policy.lease_offset_micros, policy.kind, policy.claim_limit \
                 FROM policy \
                 CROSS JOIN LATERAL ( \
                     SELECT job.id, job.not_before, job.attempts, job.state, \
                            job.claim_expires_at \
                     FROM background_jobs AS job \
                     WHERE job.state = 'pending' \
                       AND job.kind = policy.kind \
                       AND job.not_before <= statement_timestamp() \
                     ORDER BY job.not_before, job.id \
                     LIMIT policy.claim_limit \
                     FOR UPDATE SKIP LOCKED \
                 ) AS candidate \
                 UNION ALL \
                 SELECT candidate.id, candidate.not_before, candidate.attempts, candidate.state, \
                        candidate.claim_expires_at, policy.max_attempts, \
                        policy.lease_offset_micros, policy.kind, policy.claim_limit \
                 FROM policy \
                 CROSS JOIN LATERAL ( \
                     SELECT job.id, job.not_before, job.attempts, job.state, \
                            job.claim_expires_at \
                     FROM background_jobs AS job \
                     WHERE job.state = 'running' \
                       AND job.kind = policy.kind \
                       AND job.claim_expires_at <= statement_timestamp() \
                     ORDER BY job.not_before, job.id \
                     LIMIT policy.claim_limit \
                     FOR UPDATE SKIP LOCKED \
                 ) AS candidate \
             ), \
             ranked AS ( \
                 SELECT candidates.id, candidates.not_before, candidates.attempts, \
                        candidates.state, candidates.claim_expires_at, \
                        candidates.max_attempts, candidates.lease_offset_micros, \
                        candidates.claim_limit, \
                        row_number() OVER ( \
                            PARTITION BY candidates.kind \
                            ORDER BY candidates.not_before, candidates.id \
                        ) AS position \
                 FROM candidates \
             ), \
             picked AS ( \
                 SELECT ranked.id, ranked.attempts, ranked.lease_offset_micros, \
                        ranked.attempts >= ranked.max_attempts AS exhausted, \
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
                                 ELSE picked.attempts + 1 END, \
                 attempted_by = CASE WHEN picked.exhausted THEN job.attempted_by \
                                     ELSE $5 END, \
                 error_summary = CASE WHEN picked.rescued \
                                      THEN 'lease expired; rescued' \
                                      WHEN picked.exhausted \
                                      THEN COALESCE(job.error_summary, 'attempt budget spent') \
                                      ELSE job.error_summary END, \
                 errors = CASE WHEN picked.rescued \
                               THEN job.errors || jsonb_build_object( \
                                        'attempt', picked.attempts, 'at', statement_timestamp(), \
                                        'error', 'lease expired; rescued') \
                               ELSE job.errors END, \
                 claim_generation = nextval('background_jobs_claim_generation') \
             FROM picked \
             WHERE job.id = picked.id \
                AND ((job.state = 'pending' AND job.not_before <= statement_timestamp()) \
                     OR (job.state = 'running' AND job.claim_expires_at <= statement_timestamp())) \
             RETURNING job.id, job.kind, (job.state = 'failed') AS \"exhausted!\", job.attempts, \
                       job.claim_generation, \
                       CASE WHEN job.state = 'running' THEN job.payload::text END AS payload, \
                       job.trace_context AS trace_parent, job.trace_state, job.error_summary, \
                       EXTRACT(EPOCH FROM statement_timestamp() - job.not_before)::double precision \
                           AS \"queue_wait_seconds!\"",
            &binds.names as _,
            &binds.max_attempts,
            &binds.lease_offsets_micros,
            requested,
            shared.worker_id,
            &binds.claim_limits,
        )
        .try_map(|row| row.into_drawn(&shared.registry))
        .fetch_all(&mut *connection),
    )
    .await
}

/// The per-kind policy the claim statement unnests, one parallel array per
/// column.
struct PolicyBinds<'registry> {
    names: Vec<&'registry str>,
    max_attempts: Vec<i16>,
    lease_offsets_micros: Vec<i64>,
    claim_limits: Vec<i64>,
}

/// Each kind's name, attempt budget, lease offset, and how many rows this claim
/// may draw of it.
fn policy_binds(registry: &crate::Kinds, requested: i64) -> PolicyBinds<'_> {
    let mut binds = PolicyBinds {
        names: Vec::new(),
        max_attempts: Vec::new(),
        lease_offsets_micros: Vec::new(),
        claim_limits: Vec::new(),
    };
    for registered in registry.iter() {
        binds.names.push(registered.name);
        binds
            .max_attempts
            .push(i16::try_from(registered.policy.max_attempts).unwrap_or(i16::MAX));
        binds
            .lease_offsets_micros
            .push(saturating_micros(lease_offset(registered.policy.timeout)));
        binds
            .claim_limits
            .push(registered.running.as_ref().map_or(requested, |running| {
                requested.min(slot_count(running.available_permits()))
            }));
    }
    binds
}

/// The claim's lease past the statement timestamp, and past the send instant
/// for the local deadline: the kind's timeout plus [`LEASE_RESERVE`].
///
/// The statement and the local deadline share this offset, so the local
/// deadline keeps its [`CANCEL_MARGIN`] lead over the database expiry.
fn lease_offset(timeout: Duration) -> Duration {
    timeout.saturating_add(LEASE_RESERVE)
}

fn saturating_micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}

/// A slot count as a statement bind. Counts come from `u32` limits, so the
/// fallback never runs; it would claim one row rather than unbounded ones.
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
        ClaimRound::Known { sent, rows } => {
            observe_recovery(shared, Operation::Claim);
            let next = if rows.len() == requested {
                Next::Now
            } else if rows.is_empty() {
                Next::Idle
            } else {
                Next::Soon
            };
            dispatch_known(shared, slots, rows, sent, stop, failure);
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
                queue_wait,
            } => {
                let deadline = local_deadline(sent, lease_offset);
                let Some(slot) = slots.split(1) else {
                    tracing::error!(
                        job.id = %attempt.job_id,
                        job.kind = attempt.kind,
                        "job_claim_exceeded_slots"
                    );
                    break;
                };
                let kind_slot = registered
                    .running
                    .as_ref()
                    .and_then(|running| KindSlot::take(running, &shared.wake));
                registered.metrics().queue_wait.record(queue_wait.max(0.0));
                let guard = crate::engine::SupervisorGuard::new(
                    stop.clone(),
                    failure.clone(),
                    attempt.job_id,
                    attempt.kind,
                );
                let claimed = Claimed {
                    registered,
                    attempt,
                    generation,
                    payload: payload.into_bytes(),
                    trace,
                    slot,
                    kind_slot,
                };
                shared.attempt_tracker.spawn(attempt::supervise(
                    Arc::clone(shared),
                    claimed,
                    deadline,
                    guard,
                ));
            }
        }
    }
}

/// The point this worker stops writing: the send instant plus the statement's
/// own lease offset, [`CANCEL_MARGIN`] earlier than the database expiry another
/// worker may rescue at.
fn local_deadline(sent: Instant, lease_offset: Duration) -> Instant {
    sent.checked_add(lease_offset)
        .and_then(|deadline| deadline.checked_sub(CANCEL_MARGIN))
        .unwrap_or(sent)
}

enum Drawn {
    Running {
        registered: Arc<Registered>,
        attempt: AttemptId,
        generation: i64,
        payload: String,
        trace: Carrier,
        /// The lease past the send instant, the same value the statement bound.
        lease_offset: Duration,
        queue_wait: f64,
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
            return Err(decode_error("unknown job kind"));
        };
        let attempt = AttemptId {
            job_id: JobId(self.id),
            kind: registered.name,
            number: u16::try_from(self.attempts)
                .map_err(|_| decode_error("attempt does not fit"))?,
        };
        if self.exhausted {
            return Ok(Drawn::Exhausted {
                attempt,
                error_summary: self.error_summary,
            });
        }
        let Some(payload) = self.payload else {
            return Err(decode_error("running claim has no payload"));
        };
        Ok(Drawn::Running {
            lease_offset: lease_offset(registered.policy.timeout),
            registered: Arc::clone(registered),
            attempt,
            generation: self.claim_generation,
            payload,
            trace: Carrier::from_stored(self.trace_parent, self.trace_state),
            queue_wait: self.queue_wait_seconds,
        })
    }
}
fn decode_error(message: &'static str) -> sqlx::Error {
    sqlx::Error::Decode(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_the_release_that_frees_a_full_kind_wakes_the_claim_loop() {
        let running = Arc::new(Semaphore::new(2));
        let wake = Arc::new(Notify::new());
        let first = KindSlot::take(&running, &wake).expect("a free place");
        let second = KindSlot::take(&running, &wake).expect("a free place");
        assert!(KindSlot::take(&running, &wake).is_none());

        // The kind was full: the place is free again before the wake is stored.
        drop(first);
        assert_eq!(running.available_permits(), 1);
        tokio::time::timeout(Duration::from_secs(1), wake.notified())
            .await
            .expect("the release of a full kind wakes the loop");

        // A place was already free: the loop saw it in its last claim.
        drop(second);
        assert_eq!(running.available_permits(), 2);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), wake.notified())
                .await
                .is_err()
        );
    }
}
