//! The job engine: shared state, the attempt registry, and the drain-end protocol.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use infra_postgres::{Isolation, TxError, TxOptions};
use sqlx::postgres::PgPool;
use tokio::sync::{Semaphore, watch};
use tokio::task::AbortHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::Registry;
use crate::attempt::{self, Attribution, Facts, Intended, LostReason, Outcome};
use crate::claim::{self, Claimed};
use crate::kind::JobId;
use crate::lease::{self, RowState};
use crate::maintenance;

/// How often an idle worker claims.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Claim and extension expiry on the database clock.
pub const CLAIM_TTL: Duration = Duration::from_secs(30);
/// How often one worker extends its claims.
pub const UPKEEP_INTERVAL: Duration = Duration::from_secs(10);
/// How far the worker's own cancellation leads the database expiry.
pub const CANCEL_MARGIN: Duration = Duration::from_secs(2);
/// How long an outcome write waits before it is sent again.
pub const RECORD_RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// Client bound around one engine statement: acquire, statement, and commit.
pub const OPERATION_BACKSTOP: Duration = Duration::from_secs(12);
/// Counter of failed engine statements. Label `operation`.
pub const OPERATION_FAILURES_METRIC: &str = "jobs_worker_operation_failures_total";

/// Read committed, read-write. Keeps `SKIP LOCKED` from seeing a stricter default.
pub(crate) const READ_COMMITTED: TxOptions = TxOptions {
    isolation: Isolation::ReadCommitted,
    read_only: false,
};

/// A worker's job engine. Cloning shares the pool, registry, and attempt registry.
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

/// How a drain-end release finished.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainEnd {
    /// Entries taken by [`Started::cancel_and_release`].
    pub cancelled: usize,
    /// Recorded `released`.
    pub released: usize,
    /// Recorded as the attempt's intended outcome.
    pub written: usize,
    /// Recorded `superseded`.
    pub superseded: usize,
    /// Recorded `lost` with reason `release`.
    pub lost: usize,
    /// The deadline passed before every attempt was attributed.
    pub timed_out: bool,
}

/// Why the jobs store cannot start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartupError {
    /// The table or one of its columns is missing.
    #[error("the jobs schema is missing")]
    SchemaMissing,
    /// The session is read-only or recovering.
    #[error("the PostgreSQL session is not writable")]
    NotWritable,
    /// Anything else, including the check's bound.
    #[error("the jobs store is unavailable")]
    Unavailable,
}

/// Why one engine statement did not finish as a known commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OperationError {
    /// The pool did not hand out a connection.
    #[error("acquire")]
    Acquire,
    /// `BEGIN` failed.
    #[error("begin")]
    Begin,
    /// The statement failed, or a returned row did not decode.
    #[error("statement")]
    Statement,
    /// The server rejected the commit. Nothing was written.
    #[error("commit")]
    Commit,
    /// The statement returned and no commit acknowledgement followed.
    #[error("commit outcome unknown")]
    CommitUnknown,
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
            Self::Begin => "begin",
            Self::Statement => "statement",
            Self::Commit => "commit",
            Self::CommitUnknown => "commit_unknown",
            Self::TimedOut => "timed_out",
        }
    }
}

impl Engine {
    /// Build an engine. Does no I/O.
    #[must_use]
    pub fn new(pool: PgPool, registry: Registry, max_workers: NonZeroU32) -> Self {
        let slots = usize::try_from(max_workers.get()).unwrap_or(usize::MAX);
        Self {
            shared: Arc::new(Shared {
                pool,
                registry,
                max_workers,
                slots: Arc::new(Semaphore::new(slots)),
                permit: Semaphore::new(1),
                attempts: Attempts::new(),
                attempt_tracker: TaskTracker::new(),
                failing: Failing::new(),
            }),
        }
    }

    /// Check the jobs schema and a writable session, bounded to 5 s.
    ///
    /// # Errors
    ///
    /// [`StartupError::SchemaMissing`] when the table or a column is missing,
    /// [`StartupError::NotWritable`] when the session is read-only or recovering,
    /// and [`StartupError::Unavailable`] for anything else, including the bound.
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

    /// Spawn the claim loop, claim upkeep, retention, and gauge sampling on `tracker`.
    ///
    /// Each task runs under a child of `cancel`. Does no I/O before it returns.
    #[must_use]
    pub fn start(&self, tracker: &TaskTracker, cancel: &CancellationToken) -> Started {
        let stop = cancel.child_token();
        let failure = CancellationToken::new();
        spawn_guarded(
            tracker,
            Arc::clone(&self.shared),
            stop.clone(),
            failure.clone(),
            claim::run_claim_loop,
        );
        let upkeep_cancel = cancel.child_token();
        spawn_guarded(
            tracker,
            Arc::clone(&self.shared),
            upkeep_cancel,
            failure.clone(),
            lease::run_upkeep,
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
    /// Stop claiming. Idempotent. A claim already sent still completes.
    pub fn stop_claiming(&self) {
        self.stop.cancel();
    }

    /// Attempts admitted and not yet recorded.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.shared.attempts.len()
    }

    /// Resolves once the claim loop has exited and every supervisor has ended.
    pub async fn drained(&self) {
        self.shared.attempt_tracker.wait().await;
    }

    /// Cancel in-flight attempts and release their jobs, inside `budget`.
    #[must_use]
    pub async fn cancel_and_release(&self, budget: Duration) -> DrainEnd {
        let deadline = Instant::now()
            .checked_add(budget)
            .unwrap_or_else(Instant::now);
        self.stop_claiming();
        let entries = self.shared.attempts.close();
        let cancelled = entries.len();
        let collected = entries.into_iter().map(collect_entry).collect();
        let mut end = release_collected(&self.shared, collected, deadline).await;
        end.cancelled = cancelled;
        end
    }

    /// Resolves if the claim loop or claim upkeep ends on its own.
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

pub(crate) struct Shared {
    pub(crate) pool: PgPool,
    pub(crate) registry: Registry,
    pub(crate) max_workers: NonZeroU32,
    pub(crate) slots: Arc<Semaphore>,
    pub(crate) permit: Semaphore,
    pub(crate) attempts: Attempts,
    pub(crate) attempt_tracker: TaskTracker,
    pub(crate) failing: Failing,
}

impl fmt::Debug for Shared {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Shared")
            .field("max_workers", &self.max_workers)
            .field("in_flight", &self.attempts.len())
            .finish_non_exhaustive()
    }
}

/// One engine statement. The label is [`Operation::as_str`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Claim,
    Extend,
    Record,
    Release,
    Reconcile,
    Retention,
    Sample,
}

impl Operation {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Extend => "extend",
            Self::Record => "record",
            Self::Release => "release",
            Self::Reconcile => "reconcile",
            Self::Retention => "retention",
            Self::Sample => "sample",
        }
    }
}

/// One flag per [`Operation`]: set after the first failure, clear after recovery.
pub(crate) struct Failing {
    claim: AtomicBool,
    extend: AtomicBool,
    record: AtomicBool,
    release: AtomicBool,
    reconcile: AtomicBool,
    retention: AtomicBool,
    sample: AtomicBool,
}

impl Failing {
    const fn new() -> Self {
        Self {
            claim: AtomicBool::new(false),
            extend: AtomicBool::new(false),
            record: AtomicBool::new(false),
            release: AtomicBool::new(false),
            reconcile: AtomicBool::new(false),
            retention: AtomicBool::new(false),
            sample: AtomicBool::new(false),
        }
    }

    fn flag(&self, operation: Operation) -> &AtomicBool {
        match operation {
            Operation::Claim => &self.claim,
            Operation::Extend => &self.extend,
            Operation::Record => &self.record,
            Operation::Release => &self.release,
            Operation::Reconcile => &self.reconcile,
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

/// The `closed` flag and the admitted attempts, keyed by claim.
type Table = (bool, HashMap<(JobId, i64), Entry>);

pub(crate) struct Attempts {
    inner: Mutex<Table>,
}

pub(crate) struct Entry {
    pub(crate) id: JobId,
    pub(crate) generation: i64,
    pub(crate) kind: &'static str,
    pub(crate) attempt: u16,
    pub(crate) deadline: watch::Sender<Instant>,
    pub(crate) superseded: CancellationToken,
    pub(crate) handler_cancel: CancellationToken,
    pub(crate) supervisor: AbortHandle,
    pub(crate) handler: Option<(AbortHandle, Instant)>,
    pub(crate) extending: bool,
    pub(crate) intended: Option<Intended>,
}

pub(crate) struct Handles {
    pub(crate) deadline: watch::Receiver<Instant>,
    pub(crate) superseded: CancellationToken,
    pub(crate) handler_cancel: CancellationToken,
}

impl fmt::Debug for Attempts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Attempts")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Entry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Entry")
            .field("id", &self.id)
            .field("generation", &self.generation)
            .field("kind", &self.kind)
            .field("attempt", &self.attempt)
            .field("extending", &self.extending)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Handles {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Handles").finish_non_exhaustive()
    }
}

impl Attempts {
    fn new() -> Self {
        Self {
            inner: Mutex::new((false, HashMap::new())),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Table> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(crate) fn admit(
        &self,
        claimed: Claimed,
        deadline: Instant,
        spawn: impl FnOnce(Claimed, Handles) -> AbortHandle,
    ) -> Result<(), Claimed> {
        let mut guard = self.lock();
        if guard.0 {
            return Err(claimed);
        }
        let (sender, receiver) = watch::channel(deadline);
        let superseded = CancellationToken::new();
        let handler_cancel = CancellationToken::new();
        let handles = Handles {
            deadline: receiver,
            superseded: superseded.clone(),
            handler_cancel: handler_cancel.clone(),
        };
        let id = claimed.id;
        let generation = claimed.generation;
        let kind = claimed.kind;
        let attempt = claimed.attempt;
        let supervisor = spawn(claimed, handles);
        guard.1.insert(
            (id, generation),
            Entry {
                id,
                generation,
                kind,
                attempt,
                deadline: sender,
                superseded,
                handler_cancel,
                supervisor,
                handler: None,
                extending: true,
                intended: None,
            },
        );
        Ok(())
    }

    #[must_use]
    pub(crate) fn start_handler(
        &self,
        id: JobId,
        generation: i64,
        spawn: impl FnOnce() -> AbortHandle,
    ) -> bool {
        let mut guard = self.lock();
        let Some(entry) = guard.1.get_mut(&(id, generation)) else {
            return false;
        };
        let handle = spawn();
        entry.handler = Some((handle, Instant::now()));
        true
    }

    #[must_use]
    pub(crate) fn settle(&self, id: JobId, generation: i64, intended: Intended) -> bool {
        let mut guard = self.lock();
        let Some(entry) = guard.1.get_mut(&(id, generation)) else {
            return false;
        };
        entry.intended = Some(intended);
        entry.extending = false;
        true
    }

    #[must_use]
    pub(crate) fn extending(&self) -> Vec<(JobId, i64)> {
        self.lock()
            .1
            .iter()
            .filter(|(_, entry)| entry.extending)
            .map(|(key, _)| *key)
            .collect()
    }

    pub(crate) fn acknowledge(&self, id: JobId, generation: i64, sent: Instant) {
        let guard = self.lock();
        let Some(entry) = guard.1.get(&(id, generation)) else {
            return;
        };
        if entry.extending {
            let _previous = entry.deadline.send_replace(lease::local_deadline(sent));
        }
    }

    pub(crate) fn supersede(&self, id: JobId, generation: i64) {
        let guard = self.lock();
        let Some(entry) = guard.1.get(&(id, generation)) else {
            return;
        };
        if entry.extending {
            entry.superseded.cancel();
        }
    }

    pub(crate) fn take(&self, id: JobId, generation: i64) -> Option<Entry> {
        self.lock().1.remove(&(id, generation))
    }

    #[must_use]
    pub(crate) fn close(&self) -> Vec<Entry> {
        let mut guard = self.lock();
        guard.0 = true;
        guard.1.drain().map(|(_, entry)| entry).collect()
    }

    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.lock().1.len()
    }
}

pub(crate) struct Collected {
    pub(crate) id: JobId,
    pub(crate) kind: &'static str,
    pub(crate) generation: i64,
    pub(crate) attempt: u16,
    pub(crate) intended: Option<Intended>,
    pub(crate) ran: Option<Duration>,
    pub(crate) deadline: Instant,
}

impl fmt::Debug for Collected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Collected")
            .field("id", &self.id)
            .field("generation", &self.generation)
            .field("kind", &self.kind)
            .field("attempt", &self.attempt)
            .finish_non_exhaustive()
    }
}

fn collect_entry(entry: Entry) -> Collected {
    entry.handler_cancel.cancel();
    if let Some((handle, _)) = &entry.handler {
        handle.abort();
    }
    entry.supervisor.abort();
    let ran = entry.intended.as_ref().map_or_else(
        || entry.handler.as_ref().map(|(_, started)| started.elapsed()),
        |intended| intended.ran,
    );
    Collected {
        id: entry.id,
        kind: entry.kind,
        generation: entry.generation,
        attempt: entry.attempt,
        intended: entry.intended,
        ran,
        deadline: *entry.deadline.borrow(),
    }
}

/// The error of every engine transaction closure.
#[derive(Debug)]
pub(crate) struct OpFailed(pub(crate) OperationError);

impl From<TxError> for OpFailed {
    fn from(err: TxError) -> Self {
        Self(match err {
            TxError::Acquire(_) => OperationError::Acquire,
            TxError::Begin(_) => OperationError::Begin,
            TxError::CommitFailed(_) => OperationError::Commit,
            TxError::CommitUnknown(_) => OperationError::CommitUnknown,
        })
    }
}

impl From<sqlx::Error> for OpFailed {
    fn from(_err: sqlx::Error) -> Self {
        Self(OperationError::Statement)
    }
}

/// Bound one transaction by [`OPERATION_BACKSTOP`].
///
/// The closure sets `returned` once its statement has returned. An expiry after
/// that is [`OperationError::CommitUnknown`]; before it, [`OperationError::TimedOut`].
///
/// # Errors
///
/// [`OperationError`] for an acquire, begin, statement, commit, unknown commit, or the bound.
pub(crate) async fn backstop<T>(
    returned: &AtomicBool,
    transaction: impl Future<Output = Result<T, OpFailed>>,
) -> Result<T, OperationError> {
    match tokio::time::timeout(OPERATION_BACKSTOP, transaction).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(OpFailed(error))) => Err(error),
        Err(_elapsed) => {
            if returned.load(Ordering::SeqCst) {
                Err(OperationError::CommitUnknown)
            } else {
                Err(OperationError::TimedOut)
            }
        }
    }
}

type ReleaseResult = Result<Vec<(JobId, i64)>, OperationError>;
type ReconcileResult = Result<(Instant, Vec<RowState>), OperationError>;

struct DrainProgress {
    release: Mutex<Option<ReleaseResult>>,
    reconcile: Mutex<Option<ReconcileResult>>,
}

pub(crate) async fn release_collected(
    shared: &Shared,
    collected: Vec<Collected>,
    deadline: Instant,
) -> DrainEnd {
    if collected.is_empty() {
        return DrainEnd::default();
    }
    let progress = DrainProgress {
        release: Mutex::new(None),
        reconcile: Mutex::new(None),
    };
    let timed_out = drive_release(shared, &collected, deadline, &progress).await;
    record_drain(shared, &collected, &progress, timed_out)
}

async fn drive_release(
    shared: &Shared,
    collected: &[Collected],
    deadline: Instant,
    progress: &DrainProgress,
) -> bool {
    if Instant::now() >= deadline {
        return true;
    }
    let aborted = AtomicBool::new(false);
    let timed_out = tokio::time::timeout_at(
        deadline,
        release_and_reconcile(shared, collected, deadline, progress, &aborted),
    )
    .await
    .is_err();
    timed_out || aborted.load(Ordering::SeqCst)
}

async fn release_and_reconcile(
    shared: &Shared,
    collected: &[Collected],
    deadline: Instant,
    progress: &DrainProgress,
    aborted: &AtomicBool,
) {
    if Instant::now() >= deadline {
        aborted.store(true, Ordering::SeqCst);
        return;
    }
    let Ok(_permit) = shared.permit.acquire().await else {
        return;
    };
    let pairs: Vec<(JobId, i64)> = collected
        .iter()
        .map(|claim| (claim.id, claim.generation))
        .collect();
    let release_result = lease::release(shared, &pairs).await;
    *lock(&progress.release) = Some(release_result);
    if Instant::now() >= deadline {
        aborted.store(true, Ordering::SeqCst);
        return;
    }
    let id_list = ids_to_reconcile(collected, lock(&progress.release).as_ref());
    if id_list.is_empty() {
        return;
    }
    let reconcile_result = lease::reconcile(shared, &id_list).await;
    *lock(&progress.reconcile) = Some(match reconcile_result {
        Ok(rows) => Ok((Instant::now(), rows)),
        Err(error) => Err(error),
    });
}

fn ids_to_reconcile(collected: &[Collected], release: Option<&ReleaseResult>) -> Vec<JobId> {
    let returned = match release {
        Some(Ok(returned)) => returned.as_slice(),
        _ => &[],
    };
    collected
        .iter()
        .filter(|claim| !returned.iter().any(|pair| pair_is(pair, claim)))
        .map(|claim| claim.id)
        .collect()
}

fn pair_is(pair: &(JobId, i64), claim: &Collected) -> bool {
    pair.0 == claim.id && pair.1 == claim.generation
}

fn record_drain(
    shared: &Shared,
    collected: &[Collected],
    progress: &DrainProgress,
    timed_out: bool,
) -> DrainEnd {
    let (released, release_unknown) = take_release(shared, progress);
    let rows = take_reconcile(shared, progress);
    let mut end = DrainEnd {
        timed_out,
        ..DrainEnd::default()
    };
    for claim in collected {
        let outcome = if released.contains(&(claim.id, claim.generation)) {
            Outcome::Released
        } else {
            let (attribution, newer) = attribution_of(claim, rows.as_ref());
            attempt::drain_outcome(
                attribution,
                newer,
                release_unknown,
                claim.intended.as_ref().map(|intended| intended.outcome),
            )
        };
        attempt::record(outcome, &facts_of(claim, outcome));
        tally(&mut end, outcome);
    }
    end
}

fn take_release(shared: &Shared, progress: &DrainProgress) -> (HashSet<(JobId, i64)>, bool) {
    match lock(&progress.release).as_ref() {
        Some(Ok(returned)) => {
            observe_recovery(shared, Operation::Release);
            (returned.iter().copied().collect(), false)
        }
        Some(Err(error)) => {
            observe_failure(shared, Operation::Release, *error);
            (HashSet::new(), *error == OperationError::CommitUnknown)
        }
        None => (HashSet::new(), false),
    }
}

fn take_reconcile(shared: &Shared, progress: &DrainProgress) -> Option<(Instant, Vec<RowState>)> {
    match lock(&progress.reconcile).take() {
        Some(Ok(rows)) => {
            observe_recovery(shared, Operation::Reconcile);
            Some(rows)
        }
        Some(Err(error)) => {
            observe_failure(shared, Operation::Reconcile, error);
            None
        }
        None => None,
    }
}

fn attribution_of(
    claim: &Collected,
    rows: Option<&(Instant, Vec<RowState>)>,
) -> (Option<Attribution>, bool) {
    let Some((arrived, rows)) = rows else {
        return (None, false);
    };
    let row = rows.iter().find(|row| row.id == claim.id);
    let newer = row.is_some_and(|row| row.generation != claim.generation);
    let attribution = attempt::attribute(
        claim.generation,
        claim.attempt,
        row,
        *arrived < claim.deadline,
    );
    (Some(attribution), newer)
}

fn facts_of(claim: &Collected, outcome: Outcome) -> Facts<'_> {
    let intended = claim.intended.as_ref();
    let (summary, failure, retry_in, lost) = match outcome {
        Outcome::Completed
        | Outcome::Retry
        | Outcome::Timeout
        | Outcome::Exhausted
        | Outcome::Permanent => (
            intended.and_then(|item| item.summary.as_deref()),
            intended.and_then(|item| item.failure),
            intended.and_then(|item| item.retry_in),
            None,
        ),
        Outcome::Lost => (None, None, None, Some(LostReason::Release)),
        Outcome::Superseded | Outcome::Released => (None, None, None, None),
    };
    Facts {
        id: claim.id,
        kind: claim.kind,
        attempt: claim.attempt,
        summary,
        failure,
        retry_in,
        lost,
        ran: claim.ran,
    }
}

fn tally(end: &mut DrainEnd, outcome: Outcome) {
    match outcome {
        Outcome::Released => end.released = end.released.saturating_add(1),
        Outcome::Superseded => end.superseded = end.superseded.saturating_add(1),
        Outcome::Lost => end.lost = end.lost.saturating_add(1),
        Outcome::Completed
        | Outcome::Retry
        | Outcome::Timeout
        | Outcome::Exhausted
        | Outcome::Permanent => end.written = end.written.saturating_add(1),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::sync::{Semaphore, watch};
    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    use infra_postgres::TxError;

    use super::{Attempts, OpFailed, Operation, OperationError, backstop};
    use crate::attempt::{Intended, Outcome};
    use crate::claim::Claimed;
    use crate::kind::JobId;
    use crate::lease;

    fn id() -> JobId {
        JobId::parse("01234567-89ab-cdef-fedc-ba9876543210").unwrap()
    }

    fn claimed(generation: i64) -> Claimed {
        let slots = Arc::new(Semaphore::new(4));
        let slot = slots.try_acquire_many_owned(1).unwrap();
        Claimed {
            id: id(),
            generation,
            kind: "sample",
            attempt: 1,
            payload: Vec::new(),
            trace_context: None,
            slot,
        }
    }

    fn abort() -> tokio::task::AbortHandle {
        tokio::spawn(async {}).abort_handle()
    }

    fn intended() -> Intended {
        Intended {
            outcome: Outcome::Completed,
            summary: None,
            failure: None,
            retry_in: None,
            ran: None,
        }
    }

    fn admit(
        attempts: &Attempts,
        generation: i64,
        deadline: Instant,
    ) -> (watch::Receiver<Instant>, CancellationToken) {
        let saved = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&saved);
        attempts
            .admit(claimed(generation), deadline, move |_claimed, handles| {
                *slot.lock().unwrap() = Some((handles.deadline, handles.superseded));
                abort()
            })
            .unwrap();
        saved.lock().unwrap().take().unwrap()
    }

    #[tokio::test]
    async fn admit_hands_the_claim_back_once_closed() {
        let attempts = Attempts::new();
        let _closed = attempts.close();
        let err = attempts.admit(claimed(1), Instant::now(), |_, _| panic!("spawned"));
        let returned = err.unwrap_err();
        assert_eq!(returned.id, id());
        assert_eq!(returned.generation, 1);
    }

    #[tokio::test]
    async fn start_handler_refuses_after_close_without_spawning() {
        let attempts = Attempts::new();
        attempts
            .admit(claimed(1), Instant::now(), |_, _| abort())
            .unwrap();
        let _closed = attempts.close();
        let called = AtomicBool::new(false);
        let handle = abort();
        let started = attempts.start_handler(id(), 1, || {
            called.store(true, Ordering::SeqCst);
            handle
        });
        assert!(!started);
        assert!(!called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn two_generations_of_one_job_are_each_taken_once() {
        let attempts = Attempts::new();
        let now = Instant::now();
        attempts.admit(claimed(1), now, |_, _| abort()).unwrap();
        attempts.admit(claimed(2), now, |_, _| abort()).unwrap();
        assert_eq!(attempts.len(), 2);
        assert!(attempts.take(id(), 1).is_some());
        assert!(attempts.take(id(), 2).is_some());
        assert!(attempts.take(id(), 1).is_none());
        assert!(attempts.take(id(), 2).is_none());
    }

    #[tokio::test]
    async fn take_and_close_give_each_entry_to_one_taker() {
        let attempts = Attempts::new();
        let now = Instant::now();
        attempts.admit(claimed(1), now, |_, _| abort()).unwrap();
        attempts.admit(claimed(2), now, |_, _| abort()).unwrap();
        assert!(attempts.take(id(), 1).is_some());
        let closed = attempts.close();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].generation, 2);
        assert!(attempts.take(id(), 2).is_none());

        let attempts = Attempts::new();
        attempts.admit(claimed(7), now, |_, _| abort()).unwrap();
        let closed = attempts.close();
        assert_eq!(closed.len(), 1);
        assert!(attempts.take(id(), 7).is_none());
        assert!(attempts.close().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn acknowledge_and_supersede_ignore_stale_and_settled() {
        let attempts = Attempts::new();
        let sent = Instant::now();
        let (deadline, superseded) = admit(&attempts, 1, sent);
        attempts.acknowledge(id(), 2, sent + Duration::from_secs(10));
        attempts.supersede(id(), 2);
        assert_eq!(*deadline.borrow(), sent);
        assert!(!superseded.is_cancelled());

        let later = sent + Duration::from_secs(10);
        attempts.acknowledge(id(), 1, later);
        assert_eq!(*deadline.borrow(), lease::local_deadline(later));
        assert_eq!(*deadline.borrow(), later + Duration::from_secs(28));

        attempts.supersede(id(), 1);
        assert!(superseded.is_cancelled());

        let attempts = Attempts::new();
        let (deadline, superseded) = admit(&attempts, 1, sent);
        assert!(attempts.settle(id(), 1, intended()));
        attempts.acknowledge(id(), 1, later);
        attempts.supersede(id(), 1);
        assert_eq!(*deadline.borrow(), sent);
        assert!(!superseded.is_cancelled());
    }

    #[tokio::test]
    async fn settle_returns_false_after_close() {
        let attempts = Attempts::new();
        attempts
            .admit(claimed(1), Instant::now(), |_, _| abort())
            .unwrap();
        let _closed = attempts.close();
        assert!(!attempts.settle(id(), 1, intended()));
    }

    #[test]
    fn op_failed_from_every_tx_error_and_sqlx() {
        let io = || sqlx::Error::Io(std::io::Error::other("reset"));
        assert_eq!(
            OpFailed::from(TxError::Acquire(sqlx::Error::PoolTimedOut)).0,
            OperationError::Acquire
        );
        assert_eq!(
            OpFailed::from(TxError::Begin(sqlx::Error::PoolTimedOut)).0,
            OperationError::Begin
        );
        assert_eq!(
            OpFailed::from(TxError::CommitFailed(io())).0,
            OperationError::Commit
        );
        assert_eq!(
            OpFailed::from(TxError::CommitUnknown(io())).0,
            OperationError::CommitUnknown
        );
        assert_eq!(
            OpFailed::from(sqlx::Error::PoolTimedOut).0,
            OperationError::Statement
        );
    }

    #[tokio::test(start_paused = true)]
    async fn backstop_expiry_before_returned_is_timed_out() {
        let returned = AtomicBool::new(false);
        let result = backstop(&returned, std::future::pending::<Result<(), OpFailed>>()).await;
        assert_eq!(result, Err(OperationError::TimedOut));
    }

    #[tokio::test(start_paused = true)]
    async fn backstop_expiry_after_returned_is_commit_unknown() {
        let returned = AtomicBool::new(false);
        let result = backstop(&returned, async {
            returned.store(true, Ordering::SeqCst);
            std::future::pending::<Result<(), OpFailed>>().await
        })
        .await;
        assert_eq!(result, Err(OperationError::CommitUnknown));
    }

    #[tokio::test]
    async fn backstop_passes_ok_and_err_through() {
        let returned = AtomicBool::new(false);
        let ok = backstop(&returned, async { Ok::<u8, OpFailed>(7) }).await;
        assert_eq!(ok, Ok(7));
        let err = backstop(&returned, async {
            Err::<(), OpFailed>(OpFailed(OperationError::Begin))
        })
        .await;
        assert_eq!(err, Err(OperationError::Begin));
    }

    #[test]
    fn operation_labels() {
        assert_eq!(OperationError::Acquire.as_str(), "acquire");
        assert_eq!(OperationError::Begin.as_str(), "begin");
        assert_eq!(OperationError::Statement.as_str(), "statement");
        assert_eq!(OperationError::Commit.as_str(), "commit");
        assert_eq!(OperationError::CommitUnknown.as_str(), "commit_unknown");
        assert_eq!(OperationError::TimedOut.as_str(), "timed_out");
        assert_eq!(Operation::Claim.as_str(), "claim");
        assert_eq!(Operation::Extend.as_str(), "extend");
        assert_eq!(Operation::Record.as_str(), "record");
        assert_eq!(Operation::Release.as_str(), "release");
        assert_eq!(Operation::Reconcile.as_str(), "reconcile");
        assert_eq!(Operation::Retention.as_str(), "retention");
        assert_eq!(Operation::Sample.as_str(), "sample");
    }
}
