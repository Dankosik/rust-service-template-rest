//! The job engine: shared state and bounded supervisor cleanup.

use std::fmt;
use std::future::Future;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use sqlx::postgres::PgPool;
use tokio::sync::Semaphore;
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
pub const CANCEL_MARGIN: Duration = Duration::from_secs(2);
/// How long an outcome write waits before it is sent again.
pub const RECORD_RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// Client bound around one engine statement: acquire and statement acknowledgement.
pub const OPERATION_BACKSTOP: Duration = Duration::from_secs(12);
/// Counter of failed engine statements. Label `operation`.
pub const OPERATION_FAILURES_METRIC: &str = "jobs_worker_operation_failures_total";

/// A worker's job engine. Cloning shares the pool, registry, and supervisor tracker.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OperationError {
    /// The pool did not hand out a connection.
    #[error("acquire")]
    Acquire,
    /// The statement failed, or a returned row did not decode.
    #[error("statement")]
    Statement,
    /// The client bound fired before the statement returned.
    #[error("timed out")]
    TimedOut,
}

impl OperationError {
    /// The `failure` field of the operation records.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Acquire => "acquire",
            Self::Statement => "statement",
            Self::TimedOut => "timed_out",
        }
    }
}

impl Engine {
    /// Build an engine over a writable UTF8 pool with READ COMMITTED defaults.
    /// Call [`Self::check_startup`] before starting it. Does no I/O.
    #[must_use]
    pub fn new(pool: PgPool, registry: Registry, max_workers: NonZeroU32) -> Self {
        let slots = usize::try_from(max_workers.get()).unwrap_or(usize::MAX);
        Self {
            shared: Arc::new(Shared {
                pool,
                worker_id: *WORKER_ID.get_or_init(uuid::Uuid::new_v4),
                registry,
                max_workers,
                slots: Arc::new(Semaphore::new(slots)),
                permit: Semaphore::new(1),
                force: CancellationToken::new(),
                cleanup_deadline: Mutex::new(None),
                counters: Counters::default(),
                attempt_tracker: TaskTracker::new(),
                failing: Failing::new(),
            }),
        }
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

    /// Spawn the claim loop, retention, and gauge sampling on `tracker`.
    ///
    /// Each task runs under a child of `cancel`. Does no I/O before it returns.
    #[must_use]
    pub fn start(&self, tracker: &TaskTracker, cancel: &CancellationToken) -> Started {
        crate::attempt::describe_metrics();
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
        let retention_cancel = cancel.child_token();
        tracker.spawn(maintenance::run_retention(
            Arc::clone(&self.shared),
            retention_cancel,
        ));
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
    #[must_use]
    pub async fn cancel_and_finish(&self, budget: Duration) -> DrainEnd {
        let requested = Instant::now()
            .checked_add(budget)
            .unwrap_or_else(Instant::now);
        self.stop_claiming();
        let deadline = {
            let mut stored = lock(&self.shared.cleanup_deadline);
            *stored.get_or_insert(requested)
        };
        self.shared.force.cancel();
        let timed_out = tokio::time::timeout_at(deadline, self.drained())
            .await
            .is_err();
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

static WORKER_ID: OnceLock<uuid::Uuid> = OnceLock::new();

pub(crate) struct Shared {
    pub(crate) worker_id: uuid::Uuid,
    pub(crate) pool: PgPool,
    pub(crate) registry: Registry,
    pub(crate) max_workers: NonZeroU32,
    pub(crate) slots: Arc<Semaphore>,
    pub(crate) permit: Semaphore,
    pub(crate) force: CancellationToken,
    cleanup_deadline: Mutex<Option<Instant>>,
    pub(crate) counters: Counters,
    pub(crate) attempt_tracker: TaskTracker,
    pub(crate) failing: Failing,
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

impl Shared {
    pub(crate) fn deadline(&self, local: Instant) -> Instant {
        lock(&self.cleanup_deadline).map_or(local, |cleanup| local.min(cleanup))
    }
}

/// One engine statement. The label is [`Operation::as_str`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Claim,
    Record,
    Release,
    Retention,
    Sample,
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
}

impl Failing {
    const fn new() -> Self {
        Self {
            claim: AtomicBool::new(false),
            record: AtomicBool::new(false),
            release: AtomicBool::new(false),
            retention: AtomicBool::new(false),
            sample: AtomicBool::new(false),
        }
    }

    fn flag(&self, operation: Operation) -> &AtomicBool {
        match operation {
            Operation::Claim => &self.claim,
            Operation::Record => &self.record,
            Operation::Release => &self.release,
            Operation::Retention => &self.retention,
            Operation::Sample => &self.sample,
        }
    }
}

impl fmt::Debug for Failing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Failing").finish_non_exhaustive()
    }
}

pub(crate) fn observe_failure(shared: &Shared, operation: Operation, error: OperationError) {
    metrics::counter!(OPERATION_FAILURES_METRIC, "operation" => operation.as_str()).increment(1);
    if !shared.failing.flag(operation).swap(true, Ordering::SeqCst) {
        tracing::warn!(
            operation = operation.as_str(),
            failure = error.as_str(),
            "jobs_operation_failed"
        );
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn backstop_bounds_an_unacknowledged_operation() {
        assert_eq!(
            backstop(std::future::pending::<Result<(), OperationError>>()).await,
            Err(OperationError::TimedOut)
        );
    }
}
