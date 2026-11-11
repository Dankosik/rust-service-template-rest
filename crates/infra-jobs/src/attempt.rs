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
use crate::kind::{
    Attempt, Disposition, HandlerFuture, JobError, JobId, MAX_ERROR_SUMMARY_BYTES, Policy,
    Registered,
};
use crate::state::FailureReason;

/// How long a cancelled or timed-out handler may keep running before [`drive`]
/// stops waiting for it to join its own work.
///
/// This is a tuning value, not a protocol contract: a handler that needs longer
/// than this window is abandoned with its future dropped, so nothing observable
/// depends on the exact number. [`drive`] caps the resulting wait at the
/// supervisor deadline, so grace never holds a claim past it.
const COOPERATIVE_GRACE: Duration = Duration::from_millis(100);

/// Attempt admission retained by supervisors or completion bookkeeping. No labels.
const OWNED_ATTEMPTS_METRIC: &str = "jobs_owned_attempts";
/// Queued plus in-flight completion entries. No labels.
const COMPLETION_MEMBERSHIPS_METRIC: &str = "jobs_completion_memberships";

/// Observed handler results and claim-time exhaustions, a terminal failure
/// once. Labels `kind`, `outcome`.
pub(crate) const ATTEMPTS_METRIC: &str = "jobs_attempts_total";
/// Queue-write acknowledgement, not attribution. Labels `kind`, `disposition`.
pub(crate) const PERSISTENCE_METRIC: &str = "jobs_persistence_total";
/// Handler run time. Label `kind`.
pub const ATTEMPT_DURATION_METRIC: &str = "jobs_attempt_duration_seconds";
pub const ATTEMPT_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0,
    1800.0, 3600.0,
];

enum Transition {
    Complete,
    Retry {
        summary: String,
        base_micros: f64,
        floor_micros: i64,
        timed_out: bool,
    },
    Fail {
        reason: FailureReason,
        summary: String,
    },
    Snooze {
        delay_micros: i64,
        summary: String,
    },
    Release,
}

impl Transition {
    const fn label(&self) -> &'static str {
        match self {
            Self::Complete => "completed",
            Self::Retry {
                timed_out: true, ..
            } => "timeout",
            Self::Retry { .. } => "retry",
            Self::Fail { reason, .. } => reason.as_str(),
            Self::Snooze { .. } => "snoozed",
            Self::Release => "cancelled",
        }
    }
}

/// An outcome that stores its own row: the write-only view of a [`Transition`].
///
/// It carries only the values its statement binds. Labels stay with
/// [`Transition`], so a timed-out retry is a [`Self::Retry`] here.
#[derive(Clone, Copy)]
enum Write<'a> {
    Retry {
        summary: &'a str,
        base_micros: f64,
        floor_micros: i64,
    },
    Snooze {
        delay_micros: i64,
    },
    Fail {
        reason: FailureReason,
        summary: &'a str,
    },
    Release,
}

/// An acknowledged outcome write: whether it changed the job row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Written {
    Applied,
    /// The fenced claim was no longer running, so the write matched no row.
    Unchanged,
}

impl Written {
    const fn from_rows(rows: u64) -> Self {
        if rows == 1 {
            Self::Applied
        } else {
            Self::Unchanged
        }
    }

    /// The acknowledgement's `jobs_persistence_total` disposition label.
    const fn label(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Unchanged => "unchanged",
        }
    }
}

/// An outcome write that did not acknowledge and needs no further
/// observation: its writer already counted and logged the failure, or its
/// batch ended without an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Observed;

pub(crate) fn describe_metrics() {
    metrics::describe_gauge!(OWNED_ATTEMPTS_METRIC, "Attempt slots currently owned");
    metrics::describe_gauge!(
        COMPLETION_MEMBERSHIPS_METRIC,
        "Queued and in-flight completion entries currently owned"
    );
    // Register idle observations without resetting other engines' live gauge values.
    metrics::gauge!(OWNED_ATTEMPTS_METRIC).increment(0.0);
    metrics::gauge!(COMPLETION_MEMBERSHIPS_METRIC).increment(0.0);
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
            completed: metrics::counter!(
                ATTEMPTS_METRIC,
                "kind" => kind,
                "outcome" => Transition::Complete.label()
            ),
            duration: metrics::histogram!(ATTEMPT_DURATION_METRIC, "kind" => kind),
            applied: metrics::counter!(
                PERSISTENCE_METRIC,
                "kind" => kind,
                "disposition" => Written::Applied.label()
            ),
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
    // Retire the metric membership before releasing capacity that can admit new work.
    _membership: MetricMembership,
    _global: tokio::sync::OwnedSemaphorePermit,
    _kind: Option<crate::claim::KindSlot>,
}

/// One unit of the gauge metric named at construction, held until this drops.
///
/// The metric counts a kind of member the engine owns — an admitted attempt or a
/// queued completion — so the guard names its membership and not claim custody.
struct MetricMembership(metrics::Gauge);

impl MetricMembership {
    fn new(name: &'static str) -> Self {
        let gauge = metrics::gauge!(name);
        gauge.increment(1.0);
        Self(gauge)
    }
}

impl Drop for MetricMembership {
    fn drop(&mut self) {
        self.0.decrement(1.0);
    }
}

/// Which job and attempt an outcome belongs to. The claim generation that
/// fences outcome writes travels beside it, only to the writes.
#[derive(Debug)]
pub(crate) struct AttemptId {
    pub(crate) id: JobId,
    pub(crate) kind: &'static str,
    /// Attempts used, including this one.
    pub(crate) number: u16,
}

/// Claim-time exhaustion is observed only after its transaction is acknowledged.
pub(crate) fn record_exhausted(attempt: &AttemptId, summary: Option<&str>) {
    observe_attempt(
        None,
        attempt,
        &Transition::Fail {
            reason: FailureReason::Exhausted,
            summary: summary.unwrap_or("").to_owned(),
        },
    );
}

fn record_duration(handles: Option<&KindMetrics>, attempt: &AttemptId, ran: Duration) {
    match handles {
        Some(handles) => handles.duration.record(ran.as_secs_f64()),
        None => {
            metrics::histogram!(ATTEMPT_DURATION_METRIC, "kind" => attempt.kind)
                .record(ran.as_secs_f64());
        }
    }
}

fn observe_attempt(handles: Option<&KindMetrics>, attempt: &AttemptId, transition: &Transition) {
    match handles {
        Some(handles) if matches!(transition, Transition::Complete) => {
            handles.completed.increment(1);
        }
        _ => metrics::counter!(ATTEMPTS_METRIC, "kind" => attempt.kind, "outcome" => transition.label())
            .increment(1),
    }
    match transition {
        Transition::Complete => {
            tracing::debug!(job.id = %attempt.id, job.kind = attempt.kind, job.attempt = attempt.number, "job_attempt_completed");
        }
        Transition::Snooze { summary, .. } => {
            tracing::info!(job.id = %attempt.id, job.kind = attempt.kind, outcome = transition.label(), error = summary.as_str(), "job_attempt_finished");
        }
        Transition::Release => {
            tracing::info!(job.id = %attempt.id, job.kind = attempt.kind, outcome = transition.label(), "job_attempt_finished");
        }
        Transition::Retry { summary, .. } => {
            tracing::info!(job.id = %attempt.id, job.kind = attempt.kind, job.attempt = attempt.number, error = summary.as_str(), "job_attempt_failed");
        }
        Transition::Fail { summary, .. } => {
            tracing::warn!(job.id = %attempt.id, job.kind = attempt.kind, job.attempt = attempt.number, job.failure_reason = transition.label(), error = summary.as_str(), "job_failed");
        }
    }
}

/// The supervisor of one claimed attempt.
///
/// It retires `guard` once the attempt's outcome is written or known to be
/// uncertain. A claimed kind this engine does not register leaves the guard
/// armed, so the engine fails instead of leaving the claim to its lease.
pub(crate) fn supervise(
    shared: Arc<Shared>,
    claimed: crate::claim::Claimed,
    supervisor_deadline: Instant,
    mut guard: crate::engine::SupervisorGuard,
) -> impl Future<Output = ()> + Send + 'static {
    let crate::claim::Claimed {
        attempt,
        generation,
        payload,
        trace_parent,
        trace_state,
        slot,
        kind_slot,
    } = claimed;
    let span = attempt_span(attempt.id, attempt.kind, attempt.number);
    crate::trace_context::link(&span, trace_parent.as_deref(), trace_state.as_deref());
    drop(trace_parent);
    drop(trace_state);
    let slots = Arc::new(AttemptSlots {
        _membership: MetricMembership::new(OWNED_ATTEMPTS_METRIC),
        _global: slot,
        _kind: kind_slot,
    });
    async move {
        let Some(registered) = shared.registry.get(attempt.kind) else {
            return;
        };
        run_attempt(
            &shared,
            registered,
            attempt,
            generation,
            payload,
            supervisor_deadline,
            slots,
        )
        .instrument(span)
        .await;
        guard.retire();
    }
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
    registered: &Registered,
    attempt: AttemptId,
    generation: i64,
    payload: Vec<u8>,
    supervisor_deadline: Instant,
    slots: Arc<AttemptSlots>,
) {
    if expired(shared, supervisor_deadline) {
        uncertain(shared, &attempt);
        return;
    }
    let policy = registered.policy;
    let attempt_deadline = clamped_deadline(Instant::now(), policy.timeout, supervisor_deadline);
    let cancel = CancellationToken::new();
    let prepared = prepare_handler(
        registered.dispatch.as_ref(),
        Attempt {
            id: attempt.id,
            number: attempt.number,
            generation,
            attempt_deadline,
            cancellation: cancel.clone(),
            pool: shared.pool.clone(),
        },
        &payload,
    );
    drop(payload);
    let (ended, ran) = match prepared {
        Err(ended) => (ended, None),
        Ok(future) => {
            let started = Instant::now();
            let Some(ended) = drive(
                shared,
                future,
                cancel,
                attempt_deadline,
                supervisor_deadline,
            )
            .await
            else {
                uncertain(shared, &attempt);
                return;
            };
            (ended, Some(started.elapsed()))
        }
    };
    let transition = map_outcome(attempt.kind, attempt.number, policy, ended);
    if matches!(transition, Transition::Release) {
        shared.counters.cancelled.fetch_add(1, Ordering::Relaxed);
    } else {
        shared
            .counters
            .known_results
            .fetch_add(1, Ordering::Relaxed);
    }
    record_on_span(&tracing::Span::current(), &transition);
    let handles = registered.metrics.get();
    if let Some(ran) = ran {
        record_duration(handles, &attempt, ran);
    }
    // A terminal failure counts only once its write applies. A row the write
    // did not move is failed, re-run, or exhausted by its lease rescue, which
    // counts it; counting it here too would count one job twice.
    let terminal = matches!(transition, Transition::Fail { .. });
    if !terminal {
        observe_attempt(handles, &attempt, &transition);
    }
    let written = persist(
        shared,
        &attempt,
        generation,
        &transition,
        supervisor_deadline,
        &slots,
    )
    .await;
    if terminal {
        if written == Some(Written::Applied) {
            observe_attempt(handles, &attempt, &transition);
        } else if let Transition::Fail { summary, .. } = &transition {
            tracing::info!(job.id = %attempt.id, job.kind = attempt.kind, job.attempt = attempt.number, outcome = transition.label(), error = summary.as_str(), "job_attempt_failed");
        }
    }
}

fn prepare_handler(
    dispatch: &dyn crate::kind::Dispatch,
    attempt: Attempt,
    payload: &[u8],
) -> Result<HandlerFuture, Ended> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dispatch.prepare(attempt, payload)
    }))
    .map_err(|_| Ended::Panic)?
    .map_err(Ended::Payload)
}

/// `future`'s output, or `None` once `until` passes or cleanup ends.
async fn within<T>(shared: &Shared, until: Instant, future: impl Future<Output = T>) -> Option<T> {
    tokio::select! {
        biased;
        value = future => Some(value),
        () = shared.cleanup_ended() => None,
        () = tokio::time::sleep_until(until) => None,
    }
}

fn expired(shared: &Shared, until: Instant) -> bool {
    Instant::now() >= until || shared.cleanup_has_ended()
}

/// `base + wait`, capped at `limit`: the latest instant a local wait may reach.
///
/// A sum that would overflow `Instant` takes `limit`, which every caller already
/// treats as the outermost bound on its own work.
fn clamped_deadline(base: Instant, wait: Duration, limit: Instant) -> Instant {
    base.checked_add(wait).unwrap_or(limit).min(limit)
}

/// The future is already heap-pinned. Take it once so neither normal retirement
/// nor cancellation can bypass the same synchronous destruction boundary.
struct OwnedHandler(Option<HandlerFuture>);

impl OwnedHandler {
    fn destroy(&mut self) {
        if let Some(future) = self.0.take()
            && let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                drop(future);
            }))
        {
            tracing::warn!("job_handler_drop_panicked");
            // A secondary unwind here belongs to the supervisor failure guard.
            drop(payload);
        }
    }
}

impl Drop for OwnedHandler {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Poll a ready result before cancellation; it wins as is. After cancellation
/// only success wins (see [`ended_after_cancel`]).
async fn drive(
    shared: &Shared,
    future: HandlerFuture,
    cancel: CancellationToken,
    attempt_deadline: Instant,
    supervisor_deadline: Instant,
) -> Option<Ended> {
    let mut handler = OwnedHandler(Some(future));
    let reason = {
        // Borrow through the poll boundary; the owner contains destruction as well.
        let mut run =
            std::pin::pin!(std::panic::AssertUnwindSafe(handler.0.as_mut()?).catch_unwind());
        let reason = tokio::select! {
            biased;
            result = &mut run => return Some(ended_from(result)),
            () = shared.force.cancelled() => Ended::Cancelled,
            () = tokio::time::sleep_until(attempt_deadline) => Ended::Timeout,
        };
        cancel.cancel();
        let grace = clamped_deadline(Instant::now(), COOPERATIVE_GRACE, supervisor_deadline);
        if let Some(result) = within(shared, grace, &mut run).await {
            return Some(ended_after_cancel(&result, reason));
        }
        reason
    };
    handler.destroy();
    (!expired(shared, supervisor_deadline)).then_some(reason)
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

/// The write-only view of `transition`: the outcome that stores its own row.
///
/// A completion returns `None`, because [`complete_batched`] is the only path
/// that completes an attempt. The match is exhaustive, so a new transition
/// cannot reach a statement without first choosing which of the two writes it
/// takes, and the view carries only the values its statement binds.
fn write_of(transition: &Transition) -> Option<Write<'_>> {
    Some(match transition {
        Transition::Complete => return None,
        Transition::Retry {
            summary,
            base_micros,
            floor_micros,
            ..
        } => Write::Retry {
            summary,
            base_micros: *base_micros,
            floor_micros: *floor_micros,
        },
        Transition::Snooze { delay_micros, .. } => Write::Snooze {
            delay_micros: *delay_micros,
        },
        Transition::Fail { reason, summary } => Write::Fail {
            reason: *reason,
            summary,
        },
        Transition::Release => Write::Release,
    })
}

async fn persist(
    shared: &Shared,
    attempt: &AttemptId,
    generation: i64,
    transition: &Transition,
    supervisor_deadline: Instant,
    slots: &Arc<AttemptSlots>,
) -> Option<Written> {
    let operation = if matches!(transition, Transition::Release) {
        Operation::Release
    } else {
        Operation::Record
    };
    let write = write_of(transition);
    loop {
        if expired(shared, supervisor_deadline) {
            break;
        }
        let sent = match write {
            None => {
                within(
                    shared,
                    supervisor_deadline,
                    complete_batched(shared, attempt.id, generation, supervisor_deadline, slots),
                )
                .await
            }
            Some(write) => {
                within(
                    shared,
                    supervisor_deadline,
                    send_outcome(shared, attempt.id, generation, write, operation),
                )
                .await
            }
        };
        match sent {
            None => break,
            Some(Ok(written)) => {
                acknowledged(shared, attempt, transition, operation, written);
                return Some(written);
            }
            Some(Err(Observed)) => {}
        }
        let until = clamped_deadline(Instant::now(), RECORD_RETRY_INTERVAL, supervisor_deadline);
        if within(shared, supervisor_deadline, tokio::time::sleep_until(until))
            .await
            .is_none()
        {
            break;
        }
    }
    uncertain(shared, attempt);
    None
}

/// The attempt's queue transition was acknowledged.
fn acknowledged(
    shared: &Shared,
    attempt: &AttemptId,
    transition: &Transition,
    operation: Operation,
    written: Written,
) {
    observe_recovery(shared, operation);
    let disposition = written.label();
    match kind_metrics(shared, attempt.kind) {
        Some(handles) if written == Written::Applied => handles.applied.increment(1),
        _ => count_persistence(attempt.kind, disposition),
    }
    tracing::debug!(
        job.kind = attempt.kind,
        disposition,
        "job_persistence_finished"
    );
    if written == Written::Applied && matches!(transition, Transition::Release) {
        shared.counters.released.fetch_add(1, Ordering::Relaxed);
    }
}

fn count_persistence(kind: &'static str, disposition: &'static str) {
    metrics::counter!(PERSISTENCE_METRIC, "kind" => kind, "disposition" => disposition)
        .increment(1);
}

/// The attempt's queue transition is not known to have landed; lease expiry
/// recovers the row.
fn uncertain(shared: &Shared, attempt: &AttemptId) {
    shared.counters.uncertain.fetch_add(1, Ordering::Relaxed);
    // Neither [`Written`] acknowledgement exists, so this disposition has no
    // [`Written::label`] value of its own.
    let disposition = "unknown";
    count_persistence(attempt.kind, disposition);
    tracing::warn!(
        job.kind = attempt.kind,
        disposition,
        "job_persistence_finished"
    );
}

type CompletionReply = Result<Written, Observed>;

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
    // Fields drop in declaration order: the membership retires before reply
    // closure can wake a waiter that immediately registers another completion.
    membership: MetricMembership,
    slots: Arc<AttemptSlots>,
    reply: tokio::sync::oneshot::Sender<CompletionReply>,
}

impl QueuedCompletion {
    fn retire(self) -> tokio::sync::oneshot::Sender<CompletionReply> {
        let Self {
            membership,
            slots,
            reply,
            ..
        } = self;
        drop(membership);
        drop(slots);
        reply
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
        id: JobId,
        generation: i64,
        deadline: Instant,
        slots: &Arc<AttemptSlots>,
    ) -> (
        CompletionRegistration<'_>,
        tokio::sync::oneshot::Receiver<CompletionReply>,
    ) {
        let (reply, response) = tokio::sync::oneshot::channel();
        let registration = CompletionRegistration {
            completions: self,
            id,
            generation,
        };
        self.lock_queue().push(QueuedCompletion {
            id,
            generation,
            deadline,
            membership: MetricMembership::new(COMPLETION_MEMBERSHIPS_METRIC),
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

/// Retire the entire batch before a reply or sender closure can wake a waiter.
struct CompletionBatch {
    queued: Vec<QueuedCompletion>,
    pruned_replies: Vec<tokio::sync::oneshot::Sender<CompletionReply>>,
}

impl CompletionBatch {
    fn new(queued: Vec<QueuedCompletion>) -> Self {
        Self {
            queued,
            pruned_replies: Vec::new(),
        }
    }

    /// Retire the entries `stale` selects, keeping the order of the rest. Their
    /// senders stay open until the batch finishes or drops.
    fn prune(&mut self, stale: impl FnMut(&mut QueuedCompletion) -> bool) {
        self.pruned_replies.extend(
            self.queued
                .extract_if(.., stale)
                .map(QueuedCompletion::retire),
        );
    }

    fn retire(&mut self) -> Vec<tokio::sync::oneshot::Sender<CompletionReply>> {
        std::mem::take(&mut self.queued)
            .into_iter()
            .map(QueuedCompletion::retire)
            .collect()
    }

    fn finish(mut self, results: Vec<CompletionReply>) {
        let replies: Vec<_> = self.retire().into_iter().zip(results).collect();
        // Also close pruned replies only after every entry and SQL buffer is gone.
        drop(self);
        for (reply, result) in replies {
            let _ = reply.send(result);
        }
    }
}

impl Drop for CompletionBatch {
    fn drop(&mut self) {
        drop(self.retire());
    }
}

async fn complete_batched(
    shared: &Shared,
    id: JobId,
    generation: i64,
    supervisor_deadline: Instant,
    slots: &Arc<AttemptSlots>,
) -> CompletionReply {
    let (_registration, response) =
        shared
            .completions
            .register(id, generation, supervisor_deadline, slots);
    let writer = shared.completions.writer.lock().await;
    let batch = CompletionBatch::new(std::mem::take(&mut *shared.completions.lock_queue()));
    write_batch(shared, batch).await;
    drop(writer);
    response.await.unwrap_or(Err(Observed))
}

async fn write_batch(shared: &Shared, mut batch: CompletionBatch) {
    batch.prune(|entry| entry.reply.is_closed() || expired(shared, entry.deadline));
    let Some(deadline) = batch.queued.iter().map(|entry| entry.deadline).min() else {
        return;
    };
    // The batch's bind arrays live in this scope so the statement future ends,
    // and its borrow of them with it, before any reply wakes a waiter.
    let result = {
        let (ids, generations): (Vec<_>, Vec<_>) = batch
            .queued
            .iter()
            .map(|queued| (queued.id.0, queued.generation))
            .unzip();
        within(
            shared,
            deadline,
            backstop(Box::pin(async {
                let mut connection = infra_postgres::acquire(&shared.pool, "complete jobs batch")
                    .await
                    .map_err(OperationError::Acquire)?;
                if expired(shared, deadline) {
                    return Err(OperationError::TimedOut);
                }
                // COMPLETE for every attempt queued while the previous batch was in
                // flight, under the fence `complete` explains. Returns the 1-based
                // position of each applied completion.
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
            })),
        )
        .await
    };
    match result {
        Some(Ok(positions)) => {
            let results = written_by_position(&positions, batch.queued.len())
                .into_iter()
                .map(Ok)
                .collect();
            batch.finish(results);
        }
        Some(Err(error)) => {
            observe_failure(shared, Operation::Record, &error);
            drop(error);
            let results = vec![Err(Observed); batch.queued.len()];
            batch.finish(results);
        }
        None => {}
    }
}

/// Each of `len` batched completions' writes, from the 1-based positions the
/// statement applied.
fn written_by_position(positions: &[i64], len: usize) -> Vec<Written> {
    let mut written = vec![Written::Unchanged; len];
    for &position in positions {
        if let Some(entry) = usize::try_from(position - 1)
            .ok()
            .and_then(|index| written.get_mut(index))
        {
            *entry = Written::Applied;
        }
    }
    written
}

async fn send_outcome(
    shared: &Shared,
    id: JobId,
    generation: i64,
    write: Write<'_>,
    operation: Operation,
) -> Result<Written, Observed> {
    // An sqlx statement future is about 16 KiB; box it once so the supervisor stays small.
    let result = backstop(Box::pin(async {
        let mut connection = infra_postgres::acquire(&shared.pool, "record job outcome")
            .await
            .map_err(OperationError::Acquire)?;
        execute(&mut connection, id, generation, write)
            .await
            .map_err(OperationError::from)
    }))
    .await;
    result.map(Written::from_rows).map_err(|error| {
        observe_failure(shared, operation, &error);
        Observed
    })
}

/// Store one attempt outcome. A completion is not among the views: its only
/// write is [`complete_batched`].
async fn execute(
    connection: &mut PgConnection,
    id: JobId,
    generation: i64,
    write: Write<'_>,
) -> Result<u64, sqlx::Error> {
    let id = id.0;
    let (summary, query) = match write {
        Write::Retry {
            summary,
            base_micros,
            floor_micros,
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
                summary,
            ),
        ),
        Write::Snooze { delay_micros } => {
            let delay = sqlx::postgres::types::PgInterval {
                months: 0,
                days: 0,
                microseconds: delay_micros,
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
        Write::Release => (
            "release job",
            sqlx::query!(
                "UPDATE background_jobs \
                 SET state = 'pending', claim_expires_at = NULL, attempts = attempts - 1 \
                 WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
                id,
                generation,
            ),
        ),
        Write::Fail { reason, summary } => (
            "fail job",
            sqlx::query!(
                "UPDATE background_jobs \
                 SET state = 'failed', failure_reason = $3, finished_at = statement_timestamp(), \
                     claim_expires_at = NULL, error_summary = $4, \
                     errors = errors || jsonb_build_object('attempt', attempts, 'at', statement_timestamp(), 'error', $4::text) \
                 WHERE id = $1 AND claim_generation = $2 AND claim_expires_at IS NOT NULL",
                id,
                generation,
                reason.as_str(),
                summary,
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
            Disposition::Snooze { delay_micros } => Transition::Snooze {
                delay_micros,
                summary: sanitize_summary(&error.summary),
            },
            Disposition::Permanent => Transition::Fail {
                reason: FailureReason::Permanent,
                summary: sanitize_summary(&error.summary),
            },
            Disposition::RetryAfterAtLeast { floor_micros } => retry_or_exhaust(
                attempt,
                policy,
                &error.summary,
                RetryCause::Failure { floor_micros },
            ),
            Disposition::Retry => {
                retry_or_exhaust(attempt, policy, &error.summary, RetryCause::FAILURE)
            }
        },
        Ended::Panic => retry_or_exhaust(attempt, policy, "handler panicked", RetryCause::FAILURE),
        Ended::Payload(error) => retry_or_exhaust(
            attempt,
            policy,
            &payload_summary(kind, &error),
            RetryCause::FAILURE,
        ),
        Ended::Timeout => retry_or_exhaust(
            attempt,
            policy,
            &format!(
                "attempt timed out after {}",
                humantime::format_duration(policy.timeout)
            ),
            RetryCause::Timeout,
        ),
    }
}

/// Why an attempt spent its unit without completing.
#[derive(Clone, Copy)]
enum RetryCause {
    /// The handler failed; the next attempt waits at least `floor_micros`.
    Failure { floor_micros: i64 },
    /// The handler ran past its timeout.
    Timeout,
}

impl RetryCause {
    const FAILURE: Self = Self::Failure { floor_micros: 0 };
}

/// Retry with backoff, or fail as exhausted once the attempt budget is spent.
///
/// The nominal delay after attempt `n` is `n^4` seconds: 1 s, 16 s, 81 s, and
/// so on. The retry statement in [`execute`] jitters it by ±10% and raises it
/// to the cause's floor.
fn retry_or_exhaust(attempt: u16, policy: Policy, text: &str, cause: RetryCause) -> Transition {
    let summary = sanitize_summary(text);
    if attempt >= policy.max_attempts {
        return Transition::Fail {
            reason: FailureReason::Exhausted,
            summary,
        };
    }
    let (floor_micros, timed_out) = match cause {
        RetryCause::Failure { floor_micros } => (floor_micros, false),
        RetryCause::Timeout => (0, true),
    };
    Transition::Retry {
        summary,
        base_micros: f64::from(attempt).powi(4) * 1_000_000.0,
        floor_micros,
        timed_out,
    }
}

/// The stored summary of a payload that does not decode as `kind`.
///
/// The rejected payload is never quoted: it holds untrusted producer or handler
/// input, and a value copied here would be stored, logged, and exported. The
/// category, line, and column locate the defect without it.
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

/// The final stored form of a summary: control characters become spaces, and
/// the UTF-8 length stops at [`MAX_ERROR_SUMMARY_BYTES`] without splitting a
/// character.
///
/// This is the one owner of the stored bound. Every summary this module writes
/// passes here, and [`crate::kind`] appends its sources only while they stay
/// below the same limit.
fn sanitize_summary(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len().min(MAX_ERROR_SUMMARY_BYTES));
    for ch in text.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if sanitized.len() + ch.len_utf8() > MAX_ERROR_SUMMARY_BYTES {
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
            _membership: MetricMembership::new(OWNED_ATTEMPTS_METRIC),
            _global: Arc::clone(slots).try_acquire_owned().unwrap(),
            _kind: None,
        })
    }

    const ID: JobId = JobId(uuid::Uuid::nil());

    #[test]
    fn cancelled_queued_registration_retires_only_its_claim_generation() {
        let gauges = crate::engine::tests::Gauges::default();
        metrics::with_local_recorder(&gauges, || {
            let completions = Completions::default();
            let capacity = Arc::new(tokio::sync::Semaphore::new(2));
            let first = admitted(&capacity);
            let second = admitted(&capacity);
            let deadline = Instant::now() + Duration::from_secs(10);
            let (first_registration, mut first_reply) =
                completions.register(ID, 1, deadline, &first);
            let (second_registration, mut second_reply) =
                completions.register(ID, 2, deadline, &second);
            drop(first);
            drop(second);
            assert_eq!(capacity.available_permits(), 0);
            assert_eq!(gauges.get(OWNED_ATTEMPTS_METRIC, None), 2.0);
            assert_eq!(gauges.get(COMPLETION_MEMBERSHIPS_METRIC, None), 2.0);
            describe_metrics();
            assert_eq!(
                gauges.get(OWNED_ATTEMPTS_METRIC, None),
                2.0,
                "another engine startup must not reset live memberships"
            );

            drop(first_registration);
            assert_eq!(capacity.available_permits(), 1);
            assert_eq!(gauges.get(OWNED_ATTEMPTS_METRIC, None), 1.0);
            assert_eq!(gauges.get(COMPLETION_MEMBERSHIPS_METRIC, None), 1.0);
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
            assert_eq!(gauges.get(OWNED_ATTEMPTS_METRIC, None), 0.0);
            assert_eq!(gauges.get(COMPLETION_MEMBERSHIPS_METRIC, None), 0.0);
            assert!(completions.lock_queue().is_empty());
        });
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
    fn entire_batch_retires_before_success_failure_cancellation_or_pruning_wakes_a_waiter() {
        let gauges = crate::engine::tests::Gauges::default();
        metrics::with_local_recorder(&gauges, || {
            for result in [
                Some(Ok(Written::Applied)),
                Some(Ok(Written::Unchanged)),
                Some(Err(Observed)),
                None,
            ] {
                for prune in [false, true] {
                    let completions = Completions::default();
                    let capacity = Arc::new(tokio::sync::Semaphore::new(4));
                    let mut registrations = Vec::new();
                    let mut responses = Vec::new();
                    let mut observers = Vec::new();
                    for generation in 1..=4 {
                        let slots = admitted(&capacity);
                        let (registration, mut response) = completions.register(
                            ID,
                            generation,
                            Instant::now() + Duration::from_secs(10),
                            &slots,
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
                        registrations.push(registration);
                        responses.push(response);
                        observers.push(wake);
                    }
                    let mut batch =
                        CompletionBatch::new(std::mem::take(&mut *completions.lock_queue()));
                    drop(registrations);
                    assert_eq!(capacity.available_permits(), 0, "batch retains admission");
                    assert_eq!(gauges.get(OWNED_ATTEMPTS_METRIC, None), 4.0);
                    assert_eq!(gauges.get(COMPLETION_MEMBERSHIPS_METRIC, None), 4.0);

                    if prune {
                        batch.prune(|entry| entry.generation == 1);
                        assert!(matches!(
                            responses[0].try_recv(),
                            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                        ));
                        assert_eq!(
                            observers[0].available_at_wake.load(Ordering::SeqCst),
                            usize::MAX,
                            "a pruned sender stays open until the remaining batch retires"
                        );
                    }
                    match result {
                        Some(result) => {
                            let results = vec![result; batch.queued.len()];
                            batch.finish(results);
                        }
                        None => drop(batch),
                    }
                    assert_eq!(capacity.available_permits(), 4);
                    assert_eq!(gauges.get(OWNED_ATTEMPTS_METRIC, None), 0.0);
                    assert_eq!(gauges.get(COMPLETION_MEMBERSHIPS_METRIC, None), 0.0);
                    for (index, (wake, mut response)) in
                        observers.into_iter().zip(responses).enumerate()
                    {
                        assert_eq!(wake.available_at_wake.load(Ordering::SeqCst), 4);
                        match result.filter(|_| !prune || index != 0) {
                            Some(result) => assert_eq!(response.try_recv().unwrap(), result),
                            None => assert!(matches!(
                                response.try_recv(),
                                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
                            )),
                        }
                    }
                }
            }
        });
    }

    fn outcome(ended: Ended, attempt: u16) -> Transition {
        map_outcome("sample", attempt, Policy::default(), ended)
    }

    #[derive(serde::Serialize)]
    struct DecodeProbe(bool);

    impl<'de> serde::Deserialize<'de> for DecodeProbe {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let panic = <bool as serde::Deserialize>::deserialize(deserializer)?;
            assert!(!panic, "payload-secret");
            Ok(Self(panic))
        }
    }

    impl crate::JobKind for DecodeProbe {
        const NAME: &'static str = "test.decode_probe";
    }

    #[tokio::test]
    async fn payload_preparation_panic_is_a_known_failure_and_later_work_runs() {
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handler_ran = Arc::clone(&ran);
        let mut kinds = crate::KindsBuilder::new();
        kinds.register(Policy::default(), move |_: crate::Job<DecodeProbe>| {
            let ran = Arc::clone(&handler_ran);
            async move {
                ran.store(true, Ordering::Relaxed);
                Ok(())
            }
        });
        let registry = kinds.validate().unwrap();
        let registered = registry.get(<DecodeProbe as crate::JobKind>::NAME).unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let attempt = || Attempt {
            id: ID,
            number: 1,
            generation: 1,
            attempt_deadline: Instant::now() + Duration::from_secs(1),
            cancellation: CancellationToken::new(),
            pool: pool.clone(),
        };
        let failed = prepare_handler(registered.dispatch.as_ref(), attempt(), b"true");
        let Err(ended) = failed else {
            panic!("a preparation panic must not return a handler");
        };
        assert!(matches!(ended, Ended::Panic));
        assert!(!ran.load(Ordering::Relaxed));

        let Ok(later) = prepare_handler(registered.dispatch.as_ref(), attempt(), b"false") else {
            panic!("a valid payload still prepares");
        };
        later.await.unwrap();
        assert!(ran.load(Ordering::Relaxed));
        pool.close().await;
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "attempt^4 * 1e6 is exact in f64")]
    fn dispositions_preserve_cap_and_snooze() {
        assert!(matches!(outcome(Ended::Success, 25), Transition::Complete));
        assert!(matches!(
            outcome(Ended::Error(JobError::retryable("fail")), 25),
            Transition::Fail {
                reason: FailureReason::Exhausted,
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
            Transition::Snooze {
                delay_micros: 7,
                ..
            }
        ));
        assert!(matches!(
            outcome(Ended::Error(JobError::unavailable("pool timed out")), 25),
            Transition::Snooze { delay_micros: 10_000_000, ref summary } if summary == "pool timed out"
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
                reason: FailureReason::Permanent,
                ..
            }
        ));
    }

    #[test]
    fn local_waits_clamp_to_the_supervisor_deadline() {
        let base = Instant::now();
        let limit = base + Duration::from_secs(10);
        assert_eq!(
            clamped_deadline(base, Duration::from_secs(1), limit),
            base + Duration::from_secs(1)
        );
        assert_eq!(
            clamped_deadline(base, Duration::from_secs(10), limit),
            limit,
            "a wait that reaches the limit keeps it"
        );
        assert_eq!(
            clamped_deadline(base, Duration::from_secs(600), limit),
            limit
        );
        assert_eq!(
            clamped_deadline(base, Duration::MAX, limit),
            limit,
            "an overflowing sum takes the limit"
        );
    }

    #[test]
    fn a_completed_attempt_selects_no_single_row_write() {
        assert!(write_of(&Transition::Complete).is_none());
        assert!(matches!(
            write_of(&Transition::Release),
            Some(Write::Release)
        ));
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "the view copies the exact binding")]
    fn the_write_view_carries_each_statement_binding() {
        let retry = Transition::Retry {
            summary: "retry me".to_owned(),
            base_micros: 81_000_000.0,
            floor_micros: 2_000_000,
            timed_out: true,
        };
        assert!(matches!(
            write_of(&retry),
            Some(Write::Retry {
                summary,
                base_micros,
                floor_micros,
            }) if summary == "retry me" && base_micros == 81_000_000.0 && floor_micros == 2_000_000
        ));
        let snooze = Transition::Snooze {
            delay_micros: 7,
            summary: String::new(),
        };
        assert!(matches!(
            write_of(&snooze),
            Some(Write::Snooze { delay_micros: 7 })
        ));
        let fail = Transition::Fail {
            reason: FailureReason::Exhausted,
            summary: "done".to_owned(),
        };
        assert!(matches!(
            write_of(&fail),
            Some(Write::Fail {
                reason: FailureReason::Exhausted,
                summary,
            }) if summary == "done"
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
        assert_eq!(sanitize_summary("a\nb\0c"), "a b c");
        assert_eq!(sanitize_summary(""), "");
        assert_eq!(sanitize_summary(&"x".repeat(1024)), "x".repeat(1024));
        assert_eq!(sanitize_summary(&"x".repeat(65_536)), "x".repeat(1024));
        assert_eq!(
            sanitize_summary(&("\u{0085}".repeat(1023) + "é")),
            " ".repeat(1023)
        );
        assert_eq!(
            sanitize_summary(&("é".repeat(512) + "tail")),
            "é".repeat(512)
        );
        assert_eq!(
            sanitize_summary(&("a".repeat(1023) + "é")),
            "a".repeat(1023)
        );
        let error = serde_json::from_str::<u32>("\"secret\"").unwrap_err();
        assert_eq!(
            payload_summary("sample", &error),
            "payload does not decode as sample: data error at line 1 column 8"
        );
    }
}
