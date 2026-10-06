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
//! its age.
//!
//! The state travels through [`tokio::sync::watch`], so a streaming reader
//! can wait for the next publication.
//!
//! Bootstrap owns admission through [`Readiness::refresh`], then runs
//! [`Readiness::refresh_until`] in a tracked background task. Handlers read
//! the cached result with [`ReadinessReader::verdict`]. Teardown
//! calls [`Readiness::start_drain`] before cancelling the refresher.
//!
//! Operators see the refresher through five metrics and four log events.
//! `readiness_checks_total` counts completed checks by outcome,
//! `readiness_probe_checks_total` counts each probe's own outcome in every
//! check, and the `readiness_ready` gauge is the published answer.
//! `readiness_last_completed_timestamp_seconds` and `readiness_stale_after_seconds`
//! expose completion freshness even when the refresher stops. The
//! events are `readiness_lost` and `readiness_recovered` for a published
//! flip, `readiness_check_failed` for a failure the threshold absorbed, and
//! `readiness_refresh_late` when a check completes after its predecessor
//! already went stale.

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
    /// Resolve `Ok` when the dependency can serve requests.
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
/// refresher and the drain write it, so a stopped refresher leaves the last
/// value standing; readers refuse that verdict as stale. The completion
/// timestamp and stale bound expose that expiry without another refresh.
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

/// What readers see.
#[derive(Clone, Debug)]
struct State {
    /// Set by [`Readiness::start_drain`]; wins over any check result.
    draining: bool,
    /// `None` until the first check completes.
    last_check: Option<Check>,
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

/// Readiness owner: holds the probes and publishes their verdict.
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
            }),
            probes: probes.into(),
            policy,
        }
    }

    /// A reader over the current and future verdicts.
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
        self.tx.send_if_modified(|state| {
            let changed = !state.draining;
            state.draining = true;
            // Written under the lock, like the refresher's write, so a check
            // that completes during the drain cannot leave a `1` behind.
            metrics::gauge!(READY_METRIC).set(0.0);
            changed
        });
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
        let at = Instant::now();
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
        // Publish every completed round, even when readiness stays the same:
        // its timestamp is the refresher's heartbeat for staleness detection.
        self.tx.send_modify(|state| {
            let previous = state.last_check.as_ref();
            let failure = observed.as_ref().err().cloned();
            let next =
                apply_failure_threshold(previous, observed, failure_threshold, at, stale_after);
            // While draining, readers are told "draining" whatever the probes say.
            if !state.draining {
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
            let ready = !state.draining && next.verdict.is_ok();
            metrics::gauge!(READY_METRIC).set(if ready { 1.0 } else { 0.0 });
            metrics::gauge!(LAST_COMPLETED_METRIC).set(completed_at);
            state.last_check = Some(next);
        });
        // Logged after the write lock is released so readers never wait on it.
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
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use super::*;

    struct Flaky {
        healthy: Arc<AtomicBool>,
        calls: Arc<AtomicU32>,
    }

    #[async_trait::async_trait]
    impl Probe for Flaky {
        fn name(&self) -> &'static str {
            "flaky"
        }
        async fn check(&self) -> Result<(), ProbeError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.healthy.load(Ordering::Relaxed) {
                Ok(())
            } else {
                Err(ProbeError::new("connection refused"))
            }
        }
    }

    struct Hanging;

    #[async_trait::async_trait]
    impl Probe for Hanging {
        fn name(&self) -> &'static str {
            "hanging"
        }
        async fn check(&self) -> Result<(), ProbeError> {
            std::future::pending().await
        }
    }

    fn policy() -> RefreshPolicy {
        RefreshPolicy {
            interval: Duration::from_millis(50),
            probe_budget: Duration::from_millis(20),
            failure_threshold: 3,
        }
    }

    fn flaky_probe(healthy: bool) -> (Box<dyn Probe>, Arc<AtomicBool>, Arc<AtomicU32>) {
        let flag = Arc::new(AtomicBool::new(healthy));
        let calls = Arc::new(AtomicU32::new(0));
        let probe = Box::new(Flaky {
            healthy: flag.clone(),
            calls: calls.clone(),
        });
        (probe, flag, calls)
    }

    fn flaky(healthy: bool) -> (Readiness, Arc<AtomicBool>, Arc<AtomicU32>) {
        let (probe, flag, calls) = flaky_probe(healthy);
        (Readiness::new(vec![probe], policy()), flag, calls)
    }

    #[tokio::test]
    async fn fails_closed_before_first_evaluation() {
        let (readiness, _, _) = flaky(true);
        assert_eq!(readiness.reader().verdict(), Err(NotReady::NotEvaluated));
    }

    #[tokio::test]
    async fn refresh_seeds_the_verdict_and_reads_do_not_probe() {
        let (readiness, _, calls) = flaky(true);
        readiness.refresh().await;
        let reader = readiness.reader();
        for _ in 0..10 {
            assert_eq!(reader.verdict(), Ok(()));
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "verdict() must not run probes"
        );
    }

    #[tokio::test]
    async fn cloned_owner_preserves_updates_without_readers() {
        let (readiness, _, _) = flaky(true);
        let other = readiness.clone();
        readiness.refresh().await;
        assert_eq!(other.reader().verdict(), Ok(()));

        drop(readiness);
        other.start_drain();
        assert_eq!(other.reader().verdict(), Err(NotReady::Draining));
    }

    #[tokio::test]
    async fn healthy_instance_survives_blips_below_the_threshold() {
        let (readiness, flag, _) = flaky(true);
        readiness.refresh().await;
        flag.store(false, Ordering::Relaxed);
        readiness.refresh().await;
        assert_eq!(readiness.reader().verdict(), Ok(()), "1 of 3 failures");
        readiness.refresh().await;
        assert_eq!(readiness.reader().verdict(), Ok(()), "2 of 3 failures");
        readiness.refresh().await;
        assert!(
            matches!(
                readiness.reader().verdict(),
                Err(NotReady::ProbeFailed { probe: "flaky", .. })
            ),
            "3 of 3 failures flips"
        );
        flag.store(true, Ordering::Relaxed);
        readiness.refresh().await;
        assert_eq!(readiness.reader().verdict(), Ok(()), "one success recovers");
    }

    #[tokio::test(start_paused = true)]
    async fn failed_refresh_only_absorbs_a_previous_ready_that_is_fresh_at_completion() {
        for previously_absorbed in [false, true] {
            for expired in [false, true] {
                let (readiness, flag, _) = flaky(true);
                readiness.refresh().await;
                flag.store(false, Ordering::Relaxed);
                if previously_absorbed {
                    readiness.refresh().await;
                }
                let age = policy().stale_after()
                    + if expired {
                        Duration::from_nanos(1)
                    } else {
                        Duration::ZERO
                    };
                tokio::time::advance(age).await;
                // No read is needed to mark the prior publication stale.
                readiness.refresh().await;
                let verdict = readiness.reader().verdict();
                if expired {
                    assert!(
                        matches!(verdict, Err(NotReady::ProbeFailed { .. })),
                        "{verdict:?}"
                    );
                    readiness.refresh().await;
                    assert!(readiness.reader().verdict().is_err());
                    flag.store(true, Ordering::Relaxed);
                    readiness.refresh().await;
                    assert_eq!(readiness.reader().verdict(), Ok(()));
                    flag.store(false, Ordering::Relaxed);
                    readiness.refresh().await;
                    assert_eq!(
                        readiness.reader().verdict(),
                        Ok(()),
                        "success resets the streak"
                    );
                } else {
                    assert_eq!(verdict, Ok(()), "equality remains fresh");
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_round_starting_fresh_cannot_revive_readiness_after_expiry() {
        let (probe, flag, _) = flaky_probe(true);
        let readiness = Readiness::new(
            vec![
                probe,
                Box::new(Slow {
                    name: "slow",
                    delay: policy().probe_budget / 2,
                }),
            ],
            policy(),
        );
        readiness.refresh().await;
        tokio::time::advance(policy().stale_after()).await;
        assert_eq!(readiness.reader().verdict(), Ok(()));
        flag.store(false, Ordering::Relaxed);
        readiness.refresh().await;
        assert!(matches!(
            readiness.reader().verdict(),
            Err(NotReady::ProbeFailed { .. })
        ));
    }

    #[tokio::test]
    async fn never_healthy_instance_fails_immediately() {
        let (readiness, _, _) = flaky(false);
        readiness.refresh().await;
        assert!(matches!(
            readiness.reader().verdict(),
            Err(NotReady::ProbeFailed { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hanging_probe_times_out_under_its_own_name() {
        let (healthy, _, _) = flaky_probe(true);
        let readiness = Readiness::new(vec![healthy, Box::new(Hanging)], policy());
        readiness.refresh().await;
        let verdict = readiness.reader().verdict();
        assert!(
            matches!(
                verdict,
                Err(NotReady::TimedOut {
                    probe: "hanging",
                    ..
                })
            ),
            "{verdict:?}"
        );
    }

    #[tokio::test]
    async fn draining_wins_over_a_healthy_verdict_immediately() {
        let (readiness, _, _) = flaky(true);
        readiness.refresh().await;
        let reader = readiness.reader();
        readiness.start_drain();
        assert_eq!(reader.verdict(), Err(NotReady::Draining));
    }

    #[tokio::test(start_paused = true)]
    async fn a_verdict_nobody_refreshes_goes_stale() {
        let (readiness, _, _) = flaky(true);
        readiness.refresh().await;
        let reader = readiness.reader();
        assert_eq!(reader.verdict(), Ok(()));

        tokio::time::advance(policy().stale_after() + Duration::from_millis(1)).await;
        assert!(
            matches!(reader.verdict(), Err(NotReady::Stale { .. })),
            "{:?}",
            reader.verdict()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn refreshes_on_the_interval_and_stops_on_cancel() {
        let (readiness, flag, _) = flaky(true);
        let cancel = CancellationToken::new();
        let mut published = readiness.tx.subscribe();
        let refresher = tokio::spawn({
            let readiness = readiness.clone();
            let cancel = cancel.clone();
            async move { readiness.refresh_until(cancel).await }
        });
        published.changed().await.unwrap();
        let reader = readiness.reader();

        for _ in 0..10 {
            tokio::time::advance(policy().interval).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(
            reader.verdict(),
            Ok(()),
            "a running refresher keeps the verdict fresh"
        );

        flag.store(false, Ordering::Relaxed);
        for _ in 0..3 {
            tokio::time::advance(policy().interval).await;
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            reader.verdict(),
            Err(NotReady::ProbeFailed { .. })
        ));

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), refresher)
            .await
            .expect("refresh_until must return on cancel")
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_until_stops_while_an_evaluation_is_in_flight() {
        let readiness = Readiness::new(vec![Box::new(Hanging)], policy());
        let cancel = CancellationToken::new();
        let refresher = tokio::spawn({
            let readiness = readiness.clone();
            let cancel = cancel.clone();
            async move { readiness.refresh_until(cancel).await }
        });
        tokio::task::yield_now().await;
        cancel.cancel();
        tokio::task::yield_now().await;
        assert!(
            refresher.is_finished(),
            "cancel must drop the in-flight refresh, not wait for probe_budget"
        );
        refresher.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn an_already_cancelled_refresher_never_checks_a_probe() {
        let (readiness, _, calls) = flaky(true);
        let cancel = CancellationToken::new();
        cancel.cancel();
        readiness.refresh_until(cancel).await;
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(readiness.reader().verdict(), Err(NotReady::NotEvaluated));
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_interval_refreshes_at_the_timer_resolution() {
        let (probe, _, calls) = flaky_probe(true);
        let readiness = Readiness::new(
            vec![probe],
            RefreshPolicy {
                interval: Duration::ZERO,
                ..policy()
            },
        );
        let cancel = CancellationToken::new();
        let refresher = tokio::spawn({
            let readiness = readiness.clone();
            let cancel = cancel.clone();
            async move { readiness.refresh_until(cancel).await }
        });
        readiness.tx.subscribe().changed().await.unwrap();
        for _ in 0..3 {
            tokio::time::advance(MIN_INTERVAL).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(readiness.reader().verdict(), Ok(()));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            4,
            "the first check, then one per millisecond"
        );

        cancel.cancel();
        refresher
            .await
            .expect("a zero interval must not panic the refresher");
    }

    struct Slow {
        name: &'static str,
        delay: Duration,
    }

    #[async_trait::async_trait]
    impl Probe for Slow {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn check(&self) -> Result<(), ProbeError> {
            tokio::time::sleep(self.delay).await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn every_probe_is_checked_and_the_first_failure_in_registration_order_is_named() {
        let (healthy, _, healthy_calls) = flaky_probe(true);
        let (failing, _, failing_calls) = flaky_probe(false);
        let readiness = Readiness::new(
            vec![Box::new(Hanging), healthy, failing],
            RefreshPolicy {
                probe_budget: Duration::from_millis(1),
                ..policy()
            },
        );
        readiness.refresh().await;
        let verdict = readiness.reader().verdict();
        assert!(
            matches!(
                verdict,
                Err(NotReady::TimedOut {
                    probe: "hanging",
                    ..
                })
            ),
            "{verdict:?}"
        );
        assert_eq!(healthy_calls.load(Ordering::Relaxed), 1);
        assert_eq!(failing_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_probe_does_not_spend_another_probes_budget() {
        // Each probe needs most of the budget; one after the other they
        // would overrun it.
        let delay = policy().probe_budget * 3 / 4;
        let readiness = Readiness::new(
            vec![
                Box::new(Slow {
                    name: "first",
                    delay,
                }),
                Box::new(Slow {
                    name: "second",
                    delay,
                }),
            ],
            policy(),
        );
        readiness.refresh().await;
        assert_eq!(readiness.reader().verdict(), Ok(()));
    }

    /// Event names in emission order.
    #[derive(Clone, Default)]
    struct Events(Arc<std::sync::Mutex<Vec<String>>>);

    impl tracing::Subscriber for Events {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            let mut message = Message(String::new());
            event.record(&mut message);
            self.0.lock().unwrap().push(message.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    fn paused_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .expect("test runtime")
    }

    fn scrape(test: impl Future<Output = ()>) -> String {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&recorder, || paused_runtime().block_on(test));
        recorder.handle().render()
    }

    #[track_caller]
    fn assert_sample(scrape: &str, sample: &str) {
        assert!(scrape.lines().any(|line| line == sample), "{scrape}");
    }

    #[test]
    fn each_completed_check_is_counted_by_outcome() {
        let scrape = scrape(async {
            let (readiness, flag, _) = flaky(true);
            readiness.refresh().await;
            flag.store(false, Ordering::Relaxed);
            readiness.refresh().await;
            readiness.refresh().await;
            Readiness::new(vec![Box::new(Hanging)], policy())
                .refresh()
                .await;
        });

        for (outcome, count) in [("ok", 1), ("failed", 2), ("timed_out", 1)] {
            assert_sample(
                &scrape,
                &format!("readiness_checks_total{{outcome=\"{outcome}\"}} {count}"),
            );
        }
    }

    #[test]
    fn each_probe_is_counted_by_its_own_outcome_in_every_check() {
        let scrape = scrape(async {
            let (healthy, _, _) = flaky_probe(true);
            let readiness = Readiness::new(
                vec![
                    healthy,
                    Box::new(Hanging),
                    Box::new(Slow {
                        name: "slow",
                        delay: Duration::ZERO,
                    }),
                ],
                policy(),
            );
            readiness.refresh().await;
            readiness.refresh().await;
        });

        for (probe, outcome) in [("flaky", "ok"), ("hanging", "timed_out"), ("slow", "ok")] {
            assert_sample(
                &scrape,
                &format!(
                    "readiness_probe_checks_total{{probe=\"{probe}\",outcome=\"{outcome}\"}} 2"
                ),
            );
        }
        // The check itself is counted once per round, under the first failure.
        assert_sample(&scrape, "readiness_checks_total{outcome=\"timed_out\"} 2");
    }

    #[test]
    fn the_ready_gauge_is_the_published_verdict() {
        let ready = |scrape: String| {
            scrape
                .lines()
                .find(|line| line.starts_with("readiness_ready "))
                .map(str::to_owned)
        };
        let before_the_first_check = scrape(async {
            let _ = flaky(true);
        });
        assert_eq!(
            ready(before_the_first_check).as_deref(),
            Some("readiness_ready 0")
        );

        let absorbed_failure = scrape(async {
            let (readiness, flag, _) = flaky(true);
            readiness.refresh().await;
            flag.store(false, Ordering::Relaxed);
            readiness.refresh().await;
        });
        assert_eq!(
            ready(absorbed_failure).as_deref(),
            Some("readiness_ready 1")
        );

        let withdrawn = scrape(async {
            let (readiness, flag, _) = flaky(true);
            readiness.refresh().await;
            flag.store(false, Ordering::Relaxed);
            for _ in 0..3 {
                readiness.refresh().await;
            }
        });
        assert_eq!(ready(withdrawn).as_deref(), Some("readiness_ready 0"));

        let draining = scrape(async {
            let (readiness, _, _) = flaky(true);
            readiness.refresh().await;
            readiness.start_drain();
            // A check that passes during the drain does not bring it back.
            readiness.refresh().await;
        });
        assert_eq!(ready(draining).as_deref(), Some("readiness_ready 0"));
    }

    #[test]
    fn completion_metrics_date_only_finished_checks_and_survive_stopped_refresh() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let timestamp = || {
            handle
                .render()
                .lines()
                .find_map(|line| line.strip_prefix("readiness_last_completed_timestamp_seconds "))
                .expect("completion timestamp is exposed")
                .parse::<f64>()
                .unwrap()
        };
        metrics::with_local_recorder(&recorder, || {
            paused_runtime().block_on(async {
                let (readiness, flag, _) = flaky(true);
                assert_eq!(timestamp(), 0.0);
                assert_sample(&handle.render(), "readiness_stale_after_seconds 0.17");
                let before = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64();
                readiness.refresh().await;
                let completed = timestamp();
                let after = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64();
                assert!((before..=after).contains(&completed));
                tokio::time::advance(policy().stale_after() + Duration::from_millis(1)).await;
                assert_eq!(
                    timestamp().to_bits(),
                    completed.to_bits(),
                    "stopped refresh leaves a dated completion"
                );
                assert_sample(&handle.render(), "readiness_ready 1");
                assert!(matches!(
                    readiness.reader().verdict(),
                    Err(NotReady::Stale { .. })
                ));
                readiness.start_drain();
                assert_eq!(
                    timestamp().to_bits(),
                    completed.to_bits(),
                    "drain does not forge a completion"
                );
                flag.store(false, Ordering::Relaxed);
                let before_failure = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64();
                readiness.refresh().await;
                assert!(
                    timestamp() >= before_failure,
                    "failure during drain is still a completion"
                );
                assert_sample(&handle.render(), "readiness_ready 0");

                let hanging = Readiness::new(vec![Box::new(Hanging)], policy());
                let cancel = CancellationToken::new();
                let refresh = hanging.refresh_until(cancel.clone());
                tokio::pin!(refresh);
                std::future::poll_fn(|cx| {
                    assert!(refresh.as_mut().poll(cx).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                assert_eq!(timestamp(), 0.0, "in-flight is not completed");
                cancel.cancel();
                refresh.await;
                assert_eq!(timestamp(), 0.0, "cancellation is not completed");
                hanging.refresh().await;
                assert!(timestamp() > 0.0, "a timed-out round is completed");
            });
        });
    }

    #[test]
    fn absorbed_failures_flips_and_a_late_refresh_are_logged() {
        let events = Events::default();
        tracing::subscriber::with_default(events.clone(), || {
            // Keep callsite interest independent of a sibling test's thread-local
            // subscriber; tracing-core otherwise has a single-dispatcher fast path.
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            paused_runtime().block_on(async {
                let (readiness, flag, _) = flaky(true);
                readiness.refresh().await;
                flag.store(false, Ordering::Relaxed);
                // Two absorbed failures, the flip, then a failure that is
                // already published.
                for _ in 0..4 {
                    readiness.refresh().await;
                }
                flag.store(true, Ordering::Relaxed);
                readiness.refresh().await;
                tokio::time::advance(policy().stale_after() + Duration::from_millis(1)).await;
                readiness.refresh().await;
            });
        });

        assert_eq!(
            *events.0.lock().unwrap(),
            [
                "readiness_check_failed",
                "readiness_check_failed",
                "readiness_lost",
                "readiness_recovered",
                "readiness_refresh_late",
            ]
        );
    }

    #[test]
    fn a_draining_owner_logs_no_check_outcome() {
        let events = Events::default();
        tracing::subscriber::with_default(events.clone(), || {
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            paused_runtime().block_on(async {
                let (readiness, flag, _) = flaky(true);
                readiness.refresh().await;
                readiness.start_drain();
                flag.store(false, Ordering::Relaxed);
                for _ in 0..3 {
                    readiness.refresh().await;
                }
            });
        });

        assert!(events.0.lock().unwrap().is_empty(), "{:?}", events.0);
    }

    // template:begin grpc:health-changed-verdict-test
    #[tokio::test(start_paused = true)]
    async fn wait_for_verdict_event_delivers_drain_once_then_waits_without_losing_drain() {
        let (readiness, _, calls) = flaky(true);
        readiness.refresh().await;
        let mut reader = readiness.reader();

        readiness.start_drain();
        assert_eq!(reader.wait_for_verdict_event().await, Ok(()));

        let waiter = tokio::spawn(async move { reader.wait_for_verdict_event().await });
        tokio::task::yield_now().await;
        assert!(
            !waiter.is_finished(),
            "observed draining must wait for another publication"
        );

        readiness.refresh().await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("drain publication waiter must finish")
                .expect("drain publication waiter must not panic"),
            Ok(())
        );
        assert_eq!(readiness.reader().verdict(), Err(NotReady::Draining));
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_verdict_event_delivers_each_stale_edge_once_without_a_probe() {
        let (readiness, _, calls) = flaky(true);
        readiness.refresh().await;
        let mut reader = readiness.reader();
        let waiter = tokio::spawn(async move {
            let result = reader.wait_for_verdict_event().await;
            (reader, result)
        });
        tokio::task::yield_now().await;
        assert!(
            !waiter.is_finished(),
            "a fresh check must wait for its stale boundary"
        );

        tokio::time::advance(policy().stale_after() + Duration::from_nanos(1)).await;
        let (mut reader, result) = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("stale boundary waiter must finish")
            .expect("stale boundary waiter must not panic");
        assert_eq!(result, Ok(()));

        let waiter = tokio::spawn(async move { reader.wait_for_verdict_event().await });
        tokio::task::yield_now().await;
        assert!(
            !waiter.is_finished(),
            "the same stale check must not wake repeatedly"
        );

        readiness.refresh().await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("publication waiter must finish")
                .expect("publication waiter must not panic"),
            Ok(())
        );
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn wait_for_verdict_event_reports_when_the_owner_is_dropped() {
        let readiness = Readiness::new(Vec::new(), policy());
        let mut reader = readiness.reader();
        drop(readiness);
        assert_eq!(reader.wait_for_verdict_event().await, Err(OwnerDropped));
    }
    // template:end grpc:health-changed-verdict-test
}
