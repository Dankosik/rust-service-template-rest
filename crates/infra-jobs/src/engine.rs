use std::fmt;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use enum_map::EnumMap;
use sqlx::postgres::PgPool;
use tokio::sync::{Notify, Semaphore, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::claim;
use crate::listener;
use crate::maintenance;
use crate::session::StartupError;
use crate::{KindError, Kinds};

pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const LEASE_RESERVE: Duration = Duration::from_mins(1);
pub(crate) const CANCEL_MARGIN: Duration = Duration::from_secs(2);
pub(crate) const RECORD_RETRY_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const OPERATION_BACKSTOP: Duration = Duration::from_secs(12);
const OPERATION_FAILURES_METRIC: &str = "jobs_worker_operation_failures_total";

pub struct Engine {
    shared: Arc<Shared>,
    additional: Vec<Arc<Shared>>,
}

pub struct Started {
    lifecycle: Arc<AttemptLifecycle>,
    _background: tokio_util::sync::DropGuard,
    stop: CancellationToken,
    failure: CancellationToken,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainEnd {
    pub known_results: usize,
    pub cancelled: usize,
    pub released: usize,
    pub uncertain: usize,
    pub timed_out: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error("acquire")]
    Acquire(#[source] sqlx::Error),
    #[error("statement")]
    Statement(#[from] sqlx::Error),
    #[error("timed out")]
    TimedOut,
}

impl OperationError {
    #[must_use]
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            Self::Acquire(_) => "acquire",
            Self::Statement(_) => "statement",
            Self::TimedOut => "timed_out",
        }
    }
}

impl From<infra_postgres::TxError> for OperationError {
    fn from(error: infra_postgres::TxError) -> Self {
        match error {
            infra_postgres::TxError::Acquire(error) => Self::Acquire(error),
            infra_postgres::TxError::Begin(error)
            | infra_postgres::TxError::CommitFailed(error)
            | infra_postgres::TxError::CommitUnknown(error) => Self::Statement(error),
        }
    }
}

impl Engine {
    #[must_use]
    pub fn new(pool: PgPool, registry: Kinds, max_workers: NonZeroU32) -> Self {
        Self {
            shared: Arc::new(Shared::new(
                pool,
                registry,
                max_workers,
                uuid::Uuid::new_v4(),
                Arc::default(),
            )),
            additional: Vec::new(),
        }
    }

    pub fn add_group(&mut self, registry: Kinds, max_workers: NonZeroU32) -> Result<(), KindError> {
        if let Some(duplicate) = registry.names().find(|name| {
            self.groups()
                .any(|group| group.registry.get(name).is_some())
        }) {
            return Err(KindError::Duplicate(duplicate));
        }
        self.additional.push(Arc::new(Shared::new(
            self.shared.pool.clone(),
            registry,
            max_workers,
            self.shared.worker_id,
            Arc::clone(&self.shared.lifecycle),
        )));
        Ok(())
    }

    pub(crate) fn groups(&self) -> impl Iterator<Item = &Arc<Shared>> {
        std::iter::once(&self.shared).chain(&self.additional)
    }

    pub async fn check_startup(&self) -> Result<(), StartupError> {
        crate::session::check_startup(&self.shared.pool).await
    }

    pub async fn remove_expired(&self) -> Result<u64, OperationError> {
        maintenance::remove_expired(&self.shared).await
    }

    #[must_use]
    pub fn start(self, tracker: &TaskTracker, cancel: &CancellationToken) -> Started {
        crate::attempt::describe_metrics();
        for group in self.groups() {
            group.owned_attempts.bind();
            group.completions.memberships.bind();
            for registered in group.registry.iter() {
                registered.metrics();
            }
        }
        claim::describe_metrics();
        let kinds: Vec<_> = self
            .groups()
            .flat_map(|group| group.registry.names())
            .collect();
        maintenance::init_metrics(&kinds);
        metrics::describe_counter!(
            OPERATION_FAILURES_METRIC,
            metrics::Unit::Count,
            "Failed worker database operations"
        );
        let groups = 1 + self.additional.len();
        tracing::info!(worker.id = %self.shared.worker_id, groups, "jobs_engine_starting");
        let engine = Arc::new(self);
        let background = cancel.child_token();
        let stop = background.child_token();
        let failure = CancellationToken::new();
        let lifecycle = Arc::clone(&engine.shared.lifecycle);
        lifecycle.drain.start_claim_loops(groups);
        for group in engine.groups() {
            spawn_guarded(
                tracker,
                Arc::clone(group),
                Run::Claim {
                    _exit: DrainParticipant::ClaimLoop(Arc::clone(&lifecycle.drain)),
                },
                stop.clone(),
                failure.clone(),
            );
        }
        let background_tasks = [
            Run::Retention,
            Run::Listener(Arc::clone(&engine)),
            Run::Sampling(kinds),
        ];
        for run in background_tasks {
            spawn_guarded(
                tracker,
                Arc::clone(&engine.shared),
                run,
                background.clone(),
                failure.clone(),
            );
        }
        Started {
            lifecycle,
            _background: background.drop_guard(),
            stop,
            failure,
        }
    }
}

impl fmt::Debug for Engine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kinds = fmt::from_fn(|f| {
            f.debug_list()
                .entries(self.groups().flat_map(|group| group.registry.names()))
                .finish()
        });
        formatter
            .debug_struct("Engine")
            .field("kinds", &kinds)
            .field(
                "group_limits",
                &fmt::from_fn(|f| {
                    f.debug_list()
                        .entries(self.groups().map(|group| group.max_workers))
                        .finish()
                }),
            )
            .finish_non_exhaustive()
    }
}

impl Started {
    pub fn stop_claiming(&self) {
        self.stop.cancel();
    }

    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.lifecycle.drain.in_flight()
    }

    pub async fn drained(&self) {
        self.lifecycle.drain.wait().await;
    }

    #[must_use]
    pub async fn cancel_and_finish(&self, budget: Duration) -> DrainEnd {
        self.stop_claiming();
        let deadline = self.lifecycle.cleanup.begin(budget);
        tokio::select! {
            biased;
            () = self.drained() => {}
            () = tokio::time::sleep_until(deadline) => {}
        }
        let timed_out = *self
            .lifecycle
            .cleanup
            .timed_out
            .get_or_init(|| !self.lifecycle.drain.completed_by(deadline));
        let counters = &self.lifecycle.counters;
        DrainEnd {
            known_results: counters.known_results.load(Ordering::Relaxed),
            cancelled: counters.cancelled.load(Ordering::Relaxed),
            released: counters.released.load(Ordering::Relaxed),
            uncertain: counters.uncertain.load(Ordering::Relaxed),
            timed_out,
        }
    }

    pub async fn failed(&self) {
        self.failure.cancelled().await;
    }
}

impl fmt::Debug for Started {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Started")
            .field("in_flight", &self.in_flight())
            .finish_non_exhaustive()
    }
}

enum Run {
    Claim { _exit: DrainParticipant },
    Retention,
    Listener(Arc<Engine>),
    Sampling(Vec<&'static str>),
}

impl Run {
    const fn label(&self) -> &'static str {
        match self {
            Self::Claim { .. } => "claim",
            Self::Retention => "retention",
            Self::Listener(_) => "listener",
            Self::Sampling(_) => "sampling",
        }
    }
}

fn spawn_guarded(
    tracker: &TaskTracker,
    shared: Arc<Shared>,
    run: Run,
    token: CancellationToken,
    failure: CancellationToken,
) {
    let guard = task_guard(run.label(), token.clone(), failure.clone());
    tracker.spawn(async move {
        let _guard = guard;
        match run {
            _claim @ Run::Claim { .. } => claim::run_claim_loop(shared, token, failure).await,
            Run::Retention => maintenance::run_retention(shared, token).await,
            Run::Listener(engine) => listener::run_listener(&shared, &engine, token).await,
            Run::Sampling(kinds) => maintenance::run_sampling(shared, &kinds, token).await,
        }
    });
}

#[derive(Default)]
struct DrainState {
    in_flight: usize,
    claim_loops: usize,
    completed_at: Option<Instant>,
}

pub(crate) struct AttemptDrain(watch::Sender<DrainState>);

impl Default for AttemptDrain {
    fn default() -> Self {
        Self(watch::Sender::new(DrainState::default()))
    }
}

impl AttemptDrain {
    fn start_claim_loops(&self, count: usize) {
        self.0.send_modify(|state| state.claim_loops = count);
    }

    fn in_flight(&self) -> usize {
        self.0.borrow().in_flight
    }

    fn completed_by(&self, deadline: Instant) -> bool {
        self.0
            .borrow()
            .completed_at
            .is_some_and(|at| at <= deadline)
    }

    async fn wait(&self) {
        let mut receiver = self.0.subscribe();
        drop(
            receiver
                .wait_for(|state| state.completed_at.is_some())
                .await,
        );
    }

    fn admit(&self) {
        self.0.send_if_modified(|state| {
            state.in_flight += 1;
            false
        });
    }

    fn finish(&self, retire: impl FnOnce(&mut DrainState)) {
        self.0.send_if_modified(|state| {
            retire(state);
            if state.claim_loops == 0 && state.in_flight == 0 {
                state.completed_at = Some(Instant::now());
                true
            } else {
                false
            }
        });
    }
}

enum DrainParticipant {
    ClaimLoop(Arc<AttemptDrain>),
    Supervisor(Arc<AttemptDrain>),
}

impl Drop for DrainParticipant {
    fn drop(&mut self) {
        match self {
            Self::ClaimLoop(drain) => drain.finish(|state| state.claim_loops -= 1),
            Self::Supervisor(drain) => drain.finish(|state| state.in_flight -= 1),
        }
    }
}

pub(crate) struct SupervisorGuard {
    stop: CancellationToken,
    failure: CancellationToken,
    job_id: crate::kind::JobId,
    kind: &'static str,
    retired: bool,
    _exit: DrainParticipant,
}

impl SupervisorGuard {
    pub(crate) fn new(
        drain: Arc<AttemptDrain>,
        stop: CancellationToken,
        failure: CancellationToken,
        job_id: crate::kind::JobId,
        kind: &'static str,
    ) -> Self {
        drain.admit();
        Self {
            stop,
            failure,
            job_id,
            kind,
            retired: false,
            _exit: DrainParticipant::Supervisor(drain),
        }
    }

    pub(crate) fn retire(&mut self) {
        self.retired = true;
    }
}

impl Drop for SupervisorGuard {
    fn drop(&mut self) {
        if std::thread::panicking() || !self.retired {
            tracing::error!(
                task = "attempt",
                job.id = %self.job_id,
                job.kind = self.kind,
                panicked = std::thread::panicking(),
                "jobs_engine_task_stopped"
            );
            self.failure.cancel();
            self.stop.cancel();
        }
    }
}

fn task_guard(
    task: &'static str,
    token: CancellationToken,
    failure: CancellationToken,
) -> std::mem::DropGuard<(), impl FnOnce(())> {
    std::mem::DropGuard::new((), move |()| {
        if std::thread::panicking() || !token.is_cancelled() {
            tracing::error!(
                task = task,
                panicked = std::thread::panicking(),
                "jobs_engine_task_stopped"
            );
            failure.cancel();
        }
    })
}

const CLEANUP_OVERFLOW_FALLBACK: Duration = Duration::from_hours(24 * 365);

pub(crate) struct Shared {
    pub(crate) worker_id: uuid::Uuid,
    pub(crate) pool: PgPool,
    pub(crate) registry: Kinds,
    max_workers: NonZeroU32,
    pub(crate) slots: Arc<Semaphore>,
    pub(crate) statement_turn: tokio::sync::Mutex<()>,
    pub(crate) lifecycle: Arc<AttemptLifecycle>,
    failing: EnumMap<Operation, AtomicBool>,
    pub(crate) completions: crate::attempt::Completions,
    pub(crate) owned_attempts: crate::attempt::MembershipGauge,
    pub(crate) claim_wake: Arc<Notify>,
}

impl Shared {
    fn new(
        pool: PgPool,
        registry: Kinds,
        max_workers: NonZeroU32,
        worker_id: uuid::Uuid,
        lifecycle: Arc<AttemptLifecycle>,
    ) -> Self {
        Self {
            pool,
            worker_id,
            registry,
            max_workers,
            lifecycle,
            slots: Arc::new(claim::admission_slots(max_workers)),
            statement_turn: tokio::sync::Mutex::new(()),
            failing: EnumMap::default(),
            completions: crate::attempt::Completions::default(),
            owned_attempts: crate::attempt::MembershipGauge::owned_attempts(),
            claim_wake: Arc::default(),
        }
    }
}

#[derive(Default)]
pub(crate) struct AttemptLifecycle {
    pub(crate) cleanup: ForcedCleanup,
    pub(crate) counters: Counters,
    pub(crate) drain: Arc<AttemptDrain>,
}

#[derive(Default)]
pub(crate) struct ForcedCleanup {
    force: CancellationToken,
    window: OnceLock<CleanupWindow>,
    timed_out: OnceLock<bool>,
}

struct CleanupWindow {
    started_at: Instant,
    deadline: Instant,
}

impl ForcedCleanup {
    fn begin(&self, budget: Duration) -> Instant {
        let window = self.window.get_or_init(|| {
            let started_at = Instant::now();
            let deadline = started_at
                .checked_add(budget)
                .unwrap_or(started_at + CLEANUP_OVERFLOW_FALLBACK);
            CleanupWindow {
                started_at,
                deadline,
            }
        });
        self.force.cancel();
        window.deadline
    }

    pub(crate) async fn forced(&self) {
        self.force.cancelled().await;
    }

    pub(crate) fn requested_before(&self, deadline: Instant) -> bool {
        self.window
            .get()
            .is_some_and(|window| window.started_at < deadline)
    }

    pub(crate) async fn ended(&self) {
        self.force.cancelled().await;
        match self.window.get() {
            Some(window) => tokio::time::sleep_until(window.deadline).await,
            None => std::future::pending().await,
        }
    }

    pub(crate) fn has_ended(&self) -> bool {
        self.window
            .get()
            .is_some_and(|window| Instant::now() >= window.deadline)
    }
}

#[derive(Default)]
pub(crate) struct Counters {
    pub(crate) known_results: AtomicUsize,
    pub(crate) cancelled: AtomicUsize,
    pub(crate) released: AtomicUsize,
    pub(crate) uncertain: AtomicUsize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, enum_map::Enum)]
pub(crate) enum Operation {
    Claim,
    Record,
    Release,
    Retention,
    Sample,
    Listen,
}

impl Operation {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Record => "record",
            Self::Release => "release",
            Self::Retention => "retention",
            Self::Sample => "sample",
            Self::Listen => "listen",
        }
    }
}

pub(crate) fn observe_failure(shared: &Shared, operation: Operation, error: &OperationError) {
    metrics::counter!(OPERATION_FAILURES_METRIC, "operation" => operation.as_str()).increment(1);
    if !shared.failing[operation].swap(true, Ordering::SeqCst) {
        log_operation_failure(operation, error);
    }
}

fn log_operation_failure(operation: Operation, error: &OperationError) {
    let (sqlstate, cause) = match error {
        OperationError::TimedOut => (None, Some("timeout")),
        OperationError::Acquire(err) | OperationError::Statement(err) => {
            match infra_postgres::sqlstate(err) {
                Some(code) => (Some(code), None),
                None => (None, Some(infra_postgres::failure_cause(err))),
            }
        }
    };
    let decode_reason = match error {
        OperationError::Statement(error) => claim::decode_reason(error),
        _ => None,
    };
    tracing::warn!(
        operation = operation.as_str(),
        failure = error.as_str(),
        sqlstate = sqlstate.as_deref(),
        cause,
        decode_reason,
        "jobs_operation_failed"
    );
}

pub(crate) fn observe_recovery(shared: &Shared, operation: Operation) {
    if shared.failing[operation].swap(false, Ordering::SeqCst) {
        tracing::info!(operation = operation.as_str(), "jobs_operation_recovered");
    }
}

pub(crate) async fn backstop<T>(
    operation: impl Future<Output = Result<T, OperationError>>,
) -> Result<T, OperationError> {
    tokio::time::timeout(OPERATION_BACKSTOP, operation)
        .await
        .map_err(|_| OperationError::TimedOut)?
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::gauge_recorder::Gauges;

    #[derive(Deserialize, Serialize)]
    struct Ordinary;

    impl crate::JobKind for Ordinary {
        const NAME: &'static str = "ordinary";
    }

    #[derive(Deserialize, Serialize)]
    struct Publisher;

    impl crate::JobKind for Publisher {
        const NAME: &'static str = "publisher";
    }

    #[tokio::test]
    async fn engine_debug_shows_registered_kinds_and_worker_limit() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let mut kinds = crate::KindsBuilder::new();
        kinds.register(crate::Policy::default(), |_: crate::Job<Publisher>| async {
            Ok(())
        });
        kinds.register(crate::Policy::default(), |_: crate::Job<Ordinary>| async {
            Ok(())
        });
        let engine = Engine::new(pool, kinds.validate().unwrap(), NonZeroU32::MIN);
        for output in [format!("{engine:?}"), format!("{engine:#?}")] {
            let (_, remaining) = output
                .split_once("\"publisher\"")
                .expect("first registered kind");
            assert!(remaining.contains("\"ordinary\""), "{output}");
            assert!(output.contains("group_limits"), "{output}");
            assert!(output.contains(".."), "{output}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_suspended_attempt_keeps_the_first_stop_reason() {
        for (force_before_timeout, expected) in [(true, (0, 1)), (false, (1, 0))] {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://localhost/unused")
                .unwrap();
            pool.close().await;
            let mut kinds = crate::KindsBuilder::new();
            kinds.register(
                crate::Policy {
                    timeout: Duration::from_secs(1),
                    max_attempts: 1,
                    ..crate::Policy::default()
                },
                |job: crate::Job<Ordinary>| async move {
                    job.cancellation().cancelled().await;
                    Err(crate::JobError::retryable("handler stopped"))
                },
            );
            let engine = Engine::new(pool, kinds.validate().unwrap(), NonZeroU32::MIN);
            let shared = &engine.shared;
            let job_id = crate::JobId(uuid::Uuid::nil());
            let guard = SupervisorGuard::new(
                Arc::clone(&shared.lifecycle.drain),
                CancellationToken::new(),
                CancellationToken::new(),
                job_id,
                "ordinary",
            );
            let claimed = claim::Claimed {
                registered: Arc::clone(shared.registry.get("ordinary").unwrap()),
                attempt: crate::attempt::AttemptId {
                    job_id,
                    kind: "ordinary",
                    number: 1,
                },
                generation: 1,
                payload: Ok(b"null".to_vec()),
                trace: crate::trace_context::Carrier::default(),
                slot: Arc::clone(&shared.slots).try_acquire_owned().unwrap(),
                kind_slot: None,
            };
            let mut supervisor = std::pin::pin!(crate::attempt::supervise(
                Arc::clone(shared),
                claimed,
                Instant::now() + Duration::from_secs(8),
                guard,
            ));
            assert!(futures_util::poll!(supervisor.as_mut()).is_pending());
            if !force_before_timeout {
                tokio::time::advance(Duration::from_secs(2)).await;
            }
            let cleanup_deadline = shared.lifecycle.cleanup.begin(Duration::from_secs(4));
            if force_before_timeout {
                tokio::time::advance(Duration::from_secs(2)).await;
            }
            assert!(futures_util::poll!(supervisor.as_mut()).is_pending());
            assert_eq!(
                (
                    shared
                        .lifecycle
                        .counters
                        .known_results
                        .load(Ordering::Relaxed),
                    shared.lifecycle.counters.cancelled.load(Ordering::Relaxed),
                ),
                expected,
            );
            tokio::time::advance(cleanup_deadline.saturating_duration_since(Instant::now())).await;
            supervisor.await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_reports_when_the_last_supervisor_exited_even_if_the_waiter_resumes_later() {
        let budget = Duration::from_secs(2);
        for (exit_after, observe_timeout_first, timed_out) in
            [(1, false, false), (2, true, true), (3, false, true)]
        {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://localhost/unused")
                .unwrap();
            let mut kinds = crate::KindsBuilder::new();
            kinds.register(crate::Policy::default(), |_: crate::Job<Ordinary>| async {
                Ok(())
            });
            let engine = Engine::new(pool, kinds.validate().unwrap(), NonZeroU32::MIN);
            engine.shared.lifecycle.drain.start_claim_loops(1);
            let claim_loop =
                DrainParticipant::ClaimLoop(Arc::clone(&engine.shared.lifecycle.drain));
            let started = Started {
                lifecycle: Arc::clone(&engine.shared.lifecycle),
                _background: CancellationToken::new().drop_guard(),
                stop: CancellationToken::new(),
                failure: CancellationToken::new(),
            };
            let mut supervisor = SupervisorGuard::new(
                Arc::clone(&started.lifecycle.drain),
                started.stop.clone(),
                started.failure.clone(),
                crate::JobId(uuid::Uuid::nil()),
                "ordinary",
            );
            supervisor.retire();
            drop(claim_loop);
            let deadline = Instant::now() + budget;
            let mut finish = std::pin::pin!(started.cancel_and_finish(budget));
            assert!(futures_util::poll!(finish.as_mut()).is_pending());
            tokio::time::advance(Duration::from_secs(exit_after)).await;
            let end = if observe_timeout_first {
                let end = finish.await;
                assert_eq!(Instant::now(), deadline);
                drop(supervisor);
                end
            } else {
                drop(supervisor);
                tokio::time::advance(Duration::from_secs(2)).await;
                finish.await
            };

            assert_eq!(end.timed_out, timed_out);
            assert_eq!(
                started.cancel_and_finish(Duration::ZERO).await.timed_out,
                timed_out
            );
        }
    }

    #[tokio::test]
    async fn two_groups_cannot_claim_the_same_kind() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let kinds = || {
            let mut registry = crate::KindsBuilder::new();
            registry.register(crate::Policy::default(), |_: crate::Job<Ordinary>| async {
                Ok(())
            });
            registry.validate().unwrap()
        };
        let mut engine = Engine::new(pool, kinds(), NonZeroU32::MIN);
        assert_eq!(
            engine.add_group(kinds(), NonZeroU32::MIN),
            Err(KindError::Duplicate("ordinary"))
        );
    }

    #[test]
    fn engine_drains_after_runtime_drops_an_unpolled_claim_task() {
        let started = {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _entered = runtime.enter();
            let pool = sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://localhost/unused")
                .unwrap();
            let mut kinds = crate::KindsBuilder::new();
            kinds.register(crate::Policy::default(), |_: crate::Job<Ordinary>| async {
                Ok(())
            });
            let engine = Engine::new(pool, kinds.validate().unwrap(), NonZeroU32::MIN);
            engine.start(&TaskTracker::new(), &CancellationToken::new())
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(1), started.drained())
                .await
                .expect("the stopped engine must drain even before its claim task is polled");
        });
    }

    #[tokio::test]
    async fn engine_panic_after_cancellation_still_reports_failure() {
        let token = CancellationToken::new();
        token.cancel();
        let failure = CancellationToken::new();
        let guard = task_guard("fixture", token.clone(), failure.clone());
        let task = tokio::spawn(async move {
            let _guard = guard;
            panic!("engine cleanup defect");
        });
        assert!(task.await.unwrap_err().is_panic());
        assert!(failure.is_cancelled());

        let normal_failure = CancellationToken::new();
        drop(task_guard("normal", token, normal_failure.clone()));
        assert!(!normal_failure.is_cancelled());
    }

    struct PendingDrop {
        entered: Arc<Notify>,
        drops: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Future for PendingDrop {
        type Output = Result<(), crate::JobError>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            self.entered.notify_one();
            std::task::Poll::Pending
        }
    }

    impl Drop for PendingDrop {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
            panic!("pending-handler-secret");
        }
    }

    #[tokio::test]
    async fn supervisor_custody_survives_unpolled_drop_and_abort() {
        let gauges = Gauges::default();
        let _local = metrics::set_default_local_recorder(gauges.recorder());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let entered = Arc::new(Notify::new());
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut kinds = crate::KindsBuilder::new();
        let handler_entered = Arc::clone(&entered);
        let handler_drops = Arc::clone(&drops);
        kinds.register(crate::Policy::default(), move |_: crate::Job<Ordinary>| {
            PendingDrop {
                entered: Arc::clone(&handler_entered),
                drops: Arc::clone(&handler_drops),
            }
        });
        let engine = Engine::new(pool.clone(), kinds.validate().unwrap(), NonZeroU32::MIN);
        let tracker = TaskTracker::new();
        let registered = engine.shared.registry.get("ordinary").unwrap();
        for case in ["unpolled", "abort", "expired"] {
            let stop = CancellationToken::new();
            let failure = CancellationToken::new();
            let future = crate::attempt::supervise(
                Arc::clone(&engine.shared),
                crate::claim::Claimed {
                    registered: Arc::clone(registered),
                    attempt: crate::attempt::AttemptId {
                        job_id: crate::JobId(uuid::Uuid::nil()),
                        kind: "ordinary",
                        number: 1,
                    },
                    generation: 1,
                    payload: Ok(b"null".to_vec()),
                    trace: crate::trace_context::Carrier::default(),
                    slot: Arc::clone(&engine.shared.slots)
                        .try_acquire_owned()
                        .unwrap(),
                    kind_slot: None,
                },
                Instant::now()
                    + if case == "expired" {
                        Duration::ZERO
                    } else {
                        Duration::from_secs(10)
                    },
                SupervisorGuard::new(
                    Arc::clone(&engine.shared.lifecycle.drain),
                    stop.clone(),
                    failure.clone(),
                    crate::JobId(uuid::Uuid::nil()),
                    "ordinary",
                ),
            );
            assert_eq!(gauges.get("jobs_owned_attempts", None), 1.0, "{case}");
            if case == "unpolled" {
                drop(future);
            } else {
                let task = tracker.spawn(future);
                if case == "abort" {
                    tokio::time::timeout(Duration::from_secs(1), entered.notified())
                        .await
                        .unwrap();
                    task.abort();
                    assert!(task.await.unwrap_err().is_cancelled());
                    assert_eq!(drops.load(Ordering::SeqCst), 1);
                } else {
                    tokio::time::timeout(Duration::from_secs(1), task)
                        .await
                        .unwrap()
                        .unwrap();
                }
            }
            assert_eq!(failure.is_cancelled(), case != "expired", "{case}");
            assert_eq!(stop.is_cancelled(), case != "expired", "{case}");
            assert_eq!(engine.shared.slots.available_permits(), 1, "{case}");
            assert_eq!(gauges.get("jobs_owned_attempts", None), 0.0, "{case}");
        }
        assert_eq!(
            engine
                .shared
                .lifecycle
                .counters
                .uncertain
                .load(Ordering::Relaxed),
            1,
            "only explicit expiry reports database uncertainty"
        );
        tracker.close();
        tokio::time::timeout(Duration::from_secs(1), tracker.wait())
            .await
            .unwrap();
        pool.close().await;
    }

    #[test]
    fn supervisor_panic_after_retirement_still_stops_its_start() {
        let stop = CancellationToken::new();
        let failure = CancellationToken::new();
        let guard = SupervisorGuard::new(
            Arc::default(),
            stop.clone(),
            failure.clone(),
            crate::JobId(uuid::Uuid::nil()),
            "ordinary",
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let mut guard = guard;
            guard.retire();
            panic!("retirement defect");
        }));
        assert!(result.is_err());
        assert!(stop.is_cancelled());
        assert!(failure.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn backstop_bounds_an_unacknowledged_operation() {
        std::assert_matches!(
            backstop(std::future::pending::<Result<(), OperationError>>()).await,
            Err(OperationError::TimedOut)
        );
    }
}
