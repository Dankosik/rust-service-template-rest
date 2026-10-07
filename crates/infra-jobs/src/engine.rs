//! The job engine: shared state and bounded supervisor cleanup.

use std::fmt;
use std::future::Future;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use sqlx::postgres::PgPool;
use tokio::sync::{Notify, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::Registry;
use crate::claim;
use crate::maintenance;

/// How often an idle worker claims.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Recovery reserve added to each kind's attempt timeout.
pub const LEASE_RESERVE: Duration = Duration::from_secs(60);
/// How far the worker's own cancellation leads the database expiry.
pub(crate) const CANCEL_MARGIN: Duration = Duration::from_secs(2);
/// How long an outcome write waits before it is sent again.
pub(crate) const RECORD_RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// Client bound around one engine statement: acquire and statement acknowledgement.
pub(crate) const OPERATION_BACKSTOP: Duration = Duration::from_secs(12);
/// Counter of failed engine statements. Label `operation`.
pub(crate) const OPERATION_FAILURES_METRIC: &str = "jobs_worker_operation_failures_total";

/// A worker's job engine: one kind set, its attempt slots, and its claim loop.
/// Cloning shares the pool, registry, and supervisor tracker.
#[derive(Clone)]
pub struct Engine {
    shared: Arc<Shared>,
}

/// A running engine.
pub struct Started {
    shared: Arc<Shared>,
    stop: CancellationToken,
    failure: CancellationToken,
}

/// Cumulative local observations when forced cleanup ended.
/// These counters do not attribute zero-row queue writes to this worker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainEnd {
    /// Handlers whose result (including panic/timeout) became known.
    pub known_results: usize,
    /// Handlers joined with cancellation, eligible for a fenced release.
    pub cancelled: usize,
    /// Acknowledged one-row release writes.
    pub released: usize,
    /// Attempts whose cleanup or transaction outcome stayed uncertain.
    pub uncertain: usize,
    /// The deadline passed before every supervisor finished.
    pub timed_out: bool,
}

/// Why the jobs store cannot start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartupError {
    /// PostgreSQL does not use UTF8 server encoding.
    #[error("the jobs store requires UTF8 server encoding")]
    UnsupportedEncoding,
    /// The session is read-only or recovering.
    #[error("the PostgreSQL session is not writable")]
    NotWritable,
    /// The worker pool does not default to READ COMMITTED.
    #[error("the jobs store requires READ COMMITTED session isolation")]
    UnsupportedIsolation,
    /// Anything else, including the check's bound.
    #[error("the jobs store is unavailable")]
    Unavailable,
}

/// Why one engine statement did not finish with an acknowledgement.
#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    /// The pool did not hand out a connection.
    #[error("acquire")]
    Acquire(#[source] sqlx::Error),
    /// The statement failed, or a returned row did not decode.
    #[error("statement")]
    Statement(#[source] sqlx::Error),
    /// The client bound fired before the statement returned.
    #[error("timed out")]
    TimedOut,
}

impl OperationError {
    /// The `failure` field of the operation records.
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

impl From<sqlx::Error> for OperationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Statement(error)
    }
}

impl Engine {
    /// Build an engine over a writable UTF8 pool with READ COMMITTED defaults.
    /// Call [`Self::check_startup`] before starting it. Does no I/O.
    ///
    /// This engine owns what a worker process needs once: the `LISTEN`
    /// connection, completed retention, and registered-kind sampling. Engines
    /// made with [`Self::beside`] share them.
    #[must_use]
    pub fn new(pool: PgPool, registry: Registry, max_workers: NonZeroU32) -> Self {
        Self::build(
            pool,
            registry,
            max_workers,
            uuid::Uuid::new_v4(),
            Arc::default(),
            true,
        )
    }

    /// A second engine of the same worker process on the same pool, with its
    /// own kinds, slots, and claim loop, so its work is not delayed by this
    /// engine's occupied slots.
    ///
    /// It claims under this engine's worker id, and this engine's listener,
    /// retention, and sampling serve it: start both.
    #[must_use]
    pub fn beside(&self, registry: Registry, max_workers: NonZeroU32) -> Self {
        Self::build(
            self.shared.pool.clone(),
            registry,
            max_workers,
            self.shared.worker_id,
            Arc::clone(&self.shared.peers),
            false,
        )
    }

    fn build(
        pool: PgPool,
        registry: Registry,
        max_workers: NonZeroU32,
        worker_id: uuid::Uuid,
        peers: Arc<Mutex<Vec<Peer>>>,
        owns_process_duties: bool,
    ) -> Self {
        let slots = usize::try_from(max_workers.get()).unwrap_or(usize::MAX);
        let wake = Arc::new(Notify::new());
        {
            let mut peers = peers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = registered_kinds(&peers);
            let kinds: Vec<_> = registry.names().collect();
            for &kind in &kinds {
                if previous.binary_search(&kind).is_err() {
                    maintenance::init_kind_metrics(kind);
                }
            }
            peers.push(Peer {
                kinds,
                wake: Arc::clone(&wake),
            });
            maintenance::invalidate_sample();
        }
        Self {
            shared: Arc::new(Shared {
                pool,
                worker_id,
                registry,
                max_workers,
                slots: Arc::new(Semaphore::new(slots)),
                permit: Semaphore::new(1),
                force: CancellationToken::new(),
                cleanup_deadline: OnceLock::new(),
                counters: Counters::default(),
                attempt_tracker: TaskTracker::new(),
                failing: Failing::new(),
                completions: crate::attempt::Completions::default(),
                wake,
                peers,
                owns_process_duties,
            }),
        }
    }

    /// The registered kind names, in registration order.
    pub fn kinds(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.shared.registry.names()
    }

    /// Check UTF8, READ COMMITTED defaults and a writable session, bounded to 5 s.
    ///
    /// # Errors
    ///
    /// [`StartupError::NotWritable`] when the session is read-only or recovering,
    /// [`StartupError::UnsupportedEncoding`] or [`StartupError::UnsupportedIsolation`]
    /// for incompatible session defaults, and [`StartupError::Unavailable`] for
    /// anything else, including the bound.
    pub async fn check_startup(&self) -> Result<(), StartupError> {
        maintenance::check_startup(&self.shared.pool).await
    }

    /// Delete expired completed jobs once and return how many were deleted.
    ///
    /// An admission-budget yield or a short batch ends the pass without proving
    /// empty inventory: locked eligible rows can remain.
    ///
    /// # Errors
    ///
    /// [`OperationError`] from the batch that failed. Earlier batches stay committed.
    pub async fn remove_expired(&self) -> Result<u64, OperationError> {
        maintenance::remove_expired(&self.shared).await
    }

    /// Spawn the claim loop on `tracker`, and for an engine built with
    /// [`Self::new`] also the listener, retention, and process-wide sampling.
    ///
    /// Each task runs under a child of `cancel`. Does no I/O before it returns.
    #[must_use]
    pub fn start(&self, tracker: &TaskTracker, cancel: &CancellationToken) -> Started {
        crate::attempt::describe_metrics();
        for registered in self.shared.registry.iter() {
            let _ = registered
                .metrics
                .set(crate::attempt::KindMetrics::new(registered.name));
        }
        claim::describe_metrics();
        if self.shared.owns_process_duties {
            let peers = self
                .shared
                .peers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            maintenance::init_metrics(&registered_kinds(&peers));
        }
        metrics::describe_counter!(
            OPERATION_FAILURES_METRIC,
            "Failed worker database operations"
        );
        tracing::info!(worker.id = %self.shared.worker_id, "jobs_engine_starting");
        let stop = cancel.child_token();
        let failure = CancellationToken::new();
        let spawn = |task, token, run| {
            spawn_guarded(
                tracker,
                Arc::clone(&self.shared),
                Guard {
                    task,
                    token,
                    failure: failure.clone(),
                },
                run,
            );
        };
        spawn("claim", stop.clone(), Run::Claim);
        if self.shared.owns_process_duties {
            spawn("retention", cancel.child_token(), Run::Retention);
            spawn("listener", cancel.child_token(), Run::Listener);
            spawn("sampling", cancel.child_token(), Run::Sampling);
        }
        Started {
            shared: Arc::clone(&self.shared),
            stop,
            failure,
        }
    }
}

impl fmt::Debug for Engine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Engine")
            .field("kinds", &self.shared.registry.names().collect::<Vec<_>>())
            .field("max_workers", &self.shared.max_workers)
            .finish_non_exhaustive()
    }
}

impl Started {
    /// Stop new claim rounds; an already dispatched claim retains custody.
    pub fn stop_claiming(&self) {
        self.stop.cancel();
    }

    /// Attempts admitted and not yet recorded.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.shared.attempt_tracker.len()
    }

    /// Resolves once the claim loop has exited and every supervisor has ended.
    pub async fn drained(&self) {
        self.shared.attempt_tracker.wait().await;
    }

    /// Cancel unfinished handlers and finish known outcomes inside `budget`.
    /// Supervisors retain ownership; this method never writes queue rows.
    ///
    /// The first call fixes the cleanup deadline. It holds even if this future
    /// is dropped, and a later call waits for the same deadline.
    #[must_use]
    pub async fn cancel_and_finish(&self, budget: Duration) -> DrainEnd {
        self.stop_claiming();
        // Fixed before `force` fires: a supervisor that sees the cancellation
        // also sees the deadline.
        self.shared.cleanup_deadline.get_or_init(|| {
            let now = Instant::now();
            now.checked_add(budget).unwrap_or(now + NO_CLEANUP_DEADLINE)
        });
        self.shared.force.cancel();
        let timed_out = tokio::select! {
            biased;
            () = self.drained() => false,
            () = self.shared.cleanup_ended() => true,
        };
        let counters = &self.shared.counters;
        DrainEnd {
            known_results: counters.known_results.load(Ordering::Relaxed),
            cancelled: counters.cancelled.load(Ordering::Relaxed),
            released: counters.released.load(Ordering::Relaxed),
            uncertain: counters.uncertain.load(Ordering::Relaxed),
            timed_out,
        }
    }

    /// Resolves if an attempt supervisor retires unexpectedly, or the claim
    /// loop, retention, listener, or sampling
    /// task this engine started ends before cancellation, or panics even while
    /// cancellation is in progress. The engine logs which one as `jobs_engine_task_stopped`.
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

/// The engine's own tasks. Each runs until its token is cancelled.
#[derive(Clone, Copy)]
enum Run {
    Claim,
    Retention,
    Listener,
    Sampling,
}

fn spawn_guarded(tracker: &TaskTracker, shared: Arc<Shared>, guard: Guard, run: Run) {
    let token = guard.token.clone();
    let failure = guard.failure.clone();
    tracker.spawn(async move {
        let _guard = guard;
        match run {
            Run::Claim => claim::run_claim_loop(shared, token, failure).await,
            Run::Retention => maintenance::run_retention(shared, token).await,
            Run::Listener => claim::run_listener(shared, token).await,
            Run::Sampling => maintenance::run_sampling(shared, token).await,
        }
    });
}

/// Armed before submission, including when the supervisor is never polled.
pub(crate) struct SupervisorGuard {
    stop: CancellationToken,
    failure: CancellationToken,
    retired: bool,
}

impl SupervisorGuard {
    pub(crate) fn new(stop: CancellationToken, failure: CancellationToken) -> Self {
        Self {
            stop,
            failure,
            retired: false,
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
                panicked = std::thread::panicking(),
                "jobs_engine_task_stopped"
            );
            self.failure.cancel();
            self.stop.cancel();
        }
    }
}

/// Dropped when its task ends, a panic included: an end before the token
/// was cancelled, or any panic during shutdown, fails the engine.
struct Guard {
    task: &'static str,
    token: CancellationToken,
    failure: CancellationToken,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if std::thread::panicking() || !self.token.is_cancelled() {
            tracing::error!(
                task = self.task,
                panicked = std::thread::panicking(),
                "jobs_engine_task_stopped"
            );
            self.failure.cancel();
        }
    }
}

/// Stands in for a cleanup budget too long to add to the clock.
const NO_CLEANUP_DEADLINE: Duration = Duration::from_hours(24 * 365);

/// One engine as the process's listener and sampler see it.
pub(crate) struct Peer {
    kinds: Vec<&'static str>,
    wake: Arc<Notify>,
}

fn registered_kinds(peers: &[Peer]) -> Vec<&'static str> {
    let mut kinds: Vec<_> = peers
        .iter()
        .flat_map(|peer| peer.kinds.iter().copied())
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

pub(crate) struct Shared {
    pub(crate) worker_id: uuid::Uuid,
    pub(crate) pool: PgPool,
    pub(crate) registry: Registry,
    pub(crate) max_workers: NonZeroU32,
    pub(crate) slots: Arc<Semaphore>,
    pub(crate) permit: Semaphore,
    /// Cancels running handlers.
    pub(crate) force: CancellationToken,
    /// When supervisors stop waiting, set by the first forced cleanup.
    cleanup_deadline: OnceLock<Instant>,
    pub(crate) counters: Counters,
    pub(crate) attempt_tracker: TaskTracker,
    pub(crate) failing: Failing,
    pub(crate) completions: crate::attempt::Completions,
    /// Set when a due job of a registered kind was committed.
    pub(crate) wake: Arc<Notify>,
    /// Every engine of this worker process, this one included.
    peers: Arc<Mutex<Vec<Peer>>>,
    /// Whether this engine runs the listener, retention, and sampler.
    owns_process_duties: bool,
}

impl Shared {
    /// Snapshot the registered union without holding the peer lock across I/O.
    pub(crate) fn registered_kinds(&self) -> Vec<&'static str> {
        let peers = self
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registered_kinds(&peers)
    }

    /// Publication and peer admission share the lock so stale membership cannot
    /// restore freshness after a newly registered kind invalidated it.
    pub(crate) fn publish_for_kinds(&self, kinds: &[&str], publish: impl FnOnce()) -> bool {
        let peers = self
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if registered_kinds(&peers) != kinds {
            return false;
        }
        publish();
        true
    }

    /// Resolves once the budget of a forced cleanup is spent; pending until
    /// a cleanup is forced.
    pub(crate) async fn cleanup_ended(&self) {
        self.force.cancelled().await;
        match self.cleanup_deadline.get() {
            Some(deadline) => tokio::time::sleep_until(*deadline).await,
            None => std::future::pending().await,
        }
    }

    pub(crate) fn cleanup_has_ended(&self) -> bool {
        self.cleanup_deadline
            .get()
            .is_some_and(|deadline| Instant::now() >= *deadline)
    }

    /// Wake every engine of this process that registers `kind`, or all of
    /// them when `kind` is `None`.
    pub(crate) fn wake_peers(&self, kind: Option<&str>) {
        let peers = self
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for peer in peers.iter() {
            if kind.is_none_or(|kind| peer.kinds.contains(&kind)) {
                peer.wake.notify_one();
            }
        }
    }
}

impl fmt::Debug for Shared {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Shared")
            .field("max_workers", &self.max_workers)
            .field("in_flight", &self.attempt_tracker.len())
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
pub(crate) struct Counters {
    pub(crate) known_results: AtomicUsize,
    pub(crate) cancelled: AtomicUsize,
    pub(crate) released: AtomicUsize,
    pub(crate) uncertain: AtomicUsize,
}

/// One engine statement. The label is [`Operation::as_str`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// One flag per [`Operation`]: set after the first failure, clear after recovery.
pub(crate) struct Failing {
    claim: AtomicBool,
    record: AtomicBool,
    release: AtomicBool,
    retention: AtomicBool,
    sample: AtomicBool,
    listen: AtomicBool,
}

impl Failing {
    const fn new() -> Self {
        Self {
            claim: AtomicBool::new(false),
            record: AtomicBool::new(false),
            release: AtomicBool::new(false),
            retention: AtomicBool::new(false),
            sample: AtomicBool::new(false),
            listen: AtomicBool::new(false),
        }
    }

    fn flag(&self, operation: Operation) -> &AtomicBool {
        match operation {
            Operation::Claim => &self.claim,
            Operation::Record => &self.record,
            Operation::Release => &self.release,
            Operation::Retention => &self.retention,
            Operation::Sample => &self.sample,
            Operation::Listen => &self.listen,
        }
    }
}

impl fmt::Debug for Failing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Failing").finish_non_exhaustive()
    }
}

pub(crate) fn observe_failure(shared: &Shared, operation: Operation, error: &OperationError) {
    metrics::counter!(OPERATION_FAILURES_METRIC, "operation" => operation.as_str()).increment(1);
    if !shared.failing.flag(operation).swap(true, Ordering::SeqCst) {
        log_operation_failure(operation, error);
    }
}

fn log_operation_failure(operation: Operation, error: &OperationError) {
    match error {
        OperationError::TimedOut => {
            tracing::warn!(
                operation = operation.as_str(),
                failure = error.as_str(),
                "jobs_operation_failed"
            );
        }
        OperationError::Acquire(err) | OperationError::Statement(err) => {
            if let Some(code) = infra_postgres::sqlstate(err) {
                tracing::warn!(
                    operation = operation.as_str(),
                    failure = error.as_str(),
                    sqlstate = code.as_ref(),
                    "jobs_operation_failed"
                );
            } else {
                tracing::warn!(
                    operation = operation.as_str(),
                    failure = error.as_str(),
                    cause = infra_postgres::failure_cause(err),
                    "jobs_operation_failed"
                );
            }
        }
    }
}

pub(crate) fn observe_recovery(shared: &Shared, operation: Operation) {
    if shared.failing.flag(operation).swap(false, Ordering::SeqCst) {
        tracing::info!(operation = operation.as_str(), "jobs_operation_recovered");
    }
}

/// Bound acquire and full statement acknowledgement by [`OPERATION_BACKSTOP`].
pub(crate) async fn backstop<T>(
    operation: impl Future<Output = Result<T, OperationError>>,
) -> Result<T, OperationError> {
    tokio::time::timeout(OPERATION_BACKSTOP, operation)
        .await
        .map_err(|_| OperationError::TimedOut)?
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::HashMap;

    use metrics::{
        Counter, Gauge, GaugeFn, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
    };
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Default)]
    struct RecordedGauge(Mutex<f64>);

    impl GaugeFn for RecordedGauge {
        fn increment(&self, value: f64) {
            *self.0.lock().unwrap() += value;
        }

        fn decrement(&self, value: f64) {
            *self.0.lock().unwrap() -= value;
        }

        fn set(&self, value: f64) {
            *self.0.lock().unwrap() = value;
        }
    }

    #[derive(Default)]
    pub(crate) struct Gauges(Mutex<HashMap<Key, Arc<RecordedGauge>>>);

    impl Gauges {
        pub(crate) fn get(&self, name: &'static str, kind: Option<&'static str>) -> f64 {
            let key = match kind {
                Some(kind) => Key::from_parts(name, &[("kind", kind)]),
                None => Key::from_name(name),
            };
            *self.0.lock().unwrap()[&key].0.lock().unwrap()
        }
    }

    impl Recorder for Gauges {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn register_counter(&self, _: &Key, _: &Metadata<'_>) -> Counter {
            Counter::noop()
        }

        fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
            Gauge::from_arc(Arc::clone(
                self.0.lock().unwrap().entry(key.clone()).or_default(),
            ))
        }

        fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
            Histogram::noop()
        }
    }

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
    #[allow(clippy::float_cmp, reason = "integer-valued fixture gauges are exact")]
    async fn late_peer_invalidates_freshness_and_rejects_an_in_flight_old_union() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let gauges = Gauges::default();
        metrics::with_local_recorder(&gauges, || {
            let mut kinds = crate::Kinds::new();
            kinds.register(crate::Policy::default(), |_: crate::Job<Ordinary>| async {
                Ok(())
            });
            let engine = Engine::new(pool.clone(), kinds.validate().unwrap(), NonZeroU32::MIN);
            let sampled_kinds = engine.shared.registered_kinds();
            assert!(engine.shared.publish_for_kinds(&sampled_kinds, || {
                metrics::gauge!(maintenance::FAILED_JOBS_METRIC, "kind" => "ordinary").set(3.0);
                metrics::gauge!(maintenance::OBSERVATION_TIMESTAMP_METRIC).set(100.0);
            }));

            let mut kinds = crate::Kinds::new();
            kinds.register(crate::Policy::default(), |_: crate::Job<Publisher>| async {
                Ok(())
            });
            let _publisher = engine.beside(kinds.validate().unwrap(), NonZeroU32::MIN);
            assert_eq!(
                gauges.get(maintenance::OBSERVATION_TIMESTAMP_METRIC, None),
                0.0
            );
            assert_eq!(
                gauges.get(maintenance::FAILED_JOBS_METRIC, Some("ordinary")),
                3.0
            );
            assert_eq!(
                gauges.get(maintenance::FAILED_JOBS_METRIC, Some("publisher")),
                0.0
            );
            assert!(!engine.shared.publish_for_kinds(&sampled_kinds, || {
                panic!("an observation from before peer admission must not publish");
            }));
            assert_eq!(
                gauges.get(maintenance::OBSERVATION_TIMESTAMP_METRIC, None),
                0.0
            );

            let current = engine.shared.registered_kinds();
            assert!(engine.shared.publish_for_kinds(&current, || {
                metrics::gauge!(maintenance::OBSERVATION_TIMESTAMP_METRIC).set(200.0);
            }));
            assert_eq!(
                gauges.get(maintenance::OBSERVATION_TIMESTAMP_METRIC, None),
                200.0
            );
        });
        pool.close().await;
    }

    #[tokio::test]
    async fn engine_panic_after_cancellation_still_reports_failure() {
        let token = CancellationToken::new();
        token.cancel();
        let failure = CancellationToken::new();
        let guard = Guard {
            task: "fixture",
            token: token.clone(),
            failure: failure.clone(),
        };
        let task = tokio::spawn(async move {
            let _guard = guard;
            panic!("engine cleanup defect");
        });
        assert!(task.await.unwrap_err().is_panic());
        assert!(failure.is_cancelled());

        let normal_failure = CancellationToken::new();
        drop(Guard {
            task: "normal",
            token,
            failure: normal_failure.clone(),
        });
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
    #[allow(clippy::float_cmp, reason = "integer ownership gauges are exact")]
    async fn supervisor_custody_survives_unpolled_drop_abort_and_missing_registration() {
        let gauges = Gauges::default();
        let _local = metrics::set_default_local_recorder(&gauges);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let entered = Arc::new(Notify::new());
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut kinds = crate::Kinds::new();
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
        for case in ["unpolled", "abort", "missing", "expired"] {
            let stop = CancellationToken::new();
            let failure = CancellationToken::new();
            let future = crate::attempt::supervise(
                Arc::clone(&engine.shared),
                crate::claim::Claimed {
                    id: crate::JobId(uuid::Uuid::nil()),
                    generation: 1,
                    kind: if case == "missing" {
                        "unregistered"
                    } else {
                        "ordinary"
                    },
                    attempt: 1,
                    payload: b"null".to_vec(),
                    trace_context: None,
                    trace_state: None,
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
                SupervisorGuard::new(stop.clone(), failure.clone()),
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
            engine.shared.counters.uncertain.load(Ordering::Relaxed),
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
        let guard = SupervisorGuard::new(stop.clone(), failure.clone());
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
        assert!(matches!(
            backstop(std::future::pending::<Result<(), OperationError>>()).await,
            Err(OperationError::TimedOut)
        ));
    }
}
