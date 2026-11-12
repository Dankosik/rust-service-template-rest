//! One supervisor owns its handler and fixed queue outcome through cleanup.

mod completion;
mod outcome;

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::FutureExt as _;
use metrics::Unit;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::engine::{ForcedCleanup, Operation, RECORD_RETRY_INTERVAL, Shared, observe_recovery};
use crate::kind::{Attempt, HandlerFuture, JobError, JobId, Registered};
use crate::state::FailureReason;

pub(crate) use completion::Completions;
use completion::complete_batched;
pub(crate) use outcome::complete_query;
use outcome::{Ended, Transition, Unacknowledged, Written, map_outcome, send_outcome, write_of};

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
const ATTEMPTS_METRIC: &str = "jobs_attempts_total";
/// Queue-write acknowledgement, not attribution. Labels `kind`, `disposition`.
const PERSISTENCE_METRIC: &str = "jobs_persistence_total";
/// Handler run time. Label `kind`.
pub const ATTEMPT_DURATION_METRIC: &str = "jobs_attempt_duration_seconds";
pub(crate) const ATTEMPT_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0,
    1800.0, 3600.0,
];

pub(crate) fn describe_metrics() {
    metrics::describe_gauge!(
        OWNED_ATTEMPTS_METRIC,
        Unit::Count,
        "Attempt slots currently owned"
    );
    metrics::describe_gauge!(
        COMPLETION_MEMBERSHIPS_METRIC,
        Unit::Count,
        "Queued and in-flight completion entries currently owned"
    );
    // Register idle observations without resetting other engines' live gauge values.
    metrics::gauge!(OWNED_ATTEMPTS_METRIC).increment(0.0);
    metrics::gauge!(COMPLETION_MEMBERSHIPS_METRIC).increment(0.0);
    metrics::describe_counter!(
        ATTEMPTS_METRIC,
        Unit::Count,
        "Observed attempt outcomes. Terminal failures count only after an acknowledged write."
    );
    metrics::describe_histogram!(
        ATTEMPT_DURATION_METRIC,
        Unit::Seconds,
        "Observed handler duration in seconds"
    );
    metrics::describe_counter!(
        PERSISTENCE_METRIC,
        Unit::Count,
        "Queue outcome acknowledgement disposition"
    );
}

/// An outcome write's `jobs_persistence_total` disposition.
#[derive(Clone, Copy)]
enum Persistence {
    Applied,
    Unchanged,
    /// Neither acknowledgement arrived; lease expiry recovers the row.
    Unknown,
}

impl Persistence {
    const fn label(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Unchanged => "unchanged",
            Self::Unknown => "unknown",
        }
    }
}

fn attempts_counter(kind: &'static str, outcome: &'static str) -> metrics::Counter {
    metrics::counter!(ATTEMPTS_METRIC, "kind" => kind, "outcome" => outcome)
}

fn persistence_counter(kind: &'static str, persistence: Persistence) -> metrics::Counter {
    metrics::counter!(PERSISTENCE_METRIC, "kind" => kind, "disposition" => persistence.label())
}

/// Metric handles of the completed-attempt path for one kind, registered
/// once instead of looked up by name for every attempt.
pub(crate) struct KindMetrics {
    completed: metrics::Counter,
    duration: metrics::Histogram,
    applied: metrics::Counter,
    pub(crate) queue_wait: metrics::Histogram,
}

impl KindMetrics {
    pub(crate) fn new(kind: &'static str) -> Self {
        Self {
            completed: attempts_counter(kind, Transition::Complete.label()),
            duration: metrics::histogram!(ATTEMPT_DURATION_METRIC, "kind" => kind),
            applied: persistence_counter(kind, Persistence::Applied),
            queue_wait: metrics::histogram!(crate::claim::QUEUE_WAIT_METRIC, "kind" => kind),
        }
    }
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

/// The gauge whose units [`MetricMembership`] holds, looked up once.
///
/// [`crate::Engine::start`] binds it under the installed recorder; first use
/// binds it otherwise. A lookup per attempt would hash the name and search the
/// recorder's registry, one of them under the completion queue lock.
pub(crate) struct MembershipGauge {
    name: &'static str,
    handle: std::sync::OnceLock<metrics::Gauge>,
}

impl MembershipGauge {
    const fn new(name: &'static str) -> Self {
        Self {
            name,
            handle: std::sync::OnceLock::new(),
        }
    }

    pub(crate) const fn owned_attempts() -> Self {
        Self::new(OWNED_ATTEMPTS_METRIC)
    }

    pub(crate) fn bind(&self) -> &metrics::Gauge {
        self.handle.get_or_init(|| metrics::gauge!(self.name))
    }

    fn join(&self) -> MetricMembership {
        let gauge = self.bind().clone();
        gauge.increment(1.0);
        MetricMembership(gauge)
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
    pub(crate) job_id: JobId,
    pub(crate) kind: &'static str,
    /// Attempts used, including this one.
    pub(crate) number: u16,
}

/// The claim an outcome write matches: the job row, and the generation its
/// claim took. A row a later claim took no longer matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClaimFence {
    pub(crate) job_id: JobId,
    pub(crate) generation: i64,
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

fn observe_attempt(handles: Option<&KindMetrics>, attempt: &AttemptId, transition: &Transition) {
    match handles {
        Some(handles) if matches!(transition, Transition::Complete) => {
            handles.completed.increment(1);
        }
        _ => attempts_counter(attempt.kind, transition.label()).increment(1),
    }
    let outcome = transition.label();
    match transition {
        Transition::Complete => {
            tracing::debug!(job.id = %attempt.job_id, job.kind = attempt.kind, job.attempt = attempt.number, outcome, "job_attempt_completed");
        }
        Transition::Snooze { summary, .. } => {
            tracing::info!(job.id = %attempt.job_id, job.kind = attempt.kind, job.attempt = attempt.number, outcome, error = error_field(summary), "job_attempt_finished");
        }
        Transition::Release => {
            tracing::info!(job.id = %attempt.job_id, job.kind = attempt.kind, job.attempt = attempt.number, outcome, "job_attempt_finished");
        }
        Transition::Retry { summary, .. } => {
            tracing::info!(job.id = %attempt.job_id, job.kind = attempt.kind, job.attempt = attempt.number, outcome, error = error_field(summary), "job_attempt_failed");
        }
        Transition::Fail { summary, .. } => {
            tracing::warn!(job.id = %attempt.job_id, job.kind = attempt.kind, job.attempt = attempt.number, job.failure_reason = outcome, error = summary.as_str(), "job_failed");
        }
    }
}

/// A summary as an event's `error` field, absent when there is none.
fn error_field(summary: &str) -> Option<&str> {
    (!summary.is_empty()).then_some(summary)
}

/// The supervisor of one claimed attempt.
///
/// It retires `guard` once the attempt's outcome is written or known to be
/// uncertain.
pub(crate) fn supervise(
    shared: Arc<Shared>,
    claimed: crate::claim::Claimed,
    supervisor_deadline: Instant,
    mut guard: crate::engine::SupervisorGuard,
) -> impl Future<Output = ()> + Send + 'static {
    let crate::claim::Claimed {
        registered,
        attempt,
        generation,
        payload,
        trace,
        slot,
        kind_slot,
    } = claimed;
    let span = attempt_span(&attempt);
    crate::trace_context::link(&span, &trace);
    drop(trace);
    let slots = Arc::new(AttemptSlots {
        _membership: shared.owned_attempts.join(),
        _global: slot,
        _kind: kind_slot,
    });
    async move {
        run_attempt(
            &shared,
            &registered,
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
fn attempt_span(attempt: &AttemptId) -> tracing::Span {
    tracing::info_span!(
        "job_attempt",
        otel.name = %format_args!("process {}", attempt.kind),
        otel.kind = "consumer",
        otel.status_code = tracing::field::Empty,
        job.id = %attempt.job_id,
        job.kind = attempt.kind,
        job.attempt = u64::from(attempt.number),
        outcome = tracing::field::Empty,
    )
}

/// The attempt span and `jobs_attempts_total` share the outcome label.
fn record_on_span(span: &tracing::Span, transition: &Transition) {
    span.record("outcome", transition.label());
    match transition {
        Transition::Retry { .. } | Transition::Fail { .. } => {}
        Transition::Complete | Transition::Snooze { .. } | Transition::Release => return,
    }
    span.record("otel.status_code", "ERROR");
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
        observe_uncertain(shared, &attempt);
        return;
    }
    let policy = registered.policy;
    let attempt_deadline = clamped_deadline(Instant::now(), policy.timeout, supervisor_deadline);
    let cancel = CancellationToken::new();
    let prepared = prepare_handler(
        registered.dispatch.as_ref(),
        Attempt {
            job_id: attempt.job_id,
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
                &shared.cleanup,
                future,
                cancel,
                attempt_deadline,
                supervisor_deadline,
            )
            .await
            else {
                observe_uncertain(shared, &attempt);
                return;
            };
            (ended, Some(started.elapsed()))
        }
    };
    let transition = map_outcome(&attempt, policy, ended);
    if matches!(transition, Transition::Release) {
        shared.counters.cancelled.fetch_add(1, Ordering::Relaxed);
    } else {
        shared
            .counters
            .known_results
            .fetch_add(1, Ordering::Relaxed);
    }
    record_on_span(&tracing::Span::current(), &transition);
    let handles = registered.metrics();
    if let Some(ran) = ran {
        handles.duration.record(ran.as_secs_f64());
    }
    let terminal = matches!(transition, Transition::Fail { .. });
    if !terminal {
        observe_attempt(Some(handles), &attempt, &transition);
    }
    let written = persist(
        shared,
        handles,
        &attempt,
        generation,
        &transition,
        supervisor_deadline,
        &slots,
    )
    .await;
    if terminal {
        observe_terminal(handles, &attempt, &transition, written);
    }
}

/// A terminal failure counts only once its write applies. A row the write did
/// not move is failed, re-run, or exhausted by its lease rescue, which counts
/// it; counting it here too would count one job twice.
fn observe_terminal(
    handles: &KindMetrics,
    attempt: &AttemptId,
    transition: &Transition,
    written: Option<Written>,
) {
    match (transition, written) {
        (_, Some(Written::Applied)) => observe_attempt(Some(handles), attempt, transition),
        (Transition::Fail { summary, .. }, _) => {
            tracing::info!(job.id = %attempt.job_id, job.kind = attempt.kind, job.attempt = attempt.number, outcome = transition.label(), error = error_field(summary), "job_attempt_failed");
        }
        _ => {}
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
async fn within<T>(
    cleanup: &ForcedCleanup,
    until: Instant,
    future: impl Future<Output = T>,
) -> Option<T> {
    let mut future = std::pin::pin!(future);
    let bounded = std::future::poll_fn(|cx| {
        // A resumed poll can send SQL, even when it initially only waited for I/O.
        if Instant::now() >= until || cleanup.has_ended() {
            std::task::Poll::Ready(None)
        } else {
            future.as_mut().poll(cx).map(Some)
        }
    });
    tokio::select! {
        biased;
        value = bounded => value,
        () = cleanup.ended() => None,
        () = tokio::time::sleep_until(until) => None,
    }
}

fn expired(shared: &Shared, until: Instant) -> bool {
    Instant::now() >= until || shared.cleanup.has_ended()
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

/// Before the attempt deadline, a ready handler result wins over a drain
/// request within the supervisor and cleanup bounds. After cancellation only
/// success wins.
async fn drive(
    cleanup: &ForcedCleanup,
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
        let mut first_poll = true;
        let admitted = std::future::poll_fn(|cx| {
            let now = Instant::now();
            if now >= supervisor_deadline || cleanup.has_ended() {
                cancel.cancel();
                return std::task::Poll::Ready(None);
            }
            let first = std::mem::take(&mut first_poll);
            if now >= attempt_deadline {
                // Preparation can consume the attempt's budget before admission.
                if first {
                    cancel.cancel();
                    return std::task::Poll::Ready(Some(Ended::Timeout));
                }
                // The cancellation branches supply the wakeup and reason. The
                // handler may resume for cleanup only after its token fires.
                return std::task::Poll::Pending;
            }
            run.as_mut().poll(cx).map(|result| Some(ended_from(result)))
        });
        let reason = tokio::select! {
            biased;
            ended = admitted => return ended,
            () = cleanup.forced() => Ended::Cancelled,
            () = tokio::time::sleep_until(attempt_deadline) => Ended::Timeout,
        };
        cancel.cancel();
        let grace = clamped_deadline(Instant::now(), COOPERATIVE_GRACE, supervisor_deadline);
        if let Some(result) = within(cleanup, grace, &mut run).await {
            return Some(ended_after_cancel(&result, reason));
        }
        reason
    };
    handler.destroy();
    (Instant::now() < supervisor_deadline && !cleanup.has_ended()).then_some(reason)
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
    handles: &KindMetrics,
    attempt: &AttemptId,
    generation: i64,
    transition: &Transition,
    supervisor_deadline: Instant,
    slots: &Arc<AttemptSlots>,
) -> Option<Written> {
    let write = write_of(transition);
    let operation = write.map_or(Operation::Record, outcome::Write::operation);
    let fence = ClaimFence {
        job_id: attempt.job_id,
        generation,
    };
    loop {
        if expired(shared, supervisor_deadline) {
            break;
        }
        let sent = within(&shared.cleanup, supervisor_deadline, async {
            match write {
                None => complete_batched(shared, fence, supervisor_deadline, slots).await,
                Some(write) => send_outcome(shared, fence, write, supervisor_deadline).await,
            }
        })
        .await;
        match sent {
            None => break,
            Some(Ok(written)) => {
                observe_acknowledged(shared, handles, attempt, operation, written);
                return Some(written);
            }
            Some(Err(Unacknowledged)) => {}
        }
        let until = clamped_deadline(Instant::now(), RECORD_RETRY_INTERVAL, supervisor_deadline);
        if within(
            &shared.cleanup,
            supervisor_deadline,
            tokio::time::sleep_until(until),
        )
        .await
        .is_none()
        {
            break;
        }
    }
    observe_uncertain(shared, attempt);
    None
}

fn observe_acknowledged(
    shared: &Shared,
    handles: &KindMetrics,
    attempt: &AttemptId,
    operation: Operation,
    written: Written,
) {
    observe_recovery(shared, operation);
    let persistence = written.persistence();
    match written {
        Written::Applied => handles.applied.increment(1),
        Written::Unchanged => persistence_counter(attempt.kind, persistence).increment(1),
    }
    tracing::debug!(
        job.id = %attempt.job_id,
        job.kind = attempt.kind,
        job.attempt = attempt.number,
        disposition = persistence.label(),
        "job_persistence_finished"
    );
    if written == Written::Applied && operation == Operation::Release {
        shared.counters.released.fetch_add(1, Ordering::Relaxed);
    }
}

/// The attempt's queue transition is not known to have landed; lease expiry
/// recovers the row.
fn observe_uncertain(shared: &Shared, attempt: &AttemptId) {
    shared.counters.uncertain.fetch_add(1, Ordering::Relaxed);
    persistence_counter(attempt.kind, Persistence::Unknown).increment(1);
    tracing::warn!(
        job.id = %attempt.job_id,
        job.kind = attempt.kind,
        job.attempt = attempt.number,
        disposition = Persistence::Unknown.label(),
        "job_persistence_finished"
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::Policy;

    const ID: JobId = JobId(uuid::Uuid::nil());

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
            job_id: ID,
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
        std::assert_matches!(ended, Ended::Panic);
        assert!(!ran.load(Ordering::Relaxed));

        let Ok(later) = prepare_handler(registered.dispatch.as_ref(), attempt(), b"false") else {
            panic!("a valid payload still prepares");
        };
        later.await.unwrap();
        assert!(ran.load(Ordering::Relaxed));
        pool.close().await;
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

    #[tokio::test(start_paused = true)]
    async fn an_expired_attempt_does_not_start_its_handler() {
        let cancel = CancellationToken::new();
        let (ran, observed) = tokio::sync::oneshot::channel();
        let deadline = Instant::now() + Duration::from_secs(1);
        tokio::time::advance(Duration::from_secs(1)).await;

        let ended = drive(
            &ForcedCleanup::default(),
            Box::pin(async move {
                let _ = ran.send(());
                Ok(())
            }),
            cancel.clone(),
            deadline,
            deadline + Duration::from_secs(5),
        )
        .await;

        std::assert_matches!(ended, Some(Ended::Timeout));
        assert!(cancel.is_cancelled());
        assert!(observed.await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_suspended_handler_observes_cancellation_on_resuming_after_its_attempt_deadline() {
        let cleanup = ForcedCleanup::default();
        let cancel = CancellationToken::new();
        let handler_cancel = cancel.clone();
        let deadline = Instant::now() + Duration::from_secs(1);
        let (resume, suspended) = tokio::sync::oneshot::channel();
        let (ran, observed) = tokio::sync::oneshot::channel();
        let mut attempt = std::pin::pin!(drive(
            &cleanup,
            Box::pin(async move {
                suspended.await.unwrap();
                let _ = ran.send(handler_cancel.is_cancelled());
                Err(JobError::retryable("handler stopped"))
            }),
            cancel,
            deadline,
            deadline + Duration::from_secs(5),
        ));
        assert!(futures_util::poll!(attempt.as_mut()).is_pending());
        tokio::time::advance(Duration::from_secs(1)).await;
        resume.send(()).unwrap();

        std::assert_matches!(attempt.await, Some(Ended::Timeout));
        assert!(observed.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn a_suspended_handler_does_not_resume_after_its_supervisor_deadline() {
        let cleanup = ForcedCleanup::default();
        let cancel = CancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(1);
        let (resume, suspended) = tokio::sync::oneshot::channel();
        let (ran, observed) = tokio::sync::oneshot::channel();
        let mut attempt = std::pin::pin!(drive(
            &cleanup,
            Box::pin(async move {
                suspended.await.unwrap();
                let _ = ran.send(());
                Ok(())
            }),
            cancel.clone(),
            deadline,
            deadline,
        ));
        assert!(futures_util::poll!(attempt.as_mut()).is_pending());
        tokio::time::advance(Duration::from_secs(1)).await;
        resume.send(()).unwrap();

        assert!(attempt.await.is_none());
        assert!(cancel.is_cancelled());
        assert!(observed.await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn an_expired_wait_does_not_resume_work_that_is_ready_again() {
        let cleanup = ForcedCleanup::default();
        let delay = Duration::from_secs(1);
        let deadline = Instant::now() + delay;
        let ran = std::cell::Cell::new(false);
        let mut work = std::pin::pin!(within(&cleanup, deadline, async {
            tokio::time::sleep_until(deadline).await;
            ran.set(true);
        }));
        assert!(futures_util::poll!(work.as_mut()).is_pending());
        tokio::time::advance(delay).await;
        assert_eq!(work.await, None);
        assert!(!ran.get());
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
                let span = attempt_span(&AttemptId {
                    job_id: id,
                    kind: "sample",
                    number: 3,
                });
                let attempt = AttemptId {
                    job_id: id,
                    kind: "sample",
                    number: 3,
                };
                record_on_span(&span, &map_outcome(&attempt, Policy::default(), ended));
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
}
