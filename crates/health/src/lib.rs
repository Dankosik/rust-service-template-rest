//! Readiness: a verdict checked in the background and served from cache, plus
//! the drain flag.
//!
//! The readiness endpoints are unauthenticated, so they never reach a
//! dependency themselves; otherwise any caller could turn a probe request
//! into a database round-trip. A background task checks the probes on an
//! interval and publishes the result; readers only look at it.
//!
//! A cached verdict needs an age bound: if the refresher stops or hangs, the
//! last "ready" would stand forever. [`RefreshPolicy::stale_after`] bounds
//! its age. Process roots additionally arm completion progress: once an armed
//! completion gap expires, a late check cannot restore readiness.
//!
//! The state travels through [`tokio::sync::watch`], so a streaming reader
//! can wait for the next publication.
//!
//! Bootstrap owns admission through [`Readiness::refresh`], then runs
//! [`Readiness::refresh_until`] in a tracked background task. Handlers read
//! the cached result with [`ReadinessReader::verdict`]. Teardown
//! calls [`Readiness::start_drain`] before cancelling the refresher.
//!
//! Operators see the refresher through five metrics and bounded log events.
//! `readiness_checks_total` counts completed checks by outcome,
//! `readiness_probe_checks_total` counts each probe's own outcome in every
//! check, and the `readiness_ready` gauge is the published answer.
//! `readiness_last_completed_timestamp_seconds` and `readiness_stale_after_seconds`
//! expose completion freshness even when the refresher stops. The
//! events are `readiness_lost` and `readiness_recovered` for a published
//! flip, `readiness_check_failed` for a failure the threshold absorbed, and
//! `readiness_refresh_late` when a check completes after its predecessor
//! already went stale. `readiness_progress_lost` records the first terminal
//! completion gap of an armed process.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::join_all;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// One dependency check.
///
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
pub struct ProbeError(String);

impl ProbeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

// template:begin grpc:health-owner-dropped
/// The last [`Readiness`] owner was dropped, so no further verdicts will
/// be published.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("readiness owner dropped")]
pub struct OwnerDropped;
// template:end grpc:health-owner-dropped

/// Why the service is not ready.
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

/// Completed checks by outcome. A rate that falls to zero while the process
/// runs is a stopped or hung refresher; the `failed` and `timed_out` rates
/// show a dependency flapping below the failure threshold.
const CHECKS_METRIC: &str = "readiness_checks_total";

/// Each probe's own outcome in every check. The verdict and the log events
/// name only the first failed probe; this counter shows the others, and a
/// probe that fails while the threshold still holds readiness.
const PROBE_CHECKS_METRIC: &str = "readiness_probe_checks_total";

/// `1` while the published verdict is ready, `0` before the first check,
/// while a probe verdict is withdrawn, and from the start of the drain. The
/// refresher and the drain write it; armed process expiry also withdraws it.
/// For generic unarmed users the completion timestamp and stale bound expose
/// expiry without another refresh, while readers refuse the stale verdict.
const READY_METRIC: &str = "readiness_ready";

/// Unix seconds of the last completed check, including failures; zero before
/// completion, NaN if the wall clock cannot supply a positive Unix timestamp.
/// Clock skew/jumps, future timestamps, missing/stale samples or failed scrapes
/// make freshness uncertain. These gauges are not an atomic snapshot and never
/// decide readiness: the reader uses monotonic time.
const LAST_COMPLETED_METRIC: &str = "readiness_last_completed_timestamp_seconds";

/// Maximum fresh age in seconds, interpreted with the completion timestamp.
const STALE_AFTER_METRIC: &str = "readiness_stale_after_seconds";

/// Tokio's timer resolution: a shorter [`RefreshPolicy::interval`] cannot
/// tick faster, and a zero period would panic the ticker.
const MIN_INTERVAL: Duration = Duration::from_millis(1);

/// Cadence and thresholds for the refresher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshPolicy {
    /// Time between checks. Timers resolve to a millisecond, so a shorter
    /// interval, zero included, runs at that pace.
    pub interval: Duration,
    /// Deadline for one check; every probe runs under it at the same time.
    pub probe_budget: Duration,
    /// Failed checks in a row before a ready verdict is withdrawn. Some
    /// readers act on the first unready answer with no threshold of their
    /// own (a gRPC health `Watch` client drops the backend at once), so one
    /// slow round-trip must not evict an instance. A service that is not
    /// ready yet reports a failure at once.
    pub failure_threshold: u32,
}

impl RefreshPolicy {
    /// How old a verdict may get before it is refused.
    ///
    /// `stale_after = probe_budget + period * 3` where
    /// `period = interval.max(probe_budget)`. The leading `probe_budget`
    /// covers one in-flight check after the last stamp. One check finishes
    /// before the next starts, so a probe budget above the interval makes the
    /// loop run at the budget's pace; sizing from the interval alone would expire a verdict that is
    /// being refreshed as fast as it can be. Three periods so an ordinary
    /// missed tick does not flip readiness, finite so a dead refresher cannot
    /// leave a verdict standing forever.
    #[must_use]
    pub fn stale_after(&self) -> Duration {
        let period = self.interval.max(self.probe_budget);
        self.probe_budget + period * 3
    }
}

#[derive(Clone, Debug)]
struct State {
    /// Set by [`Readiness::start_drain`]; wins over any check result.
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
    /// Called only under the watch write lock. Returns newly retained evidence.
    fn expire_progress(&mut self, now: Instant, stale_after: Duration) -> Option<ProgressLoss> {
        if !matches!(self.progress, Progress::Armed) {
            return None;
        }
        let check = self.last_check.as_ref()?;
        if now.duration_since(check.at) <= stale_after {
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
}

/// The latest completed probe round after applying the failure threshold.
#[derive(Clone, Debug)]
struct Check {
    /// Completion time, not start time: readers measure the cache's age.
    at: Instant,
    /// A failure below `failure_threshold` may still be published as ready.
    /// Readers apply drain and staleness before returning this verdict.
    verdict: Result<(), NotReady>,
    /// Failed checks in a row, including absorbed ones.
    consecutive_failures: u32,
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

/// Read side handed to the health handlers and startup admission.
#[derive(Clone, Debug)]
pub struct ReadinessReader {
    rx: watch::Receiver<State>,
    stale_after: Duration,
    // template:begin grpc:health-reader-stale-edge
    /// Timestamp of the check whose stale notification was already delivered.
    /// Prevents the same expired check from waking a stream repeatedly.
    stale_edge_for: Option<Instant>,
    // template:end grpc:health-reader-stale-edge
}

impl Readiness {
    /// A readiness owner over `probes`. Nothing is checked until
    /// [`Readiness::refresh`] or [`Readiness::refresh_until`] runs.
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
            "1 while the published readiness verdict is ready, 0 otherwise."
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

    /// Mark the service as draining. Takes effect on the next read, not
    /// after the next refresh.
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

    /// Require completion progress after successful startup admission.
    /// The admitted check retains its timestamp; arming never renews its age.
    ///
    /// # Errors
    /// Returns the current admission refusal, including a terminal stale gap.
    pub fn arm_progress(&self) -> Result<(), NotReady> {
        let mut result = Ok(());
        let mut newly_expired = None;
        self.tx.send_if_modified(|state| {
            if state.draining || matches!(state.progress, Progress::Stopped) {
                result = Err(NotReady::Draining);
                return false;
            }
            if let Some(loss) = state.progress_loss() {
                result = Err(loss.not_ready());
                return false;
            }
            let Some(check) = &state.last_check else {
                result = Err(NotReady::NotEvaluated);
                return false;
            };
            if let Err(reason) = &check.verdict {
                result = Err(reason.clone());
                return false;
            }
            let changed = matches!(state.progress, Progress::Unarmed);
            state.progress = Progress::Armed;
            newly_expired = state.expire_progress(Instant::now(), self.policy.stale_after());
            if let Some(loss) = newly_expired {
                result = Err(loss.not_ready());
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
    pub fn progress_loss(&self) -> Option<ProgressLoss> {
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
    /// A gap already expired at this stop boundary remains terminal.
    #[must_use]
    pub fn stop_progress(&self) -> Option<ProgressLoss> {
        let mut loss = None;
        let mut newly_expired = None;
        self.tx.send_if_modified(|state| {
            let changed = matches!(state.progress, Progress::Unarmed | Progress::Armed);
            newly_expired = state.stop_progress(self.policy.stale_after());
            loss = state.progress_loss();
            changed
        });
        if let Some(loss) = newly_expired {
            loss.log();
        }
        loss
    }

    /// Wait independently of probes and readers for terminal progress loss.
    /// After stop, keep waiting for cancellation so tracked process-lifetime
    /// observers do not return early. `None` means cancellation only.
    pub async fn wait_for_progress_loss(&self, cancel: CancellationToken) -> Option<ProgressLoss> {
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
                    Progress::Armed => state.last_check.as_ref().map(|check| {
                        check.at + self.policy.stale_after() + Duration::from_nanos(1)
                    }),
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

    /// Check every probe at the same time under one deadline and publish the
    /// verdict after applying the failure threshold. Adding a probe does not
    /// lengthen the refresh, and a slow probe does not spend another probe's
    /// budget. When several probes fail, the verdict names the first one in
    /// registration order.
    ///
    /// Startup admission calls this and then reads
    /// [`ReadinessReader::verdict`] before announcing readiness.
    pub async fn refresh(&self) {
        let observed = self.check_probes().await;
        let completed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .filter(|elapsed| !elapsed.is_zero())
            .map_or(f64::NAN, |elapsed| elapsed.as_secs_f64());
        // Literal labels select the metrics facade's static-label fast path.
        match &observed {
            Ok(()) => metrics::counter!(CHECKS_METRIC, "outcome" => "ok"),
            Err(NotReady::TimedOut { .. }) => {
                metrics::counter!(CHECKS_METRIC, "outcome" => "timed_out")
            }
            Err(_) => metrics::counter!(CHECKS_METRIC, "outcome" => "failed"),
        }
        .increment(1);
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
                    .map(|previous| at.duration_since(previous.at))
                    .filter(|age| *age > stale_after);
            }
            // Written under the lock so it cannot overwrite a concurrent drain.
            let ready = !state.draining && state.progress_loss().is_none() && next.verdict.is_ok();
            metrics::gauge!(READY_METRIC).set(if ready { 1.0 } else { 0.0 });
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
            tracing::warn!(
                %reason,
                consecutive_failures,
                failure_threshold,
                "readiness_check_failed"
            );
        }
        match transition_to_log {
            Some(Ok(())) => tracing::info!("readiness_recovered"),
            Some(Err(reason)) => tracing::warn!(%reason, "readiness_lost"),
            None => {}
        }
    }

    /// Refresh every `policy.interval` until `cancel` fires.
    ///
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
                let mut ticker = tokio::time::interval_at(Instant::now() + interval, interval);
                // Delay, not Burst: a late tick is skipped rather than fired in a
                // catch-up burst that would pile probe work onto a recovering
                // dependency.
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    self.refresh().await;
                }
            })
            .await;
    }

    async fn check_probes(&self) -> Result<(), NotReady> {
        let budget = self.policy.probe_budget;
        let deadline = Instant::now() + budget;
        let outcomes = join_all(self.probes.iter().map(|probe| async move {
            let probe_name = probe.name();
            let outcome = match tokio::time::timeout_at(deadline, probe.check()).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(NotReady::ProbeFailed {
                    probe: probe_name,
                    error,
                }),
                Err(_elapsed) => Err(NotReady::TimedOut {
                    probe: probe_name,
                    budget,
                }),
            };
            let label = match &outcome {
                Ok(()) => "ok",
                Err(NotReady::TimedOut { .. }) => "timed_out",
                Err(_) => "failed",
            };
            metrics::counter!(PROBE_CHECKS_METRIC, "probe" => probe_name, "outcome" => label)
                .increment(1);
            outcome
        }))
        .await;
        outcomes.into_iter().collect()
    }
}

/// Keep a fresh published ready verdict through failures below the threshold.
/// An instance without a fresh ready verdict fails immediately; any success
/// resets the streak and restores readiness. Freshness is checked at completion,
/// including when the previous verdict expires while this round is running.
fn apply_failure_threshold(
    previous: Option<&Check>,
    observed: Result<(), NotReady>,
    failure_threshold: u32,
    at: Instant,
    stale_after: Duration,
) -> Check {
    let consecutive_failures = match (&observed, previous) {
        (Ok(()), _) => 0,
        (Err(_), Some(previous)) => previous.consecutive_failures + 1,
        (Err(_), None) => 1,
    };
    let was_published_ready = previous.is_some_and(|previous| {
        previous.verdict.is_ok() && at.duration_since(previous.at) <= stale_after
    });
    let hold_ready =
        was_published_ready && observed.is_err() && consecutive_failures < failure_threshold;
    Check {
        at,
        verdict: if hold_ready { Ok(()) } else { observed },
        consecutive_failures,
    }
}

/// The new verdict when it flips between ready and not ready. The first
/// check at startup is not a flip; admission logs its own outcome.
fn readiness_transition(previous: Option<&Check>, next: &Check) -> Option<Result<(), NotReady>> {
    let previous = previous?;
    (previous.verdict.is_ok() != next.verdict.is_ok()).then(|| next.verdict.clone())
}

impl ReadinessReader {
    /// The current verdict without touching any dependency.
    ///
    /// A verdict older than [`RefreshPolicy::stale_after`] is refused: a
    /// stopped or hung refresher would otherwise leave its last "ready"
    /// standing.
    ///
    /// # Errors
    ///
    /// Returns why the service is not ready.
    pub fn verdict(&self) -> Result<(), NotReady> {
        let state = self.rx.borrow();
        if state.draining {
            return Err(NotReady::Draining);
        }
        if let Some(loss) = state.progress_loss() {
            return Err(loss.not_ready());
        }
        let Some(check) = &state.last_check else {
            return Err(NotReady::NotEvaluated);
        };
        let age = check.at.elapsed();
        if age > self.stale_after {
            return Err(NotReady::Stale {
                age,
                stale_after: self.stale_after,
            });
        }
        check.verdict.clone()
    }

    // template:begin grpc:health-changed-verdict
    /// Resolve for an unseen publication or once when the current check
    /// reaches its stale boundary.
    ///
    /// A publication may keep the same readiness verdict; it still updates
    /// freshness. The caller decides whether its transport needs a new answer.
    ///
    /// This observes the cached state only; it never checks a probe. A caller
    /// must re-read [`Self::verdict`] after this future resolves, which makes
    /// a publication race use the current monotone drain state.
    ///
    /// # Errors
    ///
    /// [`OwnerDropped`] when the last [`Readiness`] sender is gone.
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
        let stale_at = match checked_at {
            // `verdict` refuses only an age strictly above `stale_after`.
            Some(at) if self.stale_edge_for != Some(at) => {
                at + self.stale_after + Duration::from_nanos(1)
            }
            _ => return self.rx.changed().await.map_err(|_| OwnerDropped),
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
