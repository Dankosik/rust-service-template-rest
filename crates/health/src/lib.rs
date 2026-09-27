//! Readiness: a verdict checked in the background and served from cache, plus
//! the drain flag.
//!
//! The readiness endpoints are unauthenticated, so they never reach a
//! dependency themselves; otherwise any caller could turn a probe request
//! into a database round-trip. A background task checks the probes on an
//! interval and publishes the result; readers only look at it.
//!
//! A cached verdict needs a watchdog: if the refresher stops or hangs, the
//! last "ready" would stand forever. [`RefreshPolicy::stale_after`] bounds
//! its age.
//!
//! The state travels through [`tokio::sync::watch`], so a streaming reader
//! can wait for the next publication.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// One dependency check.
///
/// All probes of one check share one deadline, `probe_budget`. When it
/// passes, the running [`Probe::check`] future is dropped and the verdict
/// names that probe. Implementations must not detach work that would outlive
/// that cancellation; a check that ignores cancellation holds the whole
/// refresh past its budget.
///
/// `async_trait` boxes the future so this trait stays object-safe for
/// `Box<dyn Probe>`. A native `async fn` in a trait is not `dyn`-safe on
/// this edition, so removing the attribute is a public shape change, not a
/// cleanup.
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

/// Cadence and thresholds for the refresher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshPolicy {
    /// Time between checks.
    pub interval: Duration,
    /// Deadline for one check across every probe.
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
    /// covers one in-flight check after the last stamp. Checks are serial, so
    /// a probe budget above the interval makes the loop run at the budget's
    /// pace; sizing from the interval alone would expire a verdict that is
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

/// The published result of the latest check.
#[derive(Clone, Debug)]
struct Check {
    at: Instant,
    /// What readers are told. A failure absorbed by `failure_threshold` is
    /// published as ready.
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
    stale_edge_for: Option<Instant>,
    // template:end grpc:health-reader-stale-edge
}

impl Readiness {
    /// A readiness owner over `probes`. Nothing is checked until
    /// [`Readiness::refresh`] or [`Readiness::refresh_until`] runs.
    #[must_use]
    pub fn new(probes: Vec<Box<dyn Probe>>, policy: RefreshPolicy) -> Self {
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
            changed
        });
    }

    /// Check every probe once and publish the verdict.
    ///
    /// Startup admission calls this and then reads
    /// [`ReadinessReader::verdict`], so the first probe after bind is
    /// answered from a real check.
    pub async fn refresh(&self) {
        let observed = self.check_probes().await;
        let at = Instant::now();
        let failure_threshold = self.policy.failure_threshold;
        let mut flipped_to = None;
        self.tx.send_modify(|state| {
            let next = next_check(state.last_check.as_ref(), observed, failure_threshold, at);
            // While draining, readers are told "draining" whatever the probes say.
            if !state.draining {
                flipped_to = flip(state.last_check.as_ref(), &next);
            }
            state.last_check = Some(next);
        });
        // Logged after the write lock is released so readers never wait on it.
        match flipped_to {
            Some(Ok(())) => tracing::info!("readiness recovered"),
            Some(Err(reason)) => tracing::warn!(%reason, "readiness lost"),
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
                let interval = self.policy.interval;
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
        for probe in self.probes.iter() {
            match tokio::time::timeout_at(deadline, probe.check()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    return Err(NotReady::ProbeFailed {
                        probe: probe.name(),
                        error,
                    });
                }
                Err(_elapsed) => {
                    return Err(NotReady::TimedOut {
                        probe: probe.name(),
                        budget,
                    });
                }
            }
        }
        Ok(())
    }
}

/// Fold one observed result into the published check. After a ready verdict,
/// failures are absorbed until `failure_threshold` of them come in a row.
fn next_check(
    previous: Option<&Check>,
    observed: Result<(), NotReady>,
    failure_threshold: u32,
    at: Instant,
) -> Check {
    let consecutive_failures = match (&observed, previous) {
        (Ok(()), _) => 0,
        (Err(_), Some(previous)) => previous.consecutive_failures + 1,
        (Err(_), None) => 1,
    };
    let was_ready = previous.is_some_and(|previous| previous.verdict.is_ok());
    let absorbed = was_ready && observed.is_err() && consecutive_failures < failure_threshold;
    Check {
        at,
        verdict: if absorbed { Ok(()) } else { observed },
        consecutive_failures,
    }
}

/// The new verdict when it flips between ready and not ready. The first
/// check at startup is not a flip; admission logs its own outcome.
fn flip(previous: Option<&Check>, next: &Check) -> Option<Result<(), NotReady>> {
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
    /// This observes the cached state only; it never checks a probe. A caller
    /// must re-read [`Self::verdict`] after this future resolves, which makes
    /// a publication race use the current monotone drain state.
    ///
    /// # Errors
    ///
    /// [`OwnerDropped`] when the last [`Readiness`] sender is gone.
    pub async fn changed_verdict(&mut self) -> Result<(), OwnerDropped> {
        if self.rx.has_changed().map_err(|_| OwnerDropped)? {
            return self.rx.changed().await.map_err(|_| OwnerDropped);
        }
        // Draining is monotone and never goes stale; nothing goes stale
        // before the first check.
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
        tokio::select! {
            changed = self.rx.changed() => changed.map_err(|_| OwnerDropped),
            () = tokio::time::sleep_until(stale_at) => {
                self.stale_edge_for = checked_at;
                Ok(())
            },
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

    #[tokio::test]
    async fn a_failed_probe_does_not_start_the_next_probe() {
        let (failing, _, first_calls) = flaky_probe(false);
        let (healthy, _, second_calls) = flaky_probe(true);
        let readiness = Readiness::new(vec![failing, healthy], policy());
        readiness.refresh().await;
        assert!(matches!(
            readiness.reader().verdict(),
            Err(NotReady::ProbeFailed { .. })
        ));
        assert_eq!(first_calls.load(Ordering::Relaxed), 1);
        assert_eq!(second_calls.load(Ordering::Relaxed), 0);
    }

    // template:begin grpc:health-changed-verdict-test
    #[tokio::test(start_paused = true)]
    async fn changed_verdict_delivers_drain_once_then_waits_without_losing_drain() {
        let (readiness, _, calls) = flaky(true);
        readiness.refresh().await;
        let mut reader = readiness.reader();

        readiness.start_drain();
        assert_eq!(reader.changed_verdict().await, Ok(()));

        let waiter = tokio::spawn(async move { reader.changed_verdict().await });
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
    async fn changed_verdict_delivers_each_stale_edge_once_without_a_probe() {
        let (readiness, _, calls) = flaky(true);
        readiness.refresh().await;
        let mut reader = readiness.reader();
        let waiter = tokio::spawn(async move {
            let result = reader.changed_verdict().await;
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

        let waiter = tokio::spawn(async move { reader.changed_verdict().await });
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
    async fn changed_verdict_reports_when_the_owner_is_dropped() {
        let readiness = Readiness::new(Vec::new(), policy());
        let mut reader = readiness.reader();
        drop(readiness);
        assert_eq!(reader.changed_verdict().await, Err(OwnerDropped));
    }
    // template:end grpc:health-changed-verdict-test
}
