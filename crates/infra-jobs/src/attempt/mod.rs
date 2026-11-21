mod completion;
mod outcome;

use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use enum_map::{Enum, EnumMap};
use futures_util::FutureExt as _;
use metrics::Unit;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::engine::{
    ForcedCleanup, Operation, OperationError, RECORD_RETRY_INTERVAL, WorkerGroup, backstop,
    observe_recovery,
};
use crate::kind::{Attempt, Completion, HandlerFuture, JobError, JobId, Registered};
use crate::state::FailureReason;

use completion::complete_batched;
pub(crate) use completion::{Completions, complete_query};
use outcome::{
    AttemptOutcome, HandlerOutcome, Transition, WritePlan, Written, map_outcome, send_outcome,
    write_of,
};

const COOPERATIVE_GRACE: Duration = Duration::from_millis(100);

const OWNED_ATTEMPTS_METRIC: &str = "jobs_owned_attempts";
const COMPLETION_MEMBERSHIPS_METRIC: &str = "jobs_completion_memberships";

const ATTEMPTS_METRIC: &str = "jobs_attempts_total";
const LAST_TERMINAL_ATTEMPT_TIMESTAMP_METRIC: &str = "jobs_last_terminal_attempt_timestamp_seconds";
const PERSISTENCE_METRIC: &str = "jobs_persistence_total";
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
    metrics::gauge!(OWNED_ATTEMPTS_METRIC).increment(0.0);
    metrics::gauge!(COMPLETION_MEMBERSHIPS_METRIC).increment(0.0);
    metrics::describe_counter!(
        ATTEMPTS_METRIC,
        Unit::Count,
        "Observed attempt outcomes. Terminal failures count only after an acknowledged write."
    );
    metrics::describe_gauge!(
        LAST_TERMINAL_ATTEMPT_TIMESTAMP_METRIC,
        Unit::Seconds,
        "Unix timestamp of the last acknowledged terminal failure by job kind"
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

#[derive(Clone, Copy, Enum)]
enum Persistence {
    Applied,
    Unchanged,
    Unknown,
}

impl Persistence {
    const fn acknowledged(written: Written) -> Self {
        match written {
            Written::Applied => Self::Applied,
            Written::Unchanged => Self::Unchanged,
        }
    }

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

pub(crate) struct KindMetrics {
    kind: &'static str,
    attempts: EnumMap<AttemptOutcome, OnceLock<metrics::Counter>>,
    persistence: EnumMap<Persistence, OnceLock<metrics::Counter>>,
    last_terminal_attempt_timestamp: OnceLock<metrics::Gauge>,
    duration: metrics::Histogram,
    pub(crate) queue_wait: metrics::Histogram,
}

impl KindMetrics {
    pub(crate) fn new(kind: &'static str) -> Self {
        let metrics = Self {
            kind,
            attempts: EnumMap::default(),
            persistence: EnumMap::default(),
            last_terminal_attempt_timestamp: OnceLock::new(),
            duration: metrics::histogram!(ATTEMPT_DURATION_METRIC, "kind" => kind),
            queue_wait: metrics::histogram!(crate::claim::QUEUE_WAIT_METRIC, "kind" => kind),
        };
        metrics.attempt(AttemptOutcome::Completed);
        metrics.persistence(Persistence::Applied);
        metrics
    }

    fn attempt(&self, outcome: AttemptOutcome) -> &metrics::Counter {
        self.attempts[outcome].get_or_init(|| attempts_counter(self.kind, outcome.label()))
    }

    fn persistence(&self, disposition: Persistence) -> &metrics::Counter {
        self.persistence[disposition].get_or_init(|| persistence_counter(self.kind, disposition))
    }

    fn last_terminal_attempt_timestamp(&self) -> &metrics::Gauge {
        self.last_terminal_attempt_timestamp.get_or_init(
            || metrics::gauge!(LAST_TERMINAL_ATTEMPT_TIMESTAMP_METRIC, "kind" => self.kind),
        )
    }
}

struct AttemptSlots {
    _membership: MetricMembership,
    _group: tokio::sync::OwnedSemaphorePermit,
    _kind: Option<crate::claim::KindSlot>,
}

struct MetricMembership(metrics::Gauge);

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

#[derive(Debug)]
pub(crate) struct AttemptId {
    pub(crate) job_id: JobId,
    pub(crate) kind: &'static str,
    pub(crate) number: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClaimFence {
    pub(crate) job_id: JobId,
    pub(crate) generation: i64,
    pub(crate) token: uuid::Uuid,
}

pub(crate) fn record_exhausted(handles: &KindMetrics, attempt: &AttemptId, summary: &str) {
    observe_terminal_failure(handles, attempt, FailureReason::Exhausted, summary);
}

fn observe_attempt(handles: &KindMetrics, attempt: &AttemptId, transition: &Transition) {
    let outcome = transition.outcome();
    let label = outcome.label();
    match transition {
        Transition::Fail { .. } => return,
        Transition::Complete(_) => {
            tracing::debug!(job.id = %attempt.job_id, job.kind = attempt.kind, job.attempt = attempt.number, outcome = label, "job_attempt_completed");
        }
        Transition::Snooze { summary, .. } => {
            tracing::info!(job.id = %attempt.job_id, job.kind = attempt.kind, job.attempt = attempt.number, outcome = label, error = error_field(summary), "job_attempt_finished");
        }
        Transition::Release => {
            tracing::info!(job.id = %attempt.job_id, job.kind = attempt.kind, job.attempt = attempt.number, outcome = label, "job_attempt_finished");
        }
        Transition::Retry { summary, .. } => {
            log_attempt_failed(attempt, label, summary);
        }
    }
    handles.attempt(outcome).increment(1);
}

fn observe_terminal_failure(
    handles: &KindMetrics,
    attempt: &AttemptId,
    reason: FailureReason,
    summary: &str,
) {
    let outcome = reason.as_str();
    handles
        .attempt(AttemptOutcome::of_failure(reason))
        .increment(1);
    if let Ok(timestamp) = SystemTime::now().duration_since(UNIX_EPOCH) {
        handles
            .last_terminal_attempt_timestamp()
            .set(timestamp.as_secs_f64());
    }
    tracing::warn!(
        job.id = %attempt.job_id,
        job.kind = attempt.kind,
        job.attempt = attempt.number,
        outcome,
        job.failure_reason = outcome,
        error = summary,
        "job_failed"
    );
}

fn log_attempt_failed(attempt: &AttemptId, outcome: &'static str, summary: &str) {
    tracing::info!(
        job.id = %attempt.job_id,
        job.kind = attempt.kind,
        job.attempt = attempt.number,
        outcome,
        error = error_field(summary),
        "job_attempt_failed"
    );
}

fn error_field(summary: &str) -> Option<&str> {
    (!summary.is_empty()).then_some(summary)
}

pub(crate) fn supervise(
    group: Arc<WorkerGroup>,
    claimed: crate::claim::Claimed,
    supervisor_deadline: Instant,
    mut guard: crate::engine::SupervisorGuard,
) -> impl Future<Output = ()> + Send + 'static {
    let crate::claim::RunningJob {
        registered,
        attempt,
        fence,
        payload,
        trace,
    } = claimed.job;
    let span = attempt_span(&attempt);
    crate::trace_context::link(&span, &trace);
    drop(trace);
    let slots = Arc::new(AttemptSlots {
        _membership: group.owned_attempts.join(),
        _group: claimed.slot,
        _kind: claimed.kind_slot,
    });
    async move {
        AttemptRun {
            group: &group,
            registered: &registered,
            attempt,
            fence,
            supervisor_deadline,
            slots,
        }
        .run(payload)
        .instrument(span)
        .await;
        guard.retire();
    }
}

struct AttemptRun<'a> {
    group: &'a WorkerGroup,
    registered: &'a Registered,
    attempt: AttemptId,
    fence: ClaimFence,
    supervisor_deadline: Instant,
    slots: Arc<AttemptSlots>,
}

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

fn record_on_span(span: &tracing::Span, transition: &Transition) {
    let outcome = transition.outcome();
    span.record("outcome", outcome.label());
    if outcome.is_span_error() {
        span.record("otel.status_code", "ERROR");
    }
}

impl AttemptRun<'_> {
    async fn run(self, payload: Result<Vec<u8>, JobError>) {
        let Some((handler_outcome, handler_duration)) = self.execute_handler(payload).await else {
            self.observe_uncertain();
            return;
        };
        let transition = map_outcome(
            &self.attempt,
            self.registered.policy,
            self.registered.mode,
            handler_outcome,
        );
        let handles = self.registered.metrics();
        self.observe_outcome(handles, &transition, handler_duration);
        let Some(plan) = write_of(&transition) else {
            self.observe_acknowledged(handles, Operation::Record, Written::Applied);
            return;
        };
        let written = self.persist(plan).await;
        self.finish_persistence(handles, &transition, plan.operation(), written);
    }

    async fn execute_handler(
        &self,
        payload: Result<Vec<u8>, JobError>,
    ) -> Option<(HandlerOutcome, Option<Duration>)> {
        if deadline_reached_or_cleanup_ended(
            &self.group.lifecycle.cleanup,
            self.supervisor_deadline,
        ) {
            return None;
        }
        let attempt_deadline = clamped_deadline(
            Instant::now(),
            self.registered.policy.timeout,
            self.supervisor_deadline,
        );
        let cancel = CancellationToken::new();
        let prepared = payload.map_err(HandlerOutcome::Error).and_then(|bytes| {
            prepare_handler(
                self.registered.handler.as_ref(),
                Attempt {
                    fence: self.fence,
                    kind: self.attempt.kind,
                    number: self.attempt.number,
                    attempt_deadline,
                    cancellation: cancel.clone(),
                },
                self.group.pool.clone(),
                &bytes,
            )
        });
        match prepared {
            Err(handler_outcome) => Some((handler_outcome, None)),
            Ok(future) => {
                let started = Instant::now();
                let handler_outcome = drive(
                    &self.group.lifecycle.cleanup,
                    future,
                    cancel,
                    attempt_deadline,
                    self.supervisor_deadline,
                )
                .await?;
                Some((handler_outcome, Some(started.elapsed())))
            }
        }
    }

    async fn persist(&self, plan: WritePlan<'_>) -> Option<Written> {
        loop {
            let attempt = async {
                match plan {
                    WritePlan::CompleteBatched => {
                        complete_batched(
                            self.group,
                            self.fence,
                            self.supervisor_deadline,
                            &self.slots,
                        )
                        .await
                    }
                    WritePlan::Single(write) => {
                        send_outcome(self.group, self.fence, write, self.supervisor_deadline).await
                    }
                }
            };
            if let Ok(written) = within(
                &self.group.lifecycle.cleanup,
                self.supervisor_deadline,
                attempt,
            )
            .await?
            {
                return Some(written);
            }
            let until = clamped_deadline(
                Instant::now(),
                RECORD_RETRY_INTERVAL,
                self.supervisor_deadline,
            );
            within(
                &self.group.lifecycle.cleanup,
                self.supervisor_deadline,
                tokio::time::sleep_until(until),
            )
            .await?;
        }
    }

    fn observe_outcome(
        &self,
        handles: &KindMetrics,
        transition: &Transition,
        handler_duration: Option<Duration>,
    ) {
        if matches!(transition, Transition::Release) {
            self.group
                .lifecycle
                .counters
                .cancelled
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.group
                .lifecycle
                .counters
                .known_results
                .fetch_add(1, Ordering::Relaxed);
        }
        record_on_span(&tracing::Span::current(), transition);
        if let Some(duration) = handler_duration {
            handles.duration.record(duration);
        }
        observe_attempt(handles, &self.attempt, transition);
    }

    fn finish_persistence(
        &self,
        handles: &KindMetrics,
        transition: &Transition,
        operation: Operation,
        written: Option<Written>,
    ) {
        match written {
            Some(written) => self.observe_acknowledged(handles, operation, written),
            None => self.observe_uncertain(),
        }
        if let Transition::Fail { reason, summary } = transition {
            if written == Some(Written::Applied) {
                observe_terminal_failure(handles, &self.attempt, *reason, summary);
            } else {
                log_attempt_failed(&self.attempt, transition.label(), summary);
            }
        }
    }

    fn observe_acknowledged(&self, handles: &KindMetrics, operation: Operation, written: Written) {
        observe_recovery(self.group, operation);
        let persistence = Persistence::acknowledged(written);
        handles.persistence(persistence).increment(1);
        tracing::debug!(
            job.id = %self.attempt.job_id,
            job.kind = self.attempt.kind,
            job.attempt = self.attempt.number,
            disposition = persistence.label(),
            "job_persistence_finished"
        );
        if written == Written::Applied && operation == Operation::Release {
            self.group
                .lifecycle
                .counters
                .released
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn observe_uncertain(&self) {
        self.group
            .lifecycle
            .counters
            .uncertain
            .fetch_add(1, Ordering::Relaxed);
        self.registered
            .metrics()
            .persistence(Persistence::Unknown)
            .increment(1);
        tracing::warn!(
            job.id = %self.attempt.job_id,
            job.kind = self.attempt.kind,
            job.attempt = self.attempt.number,
            disposition = Persistence::Unknown.label(),
            "job_persistence_finished"
        );
    }
}

fn prepare_handler<'a>(
    handler: &'a dyn crate::kind::ErasedHandler,
    attempt: Attempt,
    pool: infra_postgres::Pool,
    payload: &[u8],
) -> Result<HandlerFuture<'a>, HandlerOutcome> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handler.prepare(attempt, pool, payload)
    }))
    .map_err(|_| HandlerOutcome::Panic)?
    .map_err(HandlerOutcome::PayloadDecode)
}

async fn within<T>(
    cleanup: &ForcedCleanup,
    until: Instant,
    future: impl Future<Output = T>,
) -> Option<T> {
    let mut future = std::pin::pin!(future);
    let bounded = std::future::poll_fn(|cx| {
        if deadline_reached_or_cleanup_ended(cleanup, until) {
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

pub(super) async fn execute_before_deadline<T, F>(
    group: &WorkerGroup,
    acquire_operation: &'static str,
    deadline: Instant,
    query: F,
) -> Result<T, OperationError>
where
    F: AsyncFnOnce(&mut sqlx::postgres::PgConnection) -> Result<T, sqlx::Error>,
{
    backstop(Box::pin(async {
        let mut connection = group.acquire(acquire_operation).await?;
        if deadline_reached_or_cleanup_ended(&group.lifecycle.cleanup, deadline) {
            return Err(OperationError::TimedOut);
        }
        query(&mut connection).await.map_err(OperationError::from)
    }))
    .await
}

fn deadline_reached_or_cleanup_ended(cleanup: &ForcedCleanup, until: Instant) -> bool {
    Instant::now() >= until || cleanup.has_ended()
}

fn clamped_deadline(base: Instant, wait: Duration, limit: Instant) -> Instant {
    base.checked_add(wait).unwrap_or(limit).min(limit)
}

async fn drive(
    cleanup: &ForcedCleanup,
    future: HandlerFuture<'_>,
    cancel: CancellationToken,
    attempt_deadline: Instant,
    supervisor_deadline: Instant,
) -> Option<HandlerOutcome> {
    let _cancel_on_drop = cancel.drop_guard_ref();
    let mut handler = std::mem::DropGuard::new(future, |future| {
        if let Err(payload) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(future)))
        {
            tracing::warn!("job_handler_drop_panicked");
            drop(payload);
        }
    });
    let reason = {
        let mut run = std::pin::pin!(std::panic::AssertUnwindSafe(&mut *handler).catch_unwind());
        let mut first_poll = true;
        let bounded_run = std::future::poll_fn(|cx| {
            let now = Instant::now();
            if now >= supervisor_deadline || cleanup.has_ended() {
                cancel.cancel();
                return std::task::Poll::Ready(None);
            }
            let first = std::mem::take(&mut first_poll);
            if now >= attempt_deadline {
                if first {
                    cancel.cancel();
                    return std::task::Poll::Ready(Some(HandlerOutcome::Timeout));
                }
                return std::task::Poll::Pending;
            }
            run.as_mut()
                .poll(cx)
                .map(|result| Some(outcome_from(result)))
        });
        tokio::select! {
            biased;
            handler_outcome = bounded_run => return handler_outcome,
            () = cleanup.forced() => {}
            () = tokio::time::sleep_until(attempt_deadline) => {}
        }
        let reason = if cleanup.requested_before(attempt_deadline) {
            HandlerOutcome::Cancelled
        } else {
            HandlerOutcome::Timeout
        };
        cancel.cancel();
        let grace = clamped_deadline(Instant::now(), COOPERATIVE_GRACE, supervisor_deadline);
        if let Some(result) = within(cleanup, grace, &mut run).await {
            return Some(outcome_after_cancel(&result, reason));
        }
        reason
    };
    drop(handler);
    (!deadline_reached_or_cleanup_ended(cleanup, supervisor_deadline)).then_some(reason)
}

fn outcome_after_cancel(
    result: &Result<Result<Completion, JobError>, Panicked>,
    reason: HandlerOutcome,
) -> HandlerOutcome {
    match result {
        Ok(Ok(completion)) => HandlerOutcome::Success(*completion),
        _ => reason,
    }
}

type Panicked = Box<dyn std::any::Any + Send>;

fn outcome_from(result: Result<Result<Completion, JobError>, Panicked>) -> HandlerOutcome {
    match result {
        Ok(Ok(completion)) => HandlerOutcome::Success(completion),
        Ok(Err(error)) => HandlerOutcome::Error(error),
        Err(_) => HandlerOutcome::Panic,
    }
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
        const NAME: crate::KindName = crate::KindName::new("test.decode_probe");
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
        let registered = registry
            .get(<DecodeProbe as crate::JobKind>::NAME.as_str())
            .unwrap();
        let pool =
            infra_postgres::Pool::connect_lazy_for_test("postgres://localhost/unused").unwrap();
        let attempt = || Attempt {
            fence: ClaimFence {
                job_id: ID,
                generation: 1,
                token: uuid::Uuid::nil(),
            },
            kind: <DecodeProbe as crate::JobKind>::NAME.as_str(),
            number: 1,
            attempt_deadline: Instant::now() + Duration::from_secs(1),
            cancellation: CancellationToken::new(),
        };
        let failed = prepare_handler(
            registered.handler.as_ref(),
            attempt(),
            pool.clone(),
            b"true",
        );
        let Err(handler_outcome) = failed else {
            panic!("a preparation panic must not return a handler");
        };
        std::assert_matches!(handler_outcome, HandlerOutcome::Panic);
        assert!(!ran.load(Ordering::Relaxed));

        let Ok(later) = prepare_handler(
            registered.handler.as_ref(),
            attempt(),
            pool.clone(),
            b"false",
        ) else {
            panic!("a valid payload still prepares");
        };
        later.await.unwrap();
        assert!(ran.load(Ordering::Relaxed));
        let _ = pool.close(Duration::from_secs(1)).await;
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
            clamped_deadline(base, Duration::from_mins(10), limit),
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

        let handler_outcome = drive(
            &ForcedCleanup::default(),
            Box::pin(async move {
                let _ = ran.send(());
                Ok(Completion::Pending)
            }),
            cancel.clone(),
            deadline,
            deadline + Duration::from_secs(5),
        )
        .await;

        std::assert_matches!(handler_outcome, Some(HandlerOutcome::Timeout));
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

        std::assert_matches!(attempt.await, Some(HandlerOutcome::Timeout));
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
                Ok(Completion::Pending)
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

    #[tokio::test]
    async fn returning_or_unwinding_cancels_the_handler_children() {
        for exit in ["success", "failure", "panic"] {
            let token = CancellationToken::new();
            let child = token.child_token();
            let until = Instant::now() + Duration::from_secs(10);
            let result = drive(
                &ForcedCleanup::default(),
                Box::pin(async move {
                    match exit {
                        "success" => Ok(Completion::Pending),
                        "failure" => Err(JobError::permanent("refused")),
                        _ => panic!("handler panicked"),
                    }
                }),
                token,
                until,
                until,
            )
            .await;
            assert!(result.is_some());
            assert!(child.is_cancelled(), "{exit}");
        }
    }

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
            for handler_outcome in [
                HandlerOutcome::Success(Completion::Pending),
                HandlerOutcome::Error(JobError::retryable("fail")),
                HandlerOutcome::Error(JobError::permanent("stop")),
                HandlerOutcome::Cancelled,
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
                record_on_span(
                    &span,
                    &map_outcome(
                        &attempt,
                        Policy::default(),
                        crate::kind::RetryBudget::Exhaust,
                        handler_outcome,
                    ),
                );
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
