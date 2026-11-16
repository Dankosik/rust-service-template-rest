//! The readiness endpoints are unauthenticated, so they never reach a
//! dependency themselves; otherwise any caller could turn a probe request
//! into a database round-trip. A background task checks the probes on an
//! interval and publishes the result; readers only look at it.
//!
//! A cached verdict needs an age bound: if the refresher stops or hangs, the
//! last "ready" would stand forever. [`RefreshPolicy::stale_after`] bounds
//! its age. Process roots additionally arm completion progress: once an armed
//! completion gap expires, a late check cannot restore readiness.

use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use compact_str::CompactString;
use futures_util::future::join_all;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// The probes of one check run together under one deadline, `probe_budget`.
/// When it passes, every [`Probe::check`] future still running is dropped
/// and counted as timed out. Implementations must not detach work that would
/// outlive that cancellation; a check that ignores cancellation holds the
/// whole refresh past its budget.
///
/// `async_trait` boxes the future so this trait stays dyn compatible for
/// `Box<dyn Probe>`. A trait with a native `async fn` is not dyn compatible
/// on the pinned toolchain, so removing the attribute is a public shape
/// change, not a cleanup.
#[async_trait::async_trait]
pub trait Probe: Send + Sync + 'static {
    /// Bounded label used in log lines and verdict messages.
    fn name(&self) -> &'static str;
    async fn check(&self) -> Result<(), ProbeError>;
}

/// Why one probe failed: a short, bounded class for logs and startup errors.
/// The HTTP and gRPC health answers never show it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProbeError(CompactString);

impl ProbeError {
    #[must_use]
    pub const fn from_static(message: &'static str) -> Self {
        Self(CompactString::const_new(message))
    }

    #[must_use]
    pub fn format(args: fmt::Arguments<'_>) -> Self {
        let mut text = CompactString::default();
        let _ = fmt::Write::write_fmt(&mut text, args);
        Self(text)
    }
}

// template:begin grpc:health-owner-dropped
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("readiness owner dropped")]
pub struct OwnerDropped;
// template:end grpc:health-owner-dropped

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NotReady {
    #[error("service is draining")]
    Draining,
    #[error("readiness has not been evaluated yet")]
    NotEvaluated,
    #[error("readiness verdict is stale: last evaluated {age:?} ago, stale_after {stale_after:?}")]
    Stale {
        age: Duration,
        stale_after: Duration,
    },
    #[error("{probe} probe exceeded the {budget:?} readiness budget")]
    TimedOut {
        probe: &'static str,
        budget: Duration,
    },
    #[error("{probe} probe failed: {error}")]
    ProbeFailed {
        probe: &'static str,
        error: ProbeError,
    },
}

/// Why [`Readiness::arm_progress`] refused to arm.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ArmError {
    #[error(transparent)]
    Refused(NotReady),
    /// An armed completion gap already expired; it outranks any refusal.
    #[error("readiness progress was lost")]
    ProgressLost,
}

/// A failed probe round as stored in [`Check`]; [`NotReady`] adds the
/// reader-side refusals.
#[derive(Clone, Debug)]
enum ProbeFailure {
    TimedOut {
        probe: &'static str,
        budget: Duration,
    },
    Failed {
        probe: &'static str,
        error: ProbeError,
    },
}

impl From<ProbeFailure> for NotReady {
    fn from(failure: ProbeFailure) -> Self {
        match failure {
            ProbeFailure::TimedOut { probe, budget } => Self::TimedOut { probe, budget },
            ProbeFailure::Failed { probe, error } => Self::ProbeFailed { probe, error },
        }
    }
}

fn outcome_label(outcome: &Result<(), ProbeFailure>) -> &'static str {
    match outcome {
        Ok(()) => "ok",
        Err(ProbeFailure::TimedOut { .. }) => "timed_out",
        Err(ProbeFailure::Failed { .. }) => "failed",
    }
}

const CHECKS_METRIC: &str = "readiness_checks_total";

const PROBE_CHECKS_METRIC: &str = "readiness_probe_checks_total";

const READY_METRIC: &str = "readiness_ready";

const LAST_COMPLETED_METRIC: &str = "readiness_last_completed_timestamp_seconds";

const STALE_AFTER_METRIC: &str = "readiness_stale_after_seconds";

/// Tokio's timer resolution: a shorter [`RefreshPolicy::interval`] cannot
/// tick faster, and a zero period would panic the ticker.
const MIN_INTERVAL: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshPolicy {
    /// Timers resolve to a millisecond, so a shorter interval, zero
    /// included, runs at that pace.
    pub interval: Duration,
    /// Deadline shared by all probes of one check.
    pub probe_budget: Duration,
    /// Some readers act on the first unready answer with no threshold of their
    /// own (a gRPC health `Watch` client drops the backend at once), so one
    /// slow round-trip must not evict an instance. A service that is not
    /// ready yet reports a failure at once.
    pub failure_threshold: NonZeroU32,
}

impl RefreshPolicy {
    /// `stale_after = probe_budget + period * 3` where
    /// `period = interval.max(1ms).max(probe_budget)`, saturated to the maximum
    /// duration. The leading
    /// `probe_budget` covers one in-flight check after the last stamp. The
    /// loop never runs faster than the timer resolution, and one check
    /// finishes before the next starts, so a probe budget above the interval
    /// makes the loop run at the budget's pace; sizing from the nominal
    /// interval alone would expire a verdict that is being refreshed as fast
    /// as it can be. Three periods so an ordinary missed tick does not flip
    /// readiness, finite so a dead refresher cannot leave a verdict standing
    /// forever.
    #[must_use]
    pub fn stale_after(&self) -> Duration {
        let period = self.interval.max(MIN_INTERVAL).max(self.probe_budget);
        self.probe_budget.saturating_add(period.saturating_mul(3))
    }
}

#[derive(Clone, Debug)]
struct State {
    /// Wins over any check result.
    draining: bool,
    last_check: Option<Check>,
    progress: Progress,
}

#[derive(Clone, Copy, Debug)]
enum Progress {
    Unarmed,
    Armed,
    Expired(ProgressLoss),
    Stopped,
}

/// Retained evidence that an armed process missed its completion bound.
/// Later checks and shutdown cannot erase this first observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgressLoss {
    last_completed_at: Instant,
    observed_at: Instant,
    stale_after: Duration,
}

impl ProgressLoss {
    fn not_ready(self) -> NotReady {
        NotReady::Stale {
            age: self.observed_at.duration_since(self.last_completed_at),
            stale_after: self.stale_after,
        }
    }

    fn log(self) {
        tracing::error!(
            task = "readiness_progress",
            age_seconds = self
                .observed_at
                .duration_since(self.last_completed_at)
                .as_secs_f64(),
            bound_seconds = self.stale_after.as_secs_f64(),
            "readiness_progress_lost"
        );
    }
}

impl State {
    /// Called only under the watch write lock.
    fn expire_progress(&mut self, now: Instant, stale_after: Duration) -> Option<ProgressLoss> {
        if !matches!(self.progress, Progress::Armed) {
            return None;
        }
        let check = self.last_check.as_ref()?;
        if check.is_fresh_at(now, stale_after) {
            return None;
        }
        let loss = ProgressLoss {
            last_completed_at: check.at,
            observed_at: now,
            stale_after,
        };
        self.progress = Progress::Expired(loss);
        metrics::gauge!(READY_METRIC).set(0.0);
        Some(loss)
    }

    fn progress_loss(&self) -> Option<ProgressLoss> {
        match self.progress {
            Progress::Expired(loss) => Some(loss),
            _ => None,
        }
    }

    fn stop_progress(&mut self, stale_after: Duration) -> Option<ProgressLoss> {
        let newly_expired = self.expire_progress(Instant::now(), stale_after);
        if self.progress_loss().is_none() {
            self.progress = Progress::Stopped;
        }
        newly_expired
    }

    fn verdict(&self, now: Instant, stale_after: Duration) -> Result<(), NotReady> {
        if self.draining {
            return Err(NotReady::Draining);
        }
        if let Some(loss) = self.progress_loss() {
            return Err(loss.not_ready());
        }
        let Some(check) = &self.last_check else {
            return Err(NotReady::NotEvaluated);
        };
        if !check.is_fresh_at(now, stale_after) {
            return Err(NotReady::Stale {
                age: now.duration_since(check.at),
                stale_after,
            });
        }
        check.verdict.clone().map_err(NotReady::from)
    }
}

#[derive(Clone, Debug)]
struct Check {
    /// Completion time, not start time: readers measure the cache's age.
    at: Instant,
    /// A failure below `failure_threshold` may still be published as ready.
    /// Readers apply drain and staleness before returning this verdict.
    verdict: Result<(), ProbeFailure>,
    /// Failed checks in a row, including absorbed ones.
    consecutive_failures: u32,
}

impl Check {
    /// Fresh up to and including exactly `stale_after`.
    fn is_fresh_at(&self, now: Instant, stale_after: Duration) -> bool {
        now.duration_since(self.at) <= stale_after
    }
}

#[derive(Clone)]
pub struct Readiness {
    tx: watch::Sender<State>,
    probes: Arc<[Box<dyn Probe>]>,
    policy: RefreshPolicy,
}

impl fmt::Debug for Readiness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.probes.iter().map(|probe| probe.name()).collect();
        f.debug_struct("Readiness")
            .field("probes", &names)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct ReadinessReader {
    rx: watch::Receiver<State>,
    stale_after: Duration,
    // template:begin grpc:health-reader-stale-edge
    /// Check whose stale notification was already delivered, so one expiry
    /// wakes a stream once.
    stale_edge_for: Option<Instant>,
    // template:end grpc:health-reader-stale-edge
}

impl Readiness {
    /// Nothing is checked until [`Readiness::refresh`] or
    /// [`Readiness::refresh_until`] runs.
    #[must_use]
    pub fn new(probes: Vec<Box<dyn Probe>>, policy: RefreshPolicy) -> Self {
        metrics::describe_counter!(
            CHECKS_METRIC,
            metrics::Unit::Count,
            "Completed readiness checks by outcome."
        );
        metrics::describe_counter!(
            PROBE_CHECKS_METRIC,
            metrics::Unit::Count,
            "Completed readiness probe checks by probe and outcome."
        );
        metrics::describe_gauge!(
            READY_METRIC,
            "1 while the last completed check is ready and the service is neither draining nor past an armed progress bound, 0 otherwise; readiness_last_completed_timestamp_seconds shows an unarmed refresher that stopped."
        );
        metrics::describe_gauge!(
            LAST_COMPLETED_METRIC,
            metrics::Unit::Seconds,
            "Last completed readiness check as Unix seconds; 0 before completion, NaN for an unusable clock."
        );
        metrics::describe_gauge!(
            STALE_AFTER_METRIC,
            metrics::Unit::Seconds,
            "Maximum fresh age of a completed readiness check in seconds."
        );
        metrics::gauge!(LAST_COMPLETED_METRIC).set(0.0);
        metrics::gauge!(STALE_AFTER_METRIC).set(policy.stale_after().as_secs_f64());
        // Not ready until the first check: the series exists from startup.
        metrics::gauge!(READY_METRIC).set(0.0);
        Self {
            tx: watch::Sender::new(State {
                draining: false,
                last_check: None,
                progress: Progress::Unarmed,
            }),
            probes: probes.into(),
            policy,
        }
    }

    #[must_use]
    pub fn reader(&self) -> ReadinessReader {
        ReadinessReader {
            rx: self.tx.subscribe(),
            stale_after: self.policy.stale_after(),
            // template:begin grpc:health-reader-stale-edge-init
            stale_edge_for: None,
            // template:end grpc:health-reader-stale-edge-init
        }
    }

    /// Takes effect on the next read, not after the next refresh.
    pub fn start_drain(&self) {
        let mut newly_expired = None;
        self.tx.send_if_modified(|state| {
            let changed = !state.draining;
            newly_expired = state.stop_progress(self.policy.stale_after());
            state.draining = true;
            // Written under the lock, like the refresher's write, so a check
            // that completes during the drain cannot leave a `1` behind.
            metrics::gauge!(READY_METRIC).set(0.0);
            changed || newly_expired.is_some()
        });
        if let Some(loss) = newly_expired {
            loss.log();
        }
    }

    /// Check every probe and return the verdict a reader would now see.
    pub async fn admit(&self) -> Result<(), NotReady> {
        self.refresh().await;
        self.tx
            .borrow()
            .verdict(Instant::now(), self.policy.stale_after())
    }

    /// The admitted check retains its timestamp; arming never renews its age.
    pub fn arm_progress(&self) -> Result<(), ArmError> {
        let mut result = Ok(());
        let mut newly_expired = None;
        self.tx.send_if_modified(|state| {
            if state.progress_loss().is_some() {
                result = Err(ArmError::ProgressLost);
                return false;
            }
            if state.draining || matches!(state.progress, Progress::Stopped) {
                result = Err(ArmError::Refused(NotReady::Draining));
                return false;
            }
            let Some(check) = &state.last_check else {
                result = Err(ArmError::Refused(NotReady::NotEvaluated));
                return false;
            };
            if let Err(reason) = &check.verdict {
                result = Err(ArmError::Refused(reason.clone().into()));
                return false;
            }
            let changed = matches!(state.progress, Progress::Unarmed);
            state.progress = Progress::Armed;
            newly_expired = state.expire_progress(Instant::now(), self.policy.stale_after());
            if newly_expired.is_some() {
                result = Err(ArmError::ProgressLost);
            }
            changed || newly_expired.is_some()
        });
        if let Some(loss) = newly_expired {
            loss.log();
        }
        result
    }

    /// Observe and retain an expired armed completion gap under the publication
    /// lock. Generic unarmed readers do not acquire a process obligation.
    #[must_use]
    pub fn progress_lost(&self) -> bool {
        self.progress_loss().is_some()
    }

    fn progress_loss(&self) -> Option<ProgressLoss> {
        let mut loss = None;
        let mut newly_expired = None;
        self.tx.send_if_modified(|state| {
            newly_expired = state.expire_progress(Instant::now(), self.policy.stale_after());
            loss = state.progress_loss();
            newly_expired.is_some()
        });
        if let Some(loss) = newly_expired {
            loss.log();
        }
        loss
    }

    /// Close future progress obligations before cancelling process work.
    /// A gap already expired at this stop boundary remains terminal and is
    /// still reported by [`Self::progress_lost`].
    pub fn stop_progress(&self) {
        let mut newly_expired = None;
        self.tx.send_if_modified(|state| {
            let changed = matches!(state.progress, Progress::Unarmed | Progress::Armed);
            newly_expired = state.stop_progress(self.policy.stale_after());
            changed
        });
        if let Some(loss) = newly_expired {
            loss.log();
        }
    }

    /// Call `on_loss` once for a terminal progress loss, independently of
    /// probes and readers, then return only when `cancel` fires, so a tracked
    /// process-lifetime observer never ends early, after stop included.
    pub async fn observe_progress(&self, cancel: CancellationToken, on_loss: impl FnOnce()) {
        if self.wait_for_progress_loss(&cancel).await.is_some() {
            on_loss();
            cancel.cancelled().await;
        }
    }

    /// `None` means cancellation only.
    async fn wait_for_progress_loss(&self, cancel: &CancellationToken) -> Option<ProgressLoss> {
        let mut changed = self.tx.subscribe();
        loop {
            if let Some(loss) = self.progress_loss() {
                return Some(loss);
            }
            // Drop the read borrow before any wait. Taking the current version
            // here also closes a publication race with the expiry inspection.
            let deadline = {
                let state = changed.borrow_and_update();
                if let Some(loss) = state.progress_loss() {
                    return Some(loss);
                }
                match state.progress {
                    Progress::Armed => state
                        .last_check
                        .as_ref()
                        .and_then(|check| stale_deadline(check.at, self.policy.stale_after())),
                    _ => None,
                }
            };
            let wait_for_event = async {
                match deadline {
                    Some(deadline) => {
                        let _ = tokio::time::timeout_at(deadline, changed.changed()).await;
                    }
                    None => {
                        let _ = changed.changed().await;
                    }
                }
            };
            cancel.run_until_cancelled(wait_for_event).await?;
        }
    }

    /// Probes run concurrently, so a slow probe does not spend another's budget.
    /// When several fail, the verdict names the first in registration order.
    pub async fn refresh(&self) {
        let observed = self.check_probes().await;
        let completed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .filter(|elapsed| !elapsed.is_zero())
            .map_or(f64::NAN, |elapsed| elapsed.as_secs_f64());
        metrics::counter!(CHECKS_METRIC, "outcome" => outcome_label(&observed)).increment(1);
        let failure_threshold = self.policy.failure_threshold;
        let stale_after = self.policy.stale_after();
        let mut transition_to_log = None;
        let mut absorbed_failure = None;
        let mut late_by = None;
        let mut newly_expired = None;
        // Publish every completed round, even when readiness stays the same:
        // its timestamp is the refresher's heartbeat for staleness detection.
        self.tx.send_modify(|state| {
            // Sample only after obtaining publication custody: waiting for the
            // lock cannot backdate a completion across the progress boundary.
            let at = Instant::now();
            newly_expired = state.expire_progress(at, stale_after);
            let previous = state.last_check.as_ref();
            let failure = observed.as_ref().err().cloned();
            let next =
                apply_failure_threshold(previous, observed, failure_threshold, at, stale_after);
            // While draining, readers are told "draining" whatever the probes say.
            if !state.draining && state.progress_loss().is_none() {
                transition_to_log = readiness_transition(previous, &next);
                // A failure the threshold absorbed changes nothing readers
                // see, so this record is its only trace.
                if next.verdict.is_ok() {
                    absorbed_failure = failure.map(|reason| (reason, next.consecutive_failures));
                }
                // Readers refused the previous verdict as stale before this
                // round replaced it; nothing else reports that gap.
                late_by = previous
                    .filter(|previous| !previous.is_fresh_at(at, stale_after))
                    .map(|previous| at.duration_since(previous.at));
            }
            // Written under the lock so it cannot overwrite a concurrent drain.
            let ready = !state.draining && state.progress_loss().is_none() && next.verdict.is_ok();
            metrics::gauge!(READY_METRIC).set(f64::from(ready));
            metrics::gauge!(LAST_COMPLETED_METRIC).set(completed_at);
            state.last_check = Some(next);
        });
        // Logged after the write lock is released so readers never wait on it.
        if let Some(loss) = newly_expired {
            loss.log();
        }
        if let Some(age) = late_by {
            tracing::warn!(?age, ?stale_after, "readiness_refresh_late");
        }
        if let Some((reason, consecutive_failures)) = absorbed_failure {
            let reason = NotReady::from(reason);
            tracing::warn!(
                %reason,
                consecutive_failures,
                failure_threshold,
                "readiness_check_failed"
            );
        }
        match transition_to_log {
            Some(Ok(())) => tracing::info!("readiness_recovered"),
            Some(Err(reason)) => {
                let reason = NotReady::from(reason);
                tracing::warn!(%reason, "readiness_lost");
            }
            None => {}
        }
    }

    /// Checks at once unless startup admission already did. Cancel also drops
    /// an in-flight check, so the tracker join does not wait for
    /// `probe_budget` before dependencies close. An already cancelled token
    /// never checks a probe.
    pub async fn refresh_until(&self, cancel: CancellationToken) {
        let _ = cancel
            .run_until_cancelled(async {
                if self.tx.borrow().last_check.is_none() {
                    self.refresh().await;
                }
                let interval = self.policy.interval.max(MIN_INTERVAL);
                let mut next = timer_deadline(Instant::now(), interval);
                while let Some(deadline) = next {
                    tokio::time::sleep_until(deadline).await;
                    // Space the next check from this tick, including after a delay.
                    next = timer_deadline(Instant::now(), interval);
                    self.refresh().await;
                }
                // The next tick is beyond the clock's range. Remain cancellable.
                std::future::pending::<()>().await;
            })
            .await;
    }

    async fn check_probes(&self) -> Result<(), ProbeFailure> {
        let budget = self.policy.probe_budget;
        let deadline = timer_deadline(Instant::now(), budget);
        let outcomes = join_all(self.probes.iter().map(|probe| async move {
            let probe_name = probe.name();
            let reply = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, probe.check()).await.ok(),
                None => Some(probe.check().await),
            };
            let outcome = match reply {
                Some(Ok(())) => Ok(()),
                Some(Err(error)) => Err(ProbeFailure::Failed {
                    probe: probe_name,
                    error,
                }),
                None => Err(ProbeFailure::TimedOut {
                    probe: probe_name,
                    budget,
                }),
            };
            metrics::counter!(
                PROBE_CHECKS_METRIC,
                "probe" => probe_name,
                "outcome" => outcome_label(&outcome)
            )
            .increment(1);
            outcome
        }))
        .await;
        outcomes.into_iter().collect()
    }
}

fn stale_deadline(checked_at: Instant, stale_after: Duration) -> Option<Instant> {
    // The verdict remains fresh at exactly stale_after.
    timer_deadline(
        checked_at,
        stale_after.checked_add(Duration::from_nanos(1))?,
    )
}

fn timer_deadline(start: Instant, delay: Duration) -> Option<Instant> {
    let deadline = start.checked_add(delay)?;
    // Tokio rounds deadlines up to milliseconds before registering the timer.
    deadline.checked_add(Duration::from_millis(1))?;
    Some(deadline)
}

/// Keep a fresh published ready verdict through failures below the threshold.
/// An instance without a fresh ready verdict fails immediately; any success
/// resets the streak and restores readiness. Freshness is checked at completion,
/// including when the previous verdict expires while this round is running.
fn apply_failure_threshold(
    previous: Option<&Check>,
    observed: Result<(), ProbeFailure>,
    failure_threshold: NonZeroU32,
    at: Instant,
    stale_after: Duration,
) -> Check {
    let consecutive_failures = match (&observed, previous) {
        (Ok(()), _) => 0,
        (Err(_), Some(previous)) => previous.consecutive_failures.saturating_add(1),
        (Err(_), None) => 1,
    };
    let was_published_ready = previous
        .is_some_and(|previous| previous.verdict.is_ok() && previous.is_fresh_at(at, stale_after));
    let hold_ready =
        was_published_ready && observed.is_err() && consecutive_failures < failure_threshold.get();
    Check {
        at,
        verdict: if hold_ready { Ok(()) } else { observed },
        consecutive_failures,
    }
}

/// The new verdict when it flips between ready and not ready. The first
/// check at startup is not a flip; admission logs its own outcome.
fn readiness_transition(
    previous: Option<&Check>,
    next: &Check,
) -> Option<Result<(), ProbeFailure>> {
    let previous = previous?;
    (previous.verdict.is_ok() != next.verdict.is_ok()).then(|| next.verdict.clone())
}

impl ReadinessReader {
    /// A verdict older than [`RefreshPolicy::stale_after`] is refused.
    pub fn verdict(&self) -> Result<(), NotReady> {
        self.rx.borrow().verdict(Instant::now(), self.stale_after)
    }

    // template:begin grpc:health-changed-verdict
    /// Resolves for an unseen publication, which may carry the same verdict, or once
    /// at the current check's stale boundary. A caller must re-read
    /// [`Self::verdict`] afterwards so a publication race sees the current drain state.
    ///
    /// Cancel safe: the stale edge is recorded only when it is delivered, so
    /// dropping this future in `select!` loses no event.
    pub async fn wait_for_verdict_event(&mut self) -> Result<(), OwnerDropped> {
        if self.rx.has_changed().map_err(|_| OwnerDropped)? {
            return self.rx.changed().await.map_err(|_| OwnerDropped);
        }
        // Draining is monotone and never goes stale; nothing goes stale
        // before the first check.
        // Release watch's read guard before waiting: a retained guard would
        // block refresh and drain from publishing their state.
        let checked_at = {
            let state = self.rx.borrow();
            state
                .last_check
                .as_ref()
                .filter(|_| !state.draining)
                .map(|check| check.at)
        };
        // Each check crosses its stale edge once; after that only a
        // publication wakes the caller.
        let Some(stale_at) = checked_at
            .filter(|at| self.stale_edge_for != Some(*at))
            .and_then(|at| stale_deadline(at, self.stale_after))
        else {
            return self.rx.changed().await.map_err(|_| OwnerDropped);
        };
        match tokio::time::timeout_at(stale_at, self.rx.changed()).await {
            Ok(changed) => changed.map_err(|_| OwnerDropped),
            Err(_elapsed) => {
                self.stale_edge_for = checked_at;
                Ok(())
            }
        }
    }
    // template:end grpc:health-changed-verdict
}

#[cfg(test)]
mod tests;
