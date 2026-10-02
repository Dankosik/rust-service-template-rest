//! One supervisor owns its handler and fixed queue outcome through cleanup.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::FutureExt as _;
use infra_postgres::observed;
use sqlx::postgres::PgConnection;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::engine::{
    Operation, OperationError, RECORD_RETRY_INTERVAL, Shared, backstop, observe_failure,
    observe_recovery,
};
use crate::kind::{Attempt, Disposition, HandlerFuture, JobError, JobId, Policy};

const COOPERATIVE_GRACE: Duration = Duration::from_millis(100);

/// The longest stored failure summary, in bytes.
pub(crate) const ERROR_SUMMARY_MAX_BYTES: usize = 1024;
/// Observed handler results and claim-time exhaustions. Labels `kind`, `outcome`.
pub(crate) const ATTEMPTS_METRIC: &str = "jobs_attempts_total";
/// Queue-write acknowledgement, not attribution. Labels `kind`, `disposition`.
pub(crate) const PERSISTENCE_METRIC: &str = "jobs_persistence_total";
/// Handler run time. Label `kind`.
pub const ATTEMPT_DURATION_METRIC: &str = "jobs_attempt_duration_seconds";
/// Histogram buckets for [`ATTEMPT_DURATION_METRIC`], in seconds.
pub const ATTEMPT_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0,
    1800.0, 3600.0,
];

/// The queue transition a known attempt result asks for.
enum Transition {
    Complete,
    Retry {
        summary: String,
        base_micros: f64,
        floor_micros: i64,
        timed_out: bool,
    },
    Fail {
        reason: Failure,
        summary: String,
    },
    Snooze {
        delay_micros: i64,
    },
    Release,
}

#[derive(Clone, Copy, Debug)]
enum Failure {
    Exhausted,
    Permanent,
}

impl Failure {
    const fn label(self) -> &'static str {
        match self {
            Self::Exhausted => "exhausted",
            Self::Permanent => "permanent",
        }
    }
}

impl Transition {
    const fn label(&self) -> &'static str {
        match self {
            Self::Complete => "completed",
            Self::Retry {
                timed_out: true, ..
            } => "timeout",
            Self::Retry { .. } => "retry",
            Self::Fail { reason, .. } => reason.label(),
            Self::Snooze { .. } => "snoozed",
            Self::Release => "cancelled",
        }
    }
}

/// Describe attempt metrics once during engine startup.
pub(crate) fn describe_metrics() {
    metrics::describe_counter!(
        ATTEMPTS_METRIC,
        "Observed attempt dispositions, independent of queue-write acknowledgement"
    );
    metrics::describe_histogram!(
        ATTEMPT_DURATION_METRIC,
        "Observed handler duration in seconds"
    );
    metrics::describe_counter!(
        PERSISTENCE_METRIC,
        "Queue outcome acknowledgement disposition"
    );
}

/// Metric handles of the completed-attempt path for one kind, registered
/// once at engine start instead of looked up by name for every attempt.
pub(crate) struct KindMetrics {
    completed: metrics::Counter,
    duration: metrics::Histogram,
    applied: metrics::Counter,
    pub(crate) queue_wait: metrics::Histogram,
}

impl KindMetrics {
    pub(crate) fn new(kind: &'static str) -> Self {
        Self {
            completed: metrics::counter!(ATTEMPTS_METRIC, "kind" => kind, "outcome" => "completed"),
            duration: metrics::histogram!(ATTEMPT_DURATION_METRIC, "kind" => kind),
            applied: metrics::counter!(PERSISTENCE_METRIC, "kind" => kind, "disposition" => "applied"),
            queue_wait: metrics::histogram!(crate::claim::QUEUE_WAIT_METRIC, "kind" => kind),
        }
    }
}

pub(crate) fn kind_metrics<'a>(shared: &'a Shared, kind: &str) -> Option<&'a KindMetrics> {
    shared
        .registry
        .get(kind)
        .and_then(|registered| registered.metrics.get())
}

/// Admission stays occupied until both the supervisor and its bookkeeping retire.
struct AttemptSlots {
    _global: tokio::sync::OwnedSemaphorePermit,
    _kind: Option<crate::claim::KindSlot>,
}

struct AttemptId {
    id: JobId,
    generation: i64,
    kind: &'static str,
    attempt: u16,
}

/// Claim-time exhaustion is observed only after its transaction is acknowledged.
pub(crate) fn record_exhausted(id: JobId, kind: &'static str, attempt: u16, summary: Option<&str>) {
    record(
        None,
        &AttemptId {
            id,
            generation: 0,
            kind,
            attempt,
        },
        &Transition::Fail {
            reason: Failure::Exhausted,
            summary: summary.unwrap_or("").to_owned(),
        },
        None,
    );
}

fn record(
    handles: Option<&KindMetrics>,
    attempt: &AttemptId,
    transition: &Transition,
    ran: Option<Duration>,
) {
    match handles {
        Some(handles) if matches!(transition, Transition::Complete) => {
            handles.completed.increment(1);
        }
        _ => metrics::counter!(ATTEMPTS_METRIC, "kind" => attempt.kind, "outcome" => transition.label())
            .increment(1),
    }
    if let Some(ran) = ran {
        match handles {
            Some(handles) => handles.duration.record(ran.as_secs_f64()),
            None => metrics::histogram!(ATTEMPT_DURATION_METRIC, "kind" => attempt.kind)
                .record(ran.as_secs_f64()),
        }
    }
    match transition {
        Transition::Complete => {
            tracing::debug!(job.id = %attempt.id, job.kind = attempt.kind, job.attempt = attempt.attempt, "job_attempt_completed");
        }
        Transition::Snooze { .. } | Transition::Release => {
            tracing::info!(job.id = %attempt.id, job.kind = attempt.kind, outcome = transition.label(), "job_attempt_finished");
        }
        Transition::Retry { summary, .. } => {
            tracing::info!(job.id = %attempt.id, job.kind = attempt.kind, job.attempt = attempt.attempt, error = summary.as_str(), "job_attempt_failed");
        }
        Transition::Fail { summary, .. } => {
            tracing::warn!(job.id = %attempt.id, job.kind = attempt.kind, job.attempts = attempt.attempt, job.failure_reason = transition.label(), error = summary.as_str(), "job_failed");
        }
    }
}

pub(crate) async fn supervise(
    shared: Arc<Shared>,
    claimed: crate::claim::Claimed,
    deadline: Instant,
) {
    let crate::claim::Claimed {
        id,
        generation,
        kind,
        attempt,
        payload,
        trace_context: parent,
        trace_state,
        slot,
        kind_slot,
    } = claimed;
    let span = attempt_span(id, kind, attempt);
    crate::trace_context::link(&span, parent.as_deref(), trace_state.as_deref());
    drop(parent);
    drop(trace_state);
    run_attempt(
        &shared,
        AttemptId {
            id,
            generation,
            kind,
            attempt,
        },
        payload,
        deadline,
        Arc::new(AttemptSlots {
            _global: slot,
            _kind: kind_slot,
        }),
    )
    .instrument(span)
    .await;
}

/// One span per attempt, named `process <kind>` in an exported trace. The
/// kind set is the registered one, so the name stays low-cardinality.
fn attempt_span(id: JobId, kind: &'static str, attempt: u16) -> tracing::Span {
    tracing::info_span!(
        "job_attempt",
        otel.name = %format_args!("process {kind}"),
        otel.kind = "consumer",
        otel.status_code = tracing::field::Empty,
        job.id = %id,
        job.kind = kind,
        job.attempt = u64::from(attempt),
        outcome = tracing::field::Empty,
    )
}

/// The attempt's `outcome`, the `jobs_attempts_total` label. An attempt that
/// spent its unit on a failure is an error; a snooze or a release is not.
fn record_on_span(span: &tracing::Span, transition: &Transition) {
    span.record("outcome", transition.label());
    if matches!(
        transition,
        Transition::Retry { .. } | Transition::Fail { .. }
    ) {
        span.record("otel.status_code", "ERROR");
    }
}

async fn run_attempt(
    shared: &Shared,
    attempt: AttemptId,
    payload: Vec<u8>,
    deadline: Instant,
    slots: Arc<AttemptSlots>,
) {
    if expired(shared, deadline) {
        uncertain(shared, &attempt);
        return;
    }
    let Some(registered) = shared.registry.get(attempt.kind) else {
        return;
    };
    let policy = registered.policy;
    let attempt_deadline = Instant::now()
        .checked_add(policy.timeout)
        .unwrap_or(deadline)
        .min(deadline);
    let cancel = CancellationToken::new();
    let prepared = registered.dispatch.prepare(
        Attempt {
            id: attempt.id,
            number: attempt.attempt,
            generation: attempt.generation,
            deadline: attempt_deadline,
            cancellation: cancel.clone(),
            pool: shared.pool.clone(),
        },
        &payload,
    );
    drop(payload);
    let (ended, ran) = match prepared {
        Err(error) => (Ended::Payload(error), None),
        Ok(future) => {
            let started = Instant::now();
            let Some(ended) = drive(shared, future, cancel, attempt_deadline, deadline).await
            else {
                uncertain(shared, &attempt);
                return;
            };
            (ended, Some(started.elapsed()))
        }
    };
    let transition = map_outcome(attempt.kind, attempt.attempt, policy, ended);
    if matches!(transition, Transition::Release) {
        shared.counters.cancelled.fetch_add(1, Ordering::Relaxed);
    } else {
        shared
            .counters
            .known_results
            .fetch_add(1, Ordering::Relaxed);
    }
    record_on_span(&tracing::Span::current(), &transition);
    record(registered.metrics.get(), &attempt, &transition, ran);
    persist(shared, &attempt, &transition, deadline, &slots).await;
}

/// `future`'s output, or `None` once the attempt's local deadline passes or cleanup ends.
async fn within<T>(shared: &Shared, local: Instant, future: impl Future<Output = T>) -> Option<T> {
    tokio::select! {
        biased;
        value = future => Some(value),
        () = shared.cleanup_ended() => None,
        () = tokio::time::sleep_until(local) => None,
    }
}

fn expired(shared: &Shared, local: Instant) -> bool {
    Instant::now() >= local || shared.cleanup_has_ended()
}

/// Poll a ready result before cancellation; it wins as is. After cancellation
/// only success wins (see [`ended_after_cancel`]).
async fn drive(
    shared: &Shared,
    future: HandlerFuture,
    cancel: CancellationToken,
    attempt_deadline: Instant,
    local: Instant,
) -> Option<Ended> {
    // The handler runs on the supervisor's task; dropping it is the abort.
    let mut run = std::pin::pin!(std::panic::AssertUnwindSafe(future).catch_unwind());
    let reason = tokio::select! {
        biased;
        result = &mut run => return Some(ended_from(result)),
        () = shared.force.cancelled() => Ended::Cancelled,
        () = tokio::time::sleep_until(attempt_deadline) => Ended::Timeout,
    };
    cancel.cancel();
    let grace = Instant::now()
        .checked_add(COOPERATIVE_GRACE)
        .unwrap_or(local)
        .min(local);
    if let Some(result) = within(shared, grace, &mut run).await {
        return Some(ended_after_cancel(&result, reason));
    }
    (!expired(shared, local)).then_some(reason)
}

/// A result that joins after the supervisor cancelled the handler. Success
/// means the work finished, so it is kept. An error, snooze, or panic is how
/// the handler reacted to the cancellation, so it takes the cancellation's
/// `reason`: a forced drain still releases the job and refunds its attempt.
fn ended_after_cancel(result: &Result<Result<(), JobError>, Panicked>, reason: Ended) -> Ended {
    if matches!(result, Ok(Ok(()))) {
        Ended::Success
    } else {
        reason
    }
}

type Panicked = Box<dyn std::any::Any + Send>;

fn ended_from(result: Result<Result<(), JobError>, Panicked>) -> Ended {
    match result {
        Ok(Ok(())) => Ended::Success,
        Ok(Err(error)) => Ended::Error(error),
        Err(_) => Ended::Panic,
    }
}

async fn persist(
    shared: &Shared,
    attempt: &AttemptId,
    transition: &Transition,
    local: Instant,
    slots: &Arc<AttemptSlots>,
) {
    let operation = if matches!(transition, Transition::Release) {
        Operation::Release
    } else {
        Operation::Record
    };
    loop {
        if expired(shared, local) {
            break;
        }
        let sent = if matches!(transition, Transition::Complete) {
            within(
                shared,
                local,
                complete_batched(shared, attempt, local, slots),
            )
            .await
        } else {
            within(shared, local, send_outcome(shared, attempt, transition)).await
        };
        match sent {
            None => break,
            Some(Ok(rows)) => {
                observe_recovery(shared, operation);
                let disposition = if rows == 1 { "applied" } else { "unchanged" };
                match kind_metrics(shared, attempt.kind) {
                    Some(handles) if rows == 1 => handles.applied.increment(1),
                    _ => persistence(attempt.kind, disposition),
                }
                tracing::debug!(
                    job.kind = attempt.kind,
                    disposition,
                    "job_persistence_finished"
                );
                if rows == 1 && matches!(transition, Transition::Release) {
                    shared.counters.released.fetch_add(1, Ordering::Relaxed);
                }
                return;
            }
            Some(Err(Some(error))) => observe_failure(shared, operation, &error),
            Some(Err(None)) => {}
        }
        let until = Instant::now()
            .checked_add(RECORD_RETRY_INTERVAL)
            .unwrap_or(local);
        if within(shared, local, tokio::time::sleep_until(until))
            .await
            .is_none()
        {
            break;
        }
    }
    uncertain(shared, attempt);
}

fn persistence(kind: &'static str, disposition: &'static str) {
    metrics::counter!(PERSISTENCE_METRIC, "kind" => kind, "disposition" => disposition)
        .increment(1);
}

/// The attempt's queue transition is not known to have landed; lease expiry
/// recovers the row.
fn uncertain(shared: &Shared, attempt: &AttemptId) {
    shared.counters.uncertain.fetch_add(1, Ordering::Relaxed);
    persistence(attempt.kind, "unknown");
    tracing::warn!(
        job.kind = attempt.kind,
        disposition = "unknown",
        "job_persistence_finished"
    );
}

/// Completions waiting for the next batch, and the one batch in flight.
///
/// The first waiter that holds `writer` writes every queued completion in one
/// statement; completions queued meanwhile go in the next batch. A waiter
/// whose completion another waiter wrote finds the queue empty and reads its
/// answer. A batch whose writer was dropped answers nobody, and each of its
/// waiters retries as after a failed statement.
#[derive(Default)]
pub(crate) struct Completions {
    queued: std::sync::Mutex<Vec<QueuedCompletion>>,
    writer: tokio::sync::Mutex<()>,
}

struct QueuedCompletion {
    id: JobId,
    generation: i64,
    deadline: Instant,
    // Fields drop in declaration order: custody retires before reply closure
    // can wake a waiter that immediately registers another completion.
    slots: Arc<AttemptSlots>,
    reply: tokio::sync::oneshot::Sender<Result<bool, ()>>,
}

impl QueuedCompletion {
    fn finish(self, result: Result<bool, ()>) {
        let Self { slots, reply, .. } = self;
        drop(slots);
        let _ = reply.send(result);
    }
}

/// Dropping a waiting supervisor removes only its own queued registration.
/// Once taken by the writer, the batch owns retirement instead.
struct CompletionRegistration<'a> {
    completions: &'a Completions,
    id: JobId,
    generation: i64,
}

impl Drop for CompletionRegistration<'_> {
    fn drop(&mut self) {
        let retired = {
            let mut queued = self.completions.lock_queue();
            queued
                .iter()
                .position(|entry| entry.id == self.id && entry.generation == self.generation)
                .map(|index| queued.swap_remove(index))
        };
        drop(retired);
    }
}

impl Completions {
    fn register(
        &self,
        attempt: &AttemptId,
        deadline: Instant,
        slots: &Arc<AttemptSlots>,
    ) -> (
        CompletionRegistration<'_>,
        tokio::sync::oneshot::Receiver<Result<bool, ()>>,
    ) {
        let (reply, response) = tokio::sync::oneshot::channel();
        let registration = CompletionRegistration {
            completions: self,
            id: attempt.id,
            generation: attempt.generation,
        };
        self.lock_queue().push(QueuedCompletion {
            id: attempt.id,
            generation: attempt.generation,
            deadline,
            slots: Arc::clone(slots),
            reply,
        });
        (registration, response)
    }

    fn lock_queue(&self) -> std::sync::MutexGuard<'_, Vec<QueuedCompletion>> {
        self.queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Remove batch membership before dropping each entry's custody and reply,
/// including when the SQL future is cancelled by its owning supervisor.
struct CompletionBatch(Vec<QueuedCompletion>);

impl Drop for CompletionBatch {
    fn drop(&mut self) {
        while let Some(entry) = self.0.pop() {
            drop(entry);
        }
    }
}

/// `Err(None)`: the batch failed and its writer already observed the error.
async fn complete_batched(
    shared: &Shared,
    attempt: &AttemptId,
    deadline: Instant,
    slots: &Arc<AttemptSlots>,
) -> Result<u64, Option<OperationError>> {
    let (_registration, response) = shared.completions.register(attempt, deadline, slots);
    let writer = shared.completions.writer.lock().await;
    let batch = CompletionBatch(std::mem::take(&mut *shared.completions.lock_queue()));
    write_batch(shared, batch).await;
    drop(writer);
    match response.await {
        Ok(Ok(applied)) => Ok(u64::from(applied)),
        Ok(Err(())) | Err(_) => Err(None),
    }
}

async fn write_batch(shared: &Shared, mut batch: CompletionBatch) {
    let mut index = 0;
    while index < batch.0.len() {
        if batch.0[index].reply.is_closed() || expired(shared, batch.0[index].deadline) {
            drop(batch.0.swap_remove(index));
        } else {
            index += 1;
        }
    }
    let Some(deadline) = batch.0.iter().map(|entry| entry.deadline).min() else {
        return;
    };
    let mut ids = Vec::with_capacity(batch.0.len());
    let mut generations = Vec::with_capacity(batch.0.len());
    for queued in &batch.0 {
        ids.push(queued.id.0);
        generations.push(queued.generation);
    }
    let result = within(shared, deadline, backstop(Box::pin(async {
        let mut connection = shared
            .pool
            .acquire()
            .await
            .map_err(OperationError::Acquire)?;
        if expired(shared, deadline) {
            return Err(OperationError::TimedOut);
        }
        // COMPLETE for every attempt queued while the previous batch was in flight.
        // Returns the 1-based position of each applied completion.
        observed(
            "complete jobs",
            sqlx::query_scalar!(
                "UPDATE background_jobs AS job \
                 SET state = 'completed', finished_at = statement_timestamp(), claim_expires_at = NULL \
                 FROM unnest($1::uuid[], $2::bigint[]) WITH ORDINALITY AS done (id, generation, position) \
                 WHERE job.id = done.id AND job.claim_generation = done.generation \
                   AND job.claim_expires_at IS NOT NULL \
                 RETURNING done.position AS \"position!\"",
                &ids,
                &generations,
            )
            .fetch_all(&mut *connection),
        )
        .await
        .map_err(OperationError::from)
    })))
    .await;
    match result {
        Some(Ok(positions)) => {
            let mut applied = vec![false; batch.0.len()];
            for position in positions {
                if let Some(flag) = usize::try_from(position - 1)
                    .ok()
                    .and_then(|index| applied.get_mut(index))
                {
                    *flag = true;
                }
            }
            while let Some(queued) = batch.0.pop() {
                queued.finish(Ok(applied[batch.0.len()]));
            }
        }
        Some(Err(error)) => {
            observe_failure(shared, Operation::Record, &error);
            while let Some(queued) = batch.0.pop() {
                queued.finish(Err(()));
            }
        }
        None => {}
    }
}

async fn send_outcome(
    shared: &Shared,
    attempt: &AttemptId,
    transition: &Transition,
) -> Result<u64, Option<OperationError>> {
    // An sqlx statement future is about 16 KiB; box it once so the supervisor stays small.
    backstop(Box::pin(async {
        let mut connection = shared
            .pool
            .acquire()
            .await
            .map_err(OperationError::Acquire)?;
        execute(&mut connection, attempt.id, attempt.generation, transition)
            .await
            .map_err(OperationError::from)
    }))
    .await
    .map_err(Some)
}

async fn execute(
    connection: &mut PgConnection,
    id: JobId,
    generation: i64,
    transition: &Transition,
) -> Result<u64, sqlx::Error> {
    let id = id.0;
    let (summary, query) = match transition {
        Transition::Complete => ("complete job", complete(id, generation)),
        Transition::Retry {
            summary,
            base_micros,
            floor_micros,
            ..
        } => (
            "retry job",
            sqlx::query!(
                "UPDATE background_jobs \
                 SET state = 'pending', not_before = statement_timestamp() + \
                     GREATEST($3::double precision * (0.9 + 0.2 * random()), $4::bigint) * interval '1 microsecond', \
                     claim_expires_at = NULL, error_summary = $5, \
                     errors = errors || jsonb_build_object('attempt', attempts, 'at', statement_timestamp(), 'error', $5::text) \
                 WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
                id,
                generation,
                base_micros,
                floor_micros,
                summary.as_str(),
            ),
        ),
        Transition::Snooze { delay_micros } => {
            let delay = sqlx::postgres::types::PgInterval {
                months: 0,
                days: 0,
                microseconds: *delay_micros,
            };
            (
                "snooze job",
                sqlx::query!(
                    "UPDATE background_jobs \
                     SET state = 'pending', not_before = statement_timestamp() + $3, claim_expires_at = NULL, \
                         attempts = attempts - 1 \
                     WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
                    id,
                    generation,
                    delay,
                ),
            )
        }
        // A cancelled attempt gives its unit back and keeps `not_before`, so the job
        // keeps its place in claim order instead of queueing behind the backlog.
        Transition::Release => (
            "release job",
            sqlx::query!(
                "UPDATE background_jobs \
                 SET state = 'pending', claim_expires_at = NULL, attempts = attempts - 1 \
                 WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
                id,
                generation,
            ),
        ),
        Transition::Fail { reason, summary } => (
            "fail job",
            sqlx::query!(
                "UPDATE background_jobs \
                 SET state = 'failed', failure_reason = $3, finished_at = statement_timestamp(), \
                     claim_expires_at = NULL, error_summary = $4, \
                     errors = errors || jsonb_build_object('attempt', attempts, 'at', statement_timestamp(), 'error', $4::text) \
                 WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
                id,
                generation,
                reason.label(),
                summary.as_str(),
            ),
        ),
    };
    Ok(observed(summary, query.execute(connection))
        .await?
        .rows_affected())
}

/// COMPLETE for one fenced claim.
///
/// Every outcome write fences on `claim_expires_at IS NOT NULL`, which the
/// table's CHECK makes equivalent to `state = 'running'`. A literal
/// `state = 'running'` proves the partial `background_jobs_running` predicate,
/// and after ANALYZE saw few running rows the planner scans that whole index
/// for the id, dead entries of every job claimed since the last VACUUM
/// included. This predicate leaves the primary key as the only access path.
pub(crate) fn complete(
    id: uuid::Uuid,
    generation: i64,
) -> sqlx::query::Query<'static, sqlx::Postgres, sqlx::postgres::PgArguments> {
    sqlx::query!(
        "UPDATE background_jobs \
         SET state = 'completed', finished_at = statement_timestamp(), claim_expires_at = NULL \
         WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
        id,
        generation,
    )
}

enum Ended {
    Success,
    Error(JobError),
    Panic,
    Payload(serde_json::Error),
    Timeout,
    Cancelled,
}

fn map_outcome(kind: &'static str, attempt: u16, policy: Policy, ended: Ended) -> Transition {
    match ended {
        Ended::Success => Transition::Complete,
        Ended::Cancelled => Transition::Release,
        Ended::Error(error) => match error.disposition {
            Disposition::Snooze(delay_micros) => Transition::Snooze { delay_micros },
            Disposition::Permanent => Transition::Fail {
                reason: Failure::Permanent,
                summary: summary(&error.summary),
            },
            Disposition::RetryAfterAtLeast(floor_micros) => {
                retry(attempt, policy, &error.summary, floor_micros, false)
            }
            Disposition::Retry => retry(attempt, policy, &error.summary, 0, false),
        },
        Ended::Panic => retry(attempt, policy, "handler panicked", 0, false),
        Ended::Payload(error) => retry(attempt, policy, &payload_summary(kind, &error), 0, false),
        Ended::Timeout => retry(
            attempt,
            policy,
            &format!(
                "attempt timed out after {}",
                humantime::format_duration(policy.timeout)
            ),
            0,
            true,
        ),
    }
}

fn retry(
    attempt: u16,
    policy: Policy,
    text: &str,
    floor_micros: i64,
    timed_out: bool,
) -> Transition {
    let summary = summary(text);
    if attempt >= policy.max_attempts {
        Transition::Fail {
            reason: Failure::Exhausted,
            summary,
        }
    } else {
        Transition::Retry {
            summary,
            base_micros: f64::from(attempt).powi(4) * 1_000_000.0,
            floor_micros,
            timed_out,
        }
    }
}

fn payload_summary(kind: &str, error: &serde_json::Error) -> String {
    let category = match error.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    };
    format!(
        "payload does not decode as {kind}: {category} error at line {} column {}",
        error.line(),
        error.column()
    )
}

fn summary(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len().min(ERROR_SUMMARY_MAX_BYTES));
    for ch in text.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if sanitized.len() + ch.len_utf8() > ERROR_SUMMARY_MAX_BYTES {
            break;
        }
        sanitized.push(ch);
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admitted(slots: &Arc<tokio::sync::Semaphore>) -> Arc<AttemptSlots> {
        Arc::new(AttemptSlots {
            _global: Arc::clone(slots).try_acquire_owned().unwrap(),
            _kind: None,
        })
    }

    fn attempt_id(generation: i64) -> AttemptId {
        AttemptId {
            id: JobId(uuid::Uuid::nil()),
            generation,
            kind: "sample",
            attempt: 1,
        }
    }

    #[test]
    fn cancelled_queued_registration_retires_only_its_claim_generation() {
        let completions = Completions::default();
        let capacity = Arc::new(tokio::sync::Semaphore::new(2));
        let first = admitted(&capacity);
        let second = admitted(&capacity);
        let deadline = Instant::now() + Duration::from_secs(10);
        let (first_registration, mut first_reply) =
            completions.register(&attempt_id(1), deadline, &first);
        let (second_registration, mut second_reply) =
            completions.register(&attempt_id(2), deadline, &second);
        drop(first);
        drop(second);
        assert_eq!(capacity.available_permits(), 0);

        drop(first_registration);
        assert_eq!(capacity.available_permits(), 1);
        assert_eq!(completions.lock_queue().len(), 1);
        assert!(matches!(
            first_reply.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        assert!(matches!(
            second_reply.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        drop(second_registration);
        assert_eq!(capacity.available_permits(), 2);
        assert!(completions.lock_queue().is_empty());
    }

    struct ReplyWake {
        capacity: Arc<tokio::sync::Semaphore>,
        available_at_wake: std::sync::atomic::AtomicUsize,
    }

    impl std::task::Wake for ReplyWake {
        fn wake(self: Arc<Self>) {
            self.available_at_wake
                .store(self.capacity.available_permits(), Ordering::SeqCst);
        }
    }

    #[test]
    fn in_flight_custody_retires_before_success_failure_or_cancellation_wakes_a_retry() {
        for result in [Some(Ok(true)), Some(Err(())), None] {
            let completions = Completions::default();
            let capacity = Arc::new(tokio::sync::Semaphore::new(1));
            let slots = admitted(&capacity);
            let (registration, mut response) = completions.register(
                &attempt_id(1),
                Instant::now() + Duration::from_secs(10),
                &slots,
            );
            let mut batch = CompletionBatch(std::mem::take(&mut *completions.lock_queue()));
            drop(registration);
            drop(slots);
            assert_eq!(
                capacity.available_permits(),
                0,
                "the batch retains admission"
            );

            let wake = Arc::new(ReplyWake {
                capacity: Arc::clone(&capacity),
                available_at_wake: std::sync::atomic::AtomicUsize::new(usize::MAX),
            });
            let waker = std::task::Waker::from(Arc::clone(&wake));
            let mut context = std::task::Context::from_waker(&waker);
            assert!(
                std::pin::Pin::new(&mut response)
                    .poll(&mut context)
                    .is_pending()
            );
            match result {
                Some(result) => batch.0.pop().unwrap().finish(result),
                None => drop(batch),
            }
            assert_eq!(wake.available_at_wake.load(Ordering::SeqCst), 1);
            assert_eq!(capacity.available_permits(), 1);
            match result {
                Some(result) => assert_eq!(response.try_recv().unwrap(), result),
                None => assert!(matches!(
                    response.try_recv(),
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed)
                )),
            }
        }
    }

    fn outcome(ended: Ended, attempt: u16) -> Transition {
        map_outcome("sample", attempt, Policy::default(), ended)
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "attempt^4 * 1e6 is exact in f64")]
    fn dispositions_preserve_cap_and_snooze() {
        assert!(matches!(outcome(Ended::Success, 25), Transition::Complete));
        assert!(matches!(
            outcome(Ended::Error(JobError::retryable("fail")), 25),
            Transition::Fail {
                reason: Failure::Exhausted,
                ..
            }
        ));
        assert!(matches!(
            outcome(
                Ended::Error(
                    JobError::retry_after_at_least("at least", Duration::from_secs(2)).unwrap()
                ),
                1,
            ),
            Transition::Retry {
                base_micros,
                floor_micros: 2_000_000,
                timed_out: false,
                ..
            } if base_micros == 1_000_000.0
        ));
        assert!(matches!(
            outcome(
                Ended::Error(
                    JobError::retry_after_at_least("at least", Duration::from_secs(2)).unwrap()
                ),
                3,
            ),
            Transition::Retry {
                base_micros,
                floor_micros: 2_000_000,
                timed_out: false,
                ..
            } if base_micros == 81_000_000.0
        ));
        assert!(matches!(
            outcome(
                Ended::Error(JobError::snooze(Duration::from_micros(7)).unwrap()),
                25,
            ),
            Transition::Snooze { delay_micros: 7 }
        ));
        assert!(matches!(
            outcome(Ended::Panic, 1),
            Transition::Retry {
                ref summary,
                timed_out: false,
                floor_micros: 0,
                ..
            } if summary == "handler panicked"
        ));
        assert!(matches!(
            outcome(Ended::Timeout, 1),
            Transition::Retry {
                timed_out: true,
                ..
            }
        ));
        assert!(matches!(
            outcome(Ended::Error(JobError::permanent("stop")), 1),
            Transition::Fail {
                reason: Failure::Permanent,
                ..
            }
        ));
    }

    /// Collects the fields of every span, from creation and later records.
    #[derive(Clone, Default)]
    struct SpanFields(Arc<std::sync::Mutex<Vec<std::collections::BTreeMap<String, String>>>>);

    struct Collect<'a>(&'a mut std::collections::BTreeMap<String, String>);

    impl tracing::field::Visit for Collect<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().to_owned(), format!("{value:?}"));
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.insert(field.name().to_owned(), value.to_owned());
        }
    }

    impl tracing::Subscriber for SpanFields {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            let mut spans = self.0.lock().unwrap();
            let mut fields = std::collections::BTreeMap::new();
            attributes.record(&mut Collect(&mut fields));
            spans.push(fields);
            tracing::span::Id::from_u64(spans.len() as u64)
        }

        fn record(&self, span: &tracing::span::Id, values: &tracing::span::Record<'_>) {
            let index = usize::try_from(span.into_u64()).unwrap() - 1;
            values.record(&mut Collect(&mut self.0.lock().unwrap()[index]));
        }

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, _: &tracing::Event<'_>) {}

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn the_attempt_span_is_named_for_its_kind_and_carries_the_outcome() {
        let id = JobId(uuid::Uuid::try_parse("01234567-89ab-cdef-fedc-ba9876543210").unwrap());
        let spans = SpanFields::default();
        tracing::subscriber::with_default(spans.clone(), || {
            for ended in [
                Ended::Success,
                Ended::Error(JobError::retryable("fail")),
                Ended::Error(JobError::permanent("stop")),
                Ended::Cancelled,
            ] {
                let span = attempt_span(id, "sample", 3);
                record_on_span(&span, &outcome(ended, 3));
            }
        });
        let spans = spans.0.lock().unwrap();
        let seen: Vec<_> = spans
            .iter()
            .map(|fields| {
                (
                    fields["outcome"].as_str(),
                    fields.get("otel.status_code").map(String::as_str),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("completed", None),
                ("retry", Some("ERROR")),
                ("permanent", Some("ERROR")),
                ("cancelled", None),
            ]
        );
        let first = &spans[0];
        assert_eq!(first["otel.name"], "process sample");
        assert_eq!(first["otel.kind"], "consumer");
        assert_eq!(first["job.kind"], "sample");
        assert_eq!(first["job.id"], "01234567-89ab-cdef-fedc-ba9876543210");
        assert_eq!(first["job.attempt"], "3");
    }

    #[test]
    fn summaries_sanitize_controls_and_preserve_utf8_without_payload_disclosure() {
        assert_eq!(summary("a\nb\0c"), "a b c");
        assert_eq!(summary(""), "");
        assert_eq!(summary(&"x".repeat(1024)), "x".repeat(1024));
        assert_eq!(summary(&"x".repeat(65_536)), "x".repeat(1024));
        assert_eq!(summary(&("\u{0085}".repeat(1023) + "é")), " ".repeat(1023));
        assert_eq!(summary(&("é".repeat(512) + "tail")), "é".repeat(512));
        assert_eq!(summary(&("a".repeat(1023) + "é")), "a".repeat(1023));
        let error = serde_json::from_str::<u32>("\"secret\"").unwrap_err();
        assert_eq!(
            payload_summary("sample", &error),
            "payload does not decode as sample: data error at line 1 column 8"
        );
    }
}
