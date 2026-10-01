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
    /// connection and terminal retention. Engines made with [`Self::beside`]
    /// share them.
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
    /// It claims under this engine's worker id, and this engine's listener
    /// and retention serve it: start both.
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
        peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Peer {
                kinds: registry.names().collect(),
                wake: Arc::clone(&wake),
            });
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
        maintenance::check_startup(&self.shared).await
    }

    /// Delete expired terminal jobs once and return how many were deleted.
    ///
    /// # Errors
    ///
    /// [`OperationError`] from the batch that failed. Earlier batches stay committed.
    pub async fn remove_expired(&self) -> Result<u64, OperationError> {
        maintenance::remove_expired(&self.shared).await
    }

    /// Spawn the claim loop and gauge sampling on `tracker`, and for an
    /// engine built with [`Self::new`] also the listener and retention.
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
        maintenance::init_metrics(&self.shared);
        metrics::describe_counter!(
            OPERATION_FAILURES_METRIC,
            "Failed worker database operations"
        );
        tracing::info!(worker.id = %self.shared.worker_id, "jobs_engine_starting");
        let stop = cancel.child_token();
        let failure = CancellationToken::new();
        spawn_guarded(
            tracker,
            Arc::clone(&self.shared),
            stop.clone(),
            failure.clone(),
            claim::run_claim_loop,
        );
        if self.shared.owns_process_duties {
            tracker.spawn(maintenance::run_retention(
                Arc::clone(&self.shared),
                cancel.child_token(),
            ));
            tracker.spawn(claim::run_listener(
                Arc::clone(&self.shared),
                cancel.child_token(),
            ));
        }
        let sample_cancel = cancel.child_token();
        tracker.spawn(maintenance::run_sampling(
            Arc::clone(&self.shared),
            sample_cancel,
        ));
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

    /// Resolves if the claim loop ends on its own.
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

fn spawn_guarded<F, Fut>(
    tracker: &TaskTracker,
    shared: Arc<Shared>,
    token: CancellationToken,
    failure: CancellationToken,
    run: F,
) where
    F: FnOnce(Arc<Shared>, CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let guard_token = token.clone();
    tracker.spawn(async move {
        let _guard = FailUnlessCancelled {
            token: guard_token,
            failure,
        };
        run(shared, token).await;
    });
}

struct FailUnlessCancelled {
    token: CancellationToken,
    failure: CancellationToken,
}

impl Drop for FailUnlessCancelled {
    fn drop(&mut self) {
        if !self.token.is_cancelled() {
            self.failure.cancel();
        }
    }
}

/// Stands in for a cleanup budget too long to add to the clock.
const NO_CLEANUP_DEADLINE: Duration = Duration::from_hours(24 * 365);

/// One engine as the process's listener sees it.
pub(crate) struct Peer {
    kinds: Vec<&'static str>,
    wake: Arc<Notify>,
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
    /// Whether this engine runs the listener and retention.
    owns_process_duties: bool,
}

impl Shared {
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
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn backstop_bounds_an_unacknowledged_operation() {
        assert!(matches!(
            backstop(std::future::pending::<Result<(), OperationError>>()).await,
            Err(OperationError::TimedOut)
        ));
    }
}
