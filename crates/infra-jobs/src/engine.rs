//! The job engine: shared state and bounded supervisor cleanup.

use std::fmt;
use std::future::Future;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use enum_map::EnumMap;
use sqlx::postgres::PgPool;
use tokio::sync::{Notify, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::Kinds;
use crate::claim;
use crate::listener;
use crate::maintenance;
use crate::session::StartupError;

pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Recovery reserve added to each kind's attempt timeout.
///
/// Less the 2 s cancellation margin, it is the supervisor's window after the
/// handler's timeout for writing the outcome: one 12 s statement bound and its
/// 1 s retries fit in it.
pub const LEASE_RESERVE: Duration = Duration::from_secs(60);
/// How far the worker's own cancellation leads the database expiry, so the
/// supervisor stops writing before another worker can rescue the lease.
pub(crate) const CANCEL_MARGIN: Duration = Duration::from_secs(2);
pub(crate) const RECORD_RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// Client bound around one engine statement: acquire and statement acknowledgement.
pub(crate) const OPERATION_BACKSTOP: Duration = Duration::from_secs(12);
/// Counter of failed engine statements. Label `operation`.
const OPERATION_FAILURES_METRIC: &str = "jobs_worker_operation_failures_total";

/// A worker's job engine: one kind set, its attempt slots, and its claim loop.
///
/// Not `Clone`, and [`Self::start`] consumes it: a second claim loop on the
/// same state would let [`Started::drained`] resolve before its attempts end
/// and exceed each kind's `max_running`.
pub struct Engine {
    shared: Arc<Shared>,
    peer: PeerRegistration,
}

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
    /// The deadline passed before every supervisor finished and the claim loop exited.
    pub timed_out: bool,
}

/// Why one engine statement did not finish with an acknowledgement.
///
/// A failure to `BEGIN` a transaction, or to commit one, reports as
/// [`Self::Statement`] just like a failed statement, so a caller cannot tell
/// the three apart. An unknown commit does not prove the write absent: the
/// retention batch may have deleted its rows before the commit outcome was
/// lost.
#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error("acquire")]
    Acquire(#[source] sqlx::Error),
    /// The statement failed, or a returned row did not decode.
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

/// Preserves [`OperationError::Acquire`] and maps begin and commit failures to
/// [`OperationError::Statement`].
///
/// `Begin`, `CommitFailed`, and `CommitUnknown` keep the driver error as the
/// source and the `statement` label, with no variant, label, or metric
/// vocabulary of their own. A `Transaction(TxError)` variant that preserved
/// `is_commit_unknown` the way the operator's error does would be a public
/// change, and it would buy the engine nothing: its only writing transaction
/// is the retention batch's delete of expired completed rows, which is
/// idempotent. A retry after an unknown commit removes whatever rows are left,
/// the same state a retry after a failed statement reaches; only the next
/// pass's row count differs.
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
    /// Build an engine over a writable UTF8 pool with READ COMMITTED defaults.
    /// Call [`Self::check_startup`] before starting it. Does no I/O.
    ///
    /// This engine owns what a worker process needs once: the `LISTEN`
    /// connection, completed retention, and registered-kind sampling. Engines
    /// made with [`Self::beside`] share them.
    ///
    /// Construction registers this engine's kinds as a process peer and drops
    /// the published sampling result, so a sample that did not see them is
    /// never published as current. [`Self::start`] adds no peer.
    #[must_use]
    pub fn new(pool: PgPool, registry: Kinds, max_workers: NonZeroU32) -> Self {
        Self::build(pool, registry, max_workers, Role::Owner)
    }

    /// A second engine of the same worker process on the same pool, with its
    /// own kinds, slots, and claim loop, so its work is not delayed by this
    /// engine's occupied slots.
    ///
    /// It claims under this engine's worker id, and this engine's listener,
    /// retention, and sampling serve it: start both. Construction registers its
    /// kinds as a process peer beside this engine's and drops the published
    /// sampling result.
    #[must_use]
    pub fn beside(&self, registry: Kinds, max_workers: NonZeroU32) -> Self {
        Self::build(
            self.shared.pool.clone(),
            registry,
            max_workers,
            Role::Beside(&self.shared),
        )
    }

    fn build(pool: PgPool, registry: Kinds, max_workers: NonZeroU32, role: Role<'_>) -> Self {
        let (worker_id, peers, owns_process_duties) = match role {
            Role::Owner => (uuid::Uuid::new_v4(), Arc::default(), true),
            Role::Beside(owner) => (owner.worker_id, Arc::clone(&owner.peers), false),
        };
        let slots = usize::try_from(max_workers.get()).unwrap_or(usize::MAX);
        let wake = Arc::new(Notify::new());
        let peer = PeerRegistration::new(&registry, &peers, &wake);
        Self {
            peer,
            shared: Arc::new(Shared {
                pool,
                worker_id,
                registry,
                max_workers,
                slots: Arc::new(Semaphore::new(slots)),
                statement_turn: tokio::sync::Mutex::new(()),
                cleanup: ForcedCleanup::default(),
                counters: Counters::default(),
                attempt_tracker: TaskTracker::new(),
                failing: EnumMap::default(),
                completions: crate::attempt::Completions::default(),
                owned_attempts: crate::attempt::MembershipGauge::owned_attempts(),
                wake,
                peers,
                owns_process_duties,
            }),
        }
    }

    pub fn kinds(&self) -> impl Iterator<Item = &'static str> {
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
        crate::session::check_startup(&self.shared.pool).await
    }

    /// Delete expired completed jobs once and return how many were deleted.
    ///
    /// A short batch ends the pass without proving empty inventory: locked
    /// eligible rows can remain.
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
    pub fn start(self, tracker: &TaskTracker, cancel: &CancellationToken) -> Started {
        crate::attempt::describe_metrics();
        self.shared.owned_attempts.bind();
        self.shared.completions.memberships.bind();
        for registered in self.shared.registry.iter() {
            registered.metrics();
        }
        claim::describe_metrics();
        if self.shared.owns_process_duties {
            maintenance::init_metrics(&self.shared.sampling_kinds().0);
        }
        metrics::describe_counter!(
            OPERATION_FAILURES_METRIC,
            metrics::Unit::Count,
            "Failed worker database operations"
        );
        tracing::info!(worker.id = %self.shared.worker_id, "jobs_engine_starting");
        let stop = cancel.child_token();
        let failure = CancellationToken::new();
        let spawn = |run, token| {
            spawn_guarded(
                tracker,
                Arc::clone(&self.shared),
                run,
                token,
                failure.clone(),
            );
        };
        spawn(
            Run::Claim {
                _peer: self.peer,
                _close: CloseTracker(self.shared.attempt_tracker.clone()),
            },
            stop.clone(),
        );
        if self.shared.owns_process_duties {
            spawn(Run::Retention, cancel.child_token());
            spawn(Run::Listener, cancel.child_token());
            spawn(Run::Sampling, cancel.child_token());
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
        self.shared.cleanup.begin(budget);
        let timed_out = tokio::select! {
            biased;
            () = self.drained() => false,
            () = self.shared.cleanup.ended() => true,
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

    /// Resolves when one of these happens, logged as `jobs_engine_task_stopped`
    /// with the task that caused it:
    ///
    /// - an attempt supervisor ends without retiring;
    /// - the claim loop, or the retention, listener, or sampling task this
    ///   engine started, ends before its cancellation;
    /// - any of those tasks panics, even while cancellation is in progress;
    /// - a supervisor panics, even after it retired.
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

/// Whether an engine owns the process-wide duties or serves beside the engine
/// that does, sharing its worker id and peers.
#[derive(Clone, Copy)]
enum Role<'a> {
    Owner,
    Beside(&'a Shared),
}

/// The engine's own tasks. Each runs until its token is cancelled.
enum Run {
    Claim {
        // Deregister the peer before releasing drain waiters, even before the first poll.
        _peer: PeerRegistration,
        _close: CloseTracker,
    },
    Retention,
    Listener,
    Sampling,
}

impl Run {
    /// The task label of the `jobs_engine_task_stopped` record this task ends
    /// with, so a caller cannot pair a run with the wrong label.
    const fn label(&self) -> &'static str {
        match self {
            Self::Claim { .. } => "claim",
            Self::Retention => "retention",
            Self::Listener => "listener",
            Self::Sampling => "sampling",
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
    let guard = TaskGuard {
        task: run.label(),
        token: token.clone(),
        failure: failure.clone(),
    };
    tracker.spawn(async move {
        let _guard = guard;
        match run {
            _claim @ Run::Claim { .. } => claim::run_claim_loop(shared, token, failure).await,
            Run::Retention => maintenance::run_retention(shared, token).await,
            Run::Listener => listener::run_listener(shared, token).await,
            Run::Sampling => maintenance::run_sampling(shared, token).await,
        }
    });
}

struct CloseTracker(TaskTracker);

impl Drop for CloseTracker {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Ends its start when the supervised attempt ends abnormally.
///
/// Armed before submission, including when the supervisor is never polled, so
/// custody does not depend on the supervisor being run. `stop` is the
/// claim-stop token of the engine that admitted the claim: an abnormal end, or
/// an abort while the supervisor is still running, fails the engine through
/// `failure` and stops new claim rounds through `stop`. The running round keeps
/// its custody, and the claim loop's exit closes `attempt_tracker`, which
/// [`Started::drained`] waits on, so the drain still waits for every other
/// supervisor to end.
pub(crate) struct SupervisorGuard {
    stop: CancellationToken,
    failure: CancellationToken,
    /// The attempt's job, named in the record: the guard drops outside the
    /// attempt span, so the span's fields do not reach it.
    job_id: crate::kind::JobId,
    kind: &'static str,
    retired: bool,
}

impl SupervisorGuard {
    pub(crate) fn new(
        stop: CancellationToken,
        failure: CancellationToken,
        job_id: crate::kind::JobId,
        kind: &'static str,
    ) -> Self {
        Self {
            stop,
            failure,
            job_id,
            kind,
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

/// Dropped when its task ends, a panic included: an end before the token
/// was cancelled, or any panic during shutdown, fails the engine.
///
/// The worker's staged shutdown owns the order in which work stops after this
/// guard signals failure. [`SupervisorGuard`] also stops claiming when the
/// attempt it guards can no longer settle its claim.
struct TaskGuard {
    task: &'static str,
    token: CancellationToken,
    failure: CancellationToken,
}

impl Drop for TaskGuard {
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
struct Peer {
    kinds: Vec<&'static str>,
    wake: Arc<Notify>,
}

#[derive(Default)]
struct Peers {
    entries: Vec<Peer>,
    sample_revision: Arc<()>,
}

impl Peers {
    fn invalidate_sample(&mut self) {
        self.sample_revision = Arc::default();
        maintenance::invalidate_sample();
    }
}

/// The peer list is never held across an `.await`.
fn lock_peers(peers: &Mutex<Peers>) -> MutexGuard<'_, Peers> {
    peers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Owned by the unstarted engine, then by its claim task.
struct PeerRegistration {
    peers: Arc<Mutex<Peers>>,
    wake: Arc<Notify>,
}

impl PeerRegistration {
    fn new(registry: &Kinds, peers: &Arc<Mutex<Peers>>, wake: &Arc<Notify>) -> Self {
        let mut registry_peers = lock_peers(peers);
        let previous = registered_kinds(&registry_peers.entries);
        let kinds: Vec<_> = registry.names().collect();
        for &kind in &kinds {
            if previous.binary_search(&kind).is_err() {
                maintenance::reset_kind_metrics(kind);
            }
        }
        registry_peers.entries.push(Peer {
            kinds,
            wake: Arc::clone(wake),
        });
        registry_peers.invalidate_sample();
        Self {
            peers: Arc::clone(peers),
            wake: Arc::clone(wake),
        }
    }
}

impl Drop for PeerRegistration {
    fn drop(&mut self) {
        let mut peers = lock_peers(&self.peers);
        let Some(index) = peers
            .entries
            .iter()
            .position(|peer| Arc::ptr_eq(&peer.wake, &self.wake))
        else {
            return;
        };
        let removed = peers.entries.swap_remove(index);
        let remaining = registered_kinds(&peers.entries);
        let mut changed = false;
        for kind in removed.kinds {
            if remaining.binary_search(&kind).is_err() {
                maintenance::reset_kind_metrics(kind);
                changed = true;
            }
        }
        if changed {
            peers.invalidate_sample();
        }
    }
}

/// Every peer's kinds as one list, sorted and without duplicates.
///
/// [`PeerRegistration`] looks up kinds in this list with binary search.
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
    pub(crate) registry: Kinds,
    max_workers: NonZeroU32,
    pub(crate) slots: Arc<Semaphore>,
    /// Held by the engine's own statements (claim, retention, sampling) while
    /// they use a pooled connection, so they take one connection between them:
    /// the pool budget is `jobs.max_workers + 2`, one connection per attempt
    /// slot plus this one and readiness.
    pub(crate) statement_turn: tokio::sync::Mutex<()>,
    pub(crate) cleanup: ForcedCleanup,
    pub(crate) counters: Counters,
    pub(crate) attempt_tracker: TaskTracker,
    /// Per operation: set after the first failure, clear after recovery.
    failing: EnumMap<Operation, AtomicBool>,
    pub(crate) completions: crate::attempt::Completions,
    pub(crate) owned_attempts: crate::attempt::MembershipGauge,
    /// Set when a due job of a registered kind was committed.
    pub(crate) wake: Arc<Notify>,
    /// Every engine of this worker process, this one included.
    peers: Arc<Mutex<Peers>>,
    owns_process_duties: bool,
}

impl Shared {
    /// Snapshot the kinds and the identity that authorizes publishing their sample.
    pub(crate) fn sampling_kinds(&self) -> (Vec<&'static str>, Arc<()>) {
        let peers = lock_peers(&self.peers);
        (
            registered_kinds(&peers.entries),
            Arc::clone(&peers.sample_revision),
        )
    }

    /// A change can invalidate a sample even if the same kinds later return.
    pub(crate) fn publish_if_current(&self, revision: &Arc<()>, publish: impl FnOnce()) -> bool {
        let peers = lock_peers(&self.peers);
        if !Arc::ptr_eq(&peers.sample_revision, revision) {
            return false;
        }
        publish();
        true
    }

    /// Wake every engine of this process that registers `kind`, or all of
    /// them when `kind` is `None`.
    pub(crate) fn wake_peers(&self, kind: Option<&str>) {
        for peer in &lock_peers(&self.peers).entries {
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

/// A forced cleanup: the token supervisors cancel their handlers on, and the
/// deadline they stop waiting at.
///
/// [`Self::begin`] fixes the deadline before it fires the token, so a
/// supervisor that sees the cancellation also sees the deadline.
#[derive(Default)]
pub(crate) struct ForcedCleanup {
    force: CancellationToken,
    deadline: OnceLock<Instant>,
}

impl ForcedCleanup {
    /// The first call fixes the deadline at `budget` from now; a later call
    /// keeps it.
    fn begin(&self, budget: Duration) {
        self.deadline.get_or_init(|| {
            let now = Instant::now();
            now.checked_add(budget).unwrap_or(now + NO_CLEANUP_DEADLINE)
        });
        self.force.cancel();
    }

    /// Resolves once a cleanup is forced.
    pub(crate) async fn forced(&self) {
        self.force.cancelled().await;
    }

    /// Resolves once the budget of a forced cleanup is spent; pending until
    /// a cleanup is forced.
    pub(crate) async fn ended(&self) {
        self.force.cancelled().await;
        match self.deadline.get() {
            Some(deadline) => tokio::time::sleep_until(*deadline).await,
            None => std::future::pending().await,
        }
    }

    pub(crate) fn has_ended(&self) -> bool {
        self.deadline
            .get()
            .is_some_and(|deadline| Instant::now() >= *deadline)
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

/// Logs the SQLSTATE when the database sent one, and a bounded cause otherwise.
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
    tracing::warn!(
        operation = operation.as_str(),
        failure = error.as_str(),
        sqlstate = sqlstate.as_deref(),
        cause,
        "jobs_operation_failed"
    );
}

pub(crate) fn observe_recovery(shared: &Shared, operation: Operation) {
    if shared.failing[operation].swap(false, Ordering::SeqCst) {
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
    async fn late_peer_invalidates_freshness_and_rejects_an_in_flight_old_union() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let gauges = Gauges::default();
        metrics::with_local_recorder(&gauges, || {
            let mut kinds = crate::KindsBuilder::new();
            kinds.register(crate::Policy::default(), |_: crate::Job<Ordinary>| async {
                Ok(())
            });
            let engine = Engine::new(pool.clone(), kinds.validate().unwrap(), NonZeroU32::MIN);
            let (sampled_kinds, sampled_revision) = engine.shared.sampling_kinds();
            assert!(engine.shared.publish_if_current(&sampled_revision, || {
                metrics::gauge!(maintenance::FAILED_JOBS_METRIC, "kind" => "ordinary").set(3.0);
                metrics::gauge!(maintenance::OBSERVATION_TIMESTAMP_METRIC).set(100.0);
            }));

            let make_publisher = || {
                let mut kinds = crate::KindsBuilder::new();
                kinds.register(crate::Policy::default(), |_: crate::Job<Publisher>| async {
                    Ok(())
                });
                engine.beside(kinds.validate().unwrap(), NonZeroU32::MIN)
            };
            let publisher = make_publisher();
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
            assert!(!engine.shared.publish_if_current(&sampled_revision, || {
                panic!("an observation from before peer admission must not publish");
            }));
            assert_eq!(
                gauges.get(maintenance::OBSERVATION_TIMESTAMP_METRIC, None),
                0.0
            );

            let (current_kinds, current_revision) = engine.shared.sampling_kinds();
            assert!(engine.shared.publish_if_current(&current_revision, || {
                metrics::gauge!(maintenance::OBSERVATION_TIMESTAMP_METRIC).set(200.0);
            }));
            assert_eq!(
                gauges.get(maintenance::OBSERVATION_TIMESTAMP_METRIC, None),
                200.0
            );

            drop(publisher);
            assert_eq!(engine.shared.sampling_kinds().0, sampled_kinds);
            assert!(!engine.shared.publish_if_current(&current_revision, || {
                panic!("an observation that still includes the retired peer must not publish");
            }));
            let _replacement = make_publisher();
            assert_eq!(engine.shared.sampling_kinds().0, current_kinds);
            assert!(!engine.shared.publish_if_current(&current_revision, || {
                panic!("an observation from before peer replacement must not publish");
            }));
            assert_eq!(
                gauges.get(maintenance::OBSERVATION_TIMESTAMP_METRIC, None),
                0.0
            );
        });
        pool.close().await;
    }

    #[tokio::test]
    async fn drained_peer_retires_its_registration_while_started_is_still_held() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let mut kinds = crate::KindsBuilder::new();
        kinds.register(crate::Policy::default(), |_: crate::Job<Ordinary>| async {
            Ok(())
        });
        let owner = Engine::new(pool.clone(), kinds.validate().unwrap(), NonZeroU32::MIN);
        let mut kinds = crate::KindsBuilder::new();
        kinds.register(crate::Policy::default(), |_: crate::Job<Publisher>| async {
            Ok(())
        });
        let peer = owner.beside(kinds.validate().unwrap(), NonZeroU32::MIN);
        let tracker = TaskTracker::new();
        let stop = CancellationToken::new();
        stop.cancel();
        let started = peer.start(&tracker, &stop);
        tokio::time::timeout(Duration::from_secs(1), started.drained())
            .await
            .unwrap();
        assert_eq!(owner.shared.sampling_kinds().0, ["ordinary"]);
        tracker.close();
        tokio::time::timeout(Duration::from_secs(1), tracker.wait())
            .await
            .unwrap();
        pool.close().await;
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
        let guard = TaskGuard {
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
        drop(TaskGuard {
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
    async fn supervisor_custody_survives_unpolled_drop_and_abort() {
        let gauges = Gauges::default();
        let _local = metrics::set_default_local_recorder(&gauges);
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
                    payload: b"null".to_vec(),
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
        let guard = SupervisorGuard::new(
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
