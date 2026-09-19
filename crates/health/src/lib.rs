//! Readiness aggregation and the drain signal.
//!
//! Readiness is refreshed by a background task and served from the last
//! observed result. Evaluating probes per request would make the probe route
//! consume the dependency capacity it reports on: a pooled database ping
//! needs a pool connection, so a saturated pool fails readiness, the
//! orchestrator evicts the instance, and its traffic moves to instances that
//! are already saturated.
//!
//! Serving from a cache means something must keep the cache honest, which is
//! what the staleness bound in [`Readiness::verdict`] is for.
//!
//! The snapshot travels through [`tokio::sync::watch`], so tests and the
//! shutdown sequence await transitions instead of sleeping.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// One dependency check. Implementations must respect the deadline carried
/// by the caller's timeout; a check that ignores it holds the whole
/// evaluation past its budget.
#[async_trait::async_trait]
pub trait Probe: Send + Sync + 'static {
    /// Bounded label used in log lines and verdict messages.
    fn name(&self) -> &'static str;
    /// Resolve `Ok` when the dependency can serve requests.
    async fn check(&self) -> Result<(), ProbeError>;
}

/// Why one probe failed, without dependency internals.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProbeError(pub String);

impl ProbeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Why the service is not ready.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NotReady {
    #[error("service is draining")]
    Draining,
    #[error("readiness has not been evaluated yet")]
    NotEvaluated,
    #[error("readiness verdict is stale: last evaluated {age:?} ago, budget {budget:?}")]
    Stale { age: Duration, budget: Duration },
    #[error("readiness evaluation exceeded the {budget:?} budget")]
    TimedOut { budget: Duration },
    #[error("{probe} probe failed: {error}")]
    ProbeFailed {
        probe: &'static str,
        error: ProbeError,
    },
}

/// Cadence and thresholds for the refresher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshPolicy {
    /// Time between evaluations.
    pub interval: Duration,
    /// Budget for one evaluation across every probe.
    pub probe_budget: Duration,
    /// Consecutive failures before a healthy verdict flips to unhealthy. A
    /// service that has never been healthy reports the failure at once.
    pub failure_threshold: u32,
}

impl RefreshPolicy {
    /// How old a verdict may get before it is refused.
    ///
    /// `stale_after = probe_budget + period * 3` where
    /// `period = interval.max(probe_budget)`. Evaluations are serial, so a
    /// probe budget above the interval makes the loop run at the budget's
    /// pace; sizing from the interval alone would expire a verdict that is
    /// being refreshed as fast as it can be. Three periods so an ordinary
    /// missed tick does not flip readiness, finite so a dead refresher
    /// cannot leave a verdict standing forever.
    #[must_use]
    pub fn stale_after(&self) -> Duration {
        let period = self.interval.max(self.probe_budget);
        self.probe_budget + period * 3
    }
}

/// The immutable result of one evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Evaluation {
    /// The verdict a caller should see; `None` means ready.
    pub failure: Option<NotReady>,
    /// Failed evaluations since the last healthy one.
    pub consecutive_failures: u32,
    pub evaluated_at: Instant,
}

/// Published readiness state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// Set by [`Readiness::start_drain`]; read before anything else.
    pub draining: bool,
    /// `None` until the first evaluation completes.
    pub evaluation: Option<Evaluation>,
    /// Staleness budget published by the refresher; `None` before it runs.
    pub stale_after: Option<Duration>,
}

/// Readiness owner: holds the probes and publishes snapshots.
#[derive(Clone, Debug)]
pub struct Readiness {
    tx: Arc<watch::Sender<Snapshot>>,
    probes: Arc<Vec<Box<dyn Probe>>>,
}

impl std::fmt::Debug for dyn Probe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Probe").field("name", &self.name()).finish()
    }
}

/// Read side handed to HTTP handlers and the shutdown sequence.
#[derive(Clone, Debug)]
pub struct ReadinessReader {
    rx: watch::Receiver<Snapshot>,
}

impl Readiness {
    /// A readiness owner over `probes`. Nothing is evaluated until
    /// [`Readiness::refresh`] or [`Readiness::refresh_until`] runs.
    #[must_use]
    pub fn new(probes: Vec<Box<dyn Probe>>) -> Self {
        let (tx, _rx) = watch::channel(Snapshot {
            draining: false,
            evaluation: None,
            stale_after: None,
        });
        Self {
            tx: Arc::new(tx),
            probes: Arc::new(probes),
        }
    }

    /// A reader over the current and future snapshots.
    #[must_use]
    pub fn reader(&self) -> ReadinessReader {
        ReadinessReader {
            rx: self.tx.subscribe(),
        }
    }

    /// Mark the service as draining. Takes effect on the next read, not
    /// after the next refresh.
    pub fn start_drain(&self) {
        self.tx.send_if_modified(|snapshot| {
            let changed = !snapshot.draining;
            snapshot.draining = true;
            changed
        });
    }

    /// Run one evaluation and fold it into the snapshot.
    ///
    /// Published readiness is [`ReadinessReader::verdict`]. After a healthy
    /// streak, a single observed failure below `failure_threshold` does not
    /// flip that verdict. Startup admission reads `verdict` after this call
    /// so the first probe after bind is answered from a real evaluation.
    pub async fn refresh(&self, policy: RefreshPolicy) {
        let observed = match tokio::time::timeout(policy.probe_budget, self.evaluate()).await {
            Ok(observed) => observed,
            Err(_elapsed) => Err(NotReady::TimedOut {
                budget: policy.probe_budget,
            }),
        };
        let evaluated_at = Instant::now();
        self.tx.send_modify(|snapshot| {
            let previous = snapshot.evaluation.clone();
            snapshot.evaluation = Some(fold_evaluation(
                previous.as_ref(),
                &observed,
                policy,
                evaluated_at,
            ));
        });
    }

    /// Refresh on `policy.interval` until `cancel` fires.
    ///
    /// Publishes the staleness budget before awaiting the first evaluation
    /// so a refresher that dies on its first pass still leaves readers able
    /// to refuse the verdict it never wrote. The first evaluation runs
    /// immediately unless startup admission already seeded the cache.
    /// Cancel is selected against in-flight `refresh` so tracker join can
    /// finish before dependency close.
    pub async fn refresh_until(&self, policy: RefreshPolicy, cancel: CancellationToken) {
        self.tx.send_modify(|snapshot| {
            snapshot.stale_after = Some(policy.stale_after());
        });
        // Cancel covers both the timer and an in-flight probe. An already
        // cancelled token never polls the work; no detached task is created.
        let _ = cancel
            .run_until_cancelled(async {
                if self.tx.borrow().evaluation.is_none() {
                    self.refresh(policy).await;
                }
                let mut ticker = tokio::time::interval(policy.interval);
                // Delay, not Burst: a late tick is skipped rather than fired in a
                // catch-up burst that would pile probe work onto a recovering
                // dependency.
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // The first evaluation was done above or by startup admission.
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    let before = self.reader().verdict();
                    self.refresh(policy).await;
                    let after = self.reader().verdict();
                    if before.is_ok() != after.is_ok() {
                        match &after {
                            Ok(()) => tracing::info!("readiness recovered"),
                            Err(reason) => tracing::warn!(%reason, "readiness lost"),
                        }
                    }
                }
            })
            .await;
    }

    async fn evaluate(&self) -> Result<(), NotReady> {
        for probe in self.probes.iter() {
            probe.check().await.map_err(|error| NotReady::ProbeFailed {
                probe: probe.name(),
                error,
            })?;
        }
        Ok(())
    }
}

fn fold_evaluation(
    previous: Option<&Evaluation>,
    observed: &Result<(), NotReady>,
    policy: RefreshPolicy,
    evaluated_at: Instant,
) -> Evaluation {
    match observed {
        Ok(()) => Evaluation {
            failure: None,
            consecutive_failures: 0,
            evaluated_at,
        },
        Err(failure) => {
            let consecutive_failures = previous.map_or(0, |p| p.consecutive_failures) + 1;
            // Hold the previous healthy verdict until the streak reaches
            // the threshold. One that was already failing keeps reporting
            // the newest cause; one that has never been healthy fails
            // immediately.
            let reported = match previous {
                Some(p)
                    if p.failure.is_none() && consecutive_failures < policy.failure_threshold =>
                {
                    None
                }
                _ => Some(failure.clone()),
            };
            Evaluation {
                failure: reported,
                consecutive_failures,
                evaluated_at,
            }
        }
    }
}

impl ReadinessReader {
    /// The current verdict without touching any dependency.
    ///
    /// A verdict older than the refresher's own cadence is refused rather
    /// than served: a stopped refresher leaves its last verdict standing,
    /// and the last thing a healthy service writes is "healthy".
    /// Staleness applies only after [`Readiness::refresh_until`] publishes
    /// `stale_after`; `None` means the refresher is not running, not "never
    /// stale-check".
    ///
    /// # Errors
    ///
    /// Returns why the service is not ready.
    pub fn verdict(&self) -> Result<(), NotReady> {
        let snapshot = self.rx.borrow().clone();
        if snapshot.draining {
            return Err(NotReady::Draining);
        }
        let Some(evaluation) = snapshot.evaluation else {
            return Err(NotReady::NotEvaluated);
        };
        if let Some(budget) = snapshot.stale_after {
            let age = evaluation.evaluated_at.elapsed();
            if age > budget {
                return Err(NotReady::Stale { age, budget });
            }
        }
        match evaluation.failure {
            None => Ok(()),
            Some(reason) => Err(reason),
        }
    }

    /// Resolve after the next published change.
    pub async fn changed(&mut self) {
        let _ = self.rx.changed().await;
    }
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

    fn flaky(healthy: bool) -> (Readiness, Arc<AtomicBool>, Arc<AtomicU32>) {
        let flag = Arc::new(AtomicBool::new(healthy));
        let calls = Arc::new(AtomicU32::new(0));
        let readiness = Readiness::new(vec![Box::new(Flaky {
            healthy: flag.clone(),
            calls: calls.clone(),
        })]);
        (readiness, flag, calls)
    }

    #[tokio::test]
    async fn fails_closed_before_first_evaluation() {
        let (readiness, _, _) = flaky(true);
        assert_eq!(readiness.reader().verdict(), Err(NotReady::NotEvaluated));
    }

    #[tokio::test]
    async fn refresh_seeds_the_verdict_and_reads_do_not_probe() {
        let (readiness, _, calls) = flaky(true);
        readiness.refresh(policy()).await;
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
    async fn healthy_instance_survives_blips_below_the_threshold() {
        let (readiness, flag, _) = flaky(true);
        readiness.refresh(policy()).await;
        flag.store(false, Ordering::Relaxed);
        readiness.refresh(policy()).await;
        assert_eq!(readiness.reader().verdict(), Ok(()), "1 of 3 failures");
        readiness.refresh(policy()).await;
        assert_eq!(readiness.reader().verdict(), Ok(()), "2 of 3 failures");
        readiness.refresh(policy()).await;
        assert!(
            matches!(
                readiness.reader().verdict(),
                Err(NotReady::ProbeFailed { probe: "flaky", .. })
            ),
            "3 of 3 failures flips"
        );
        flag.store(true, Ordering::Relaxed);
        readiness.refresh(policy()).await;
        assert_eq!(readiness.reader().verdict(), Ok(()), "one success recovers");
    }

    #[tokio::test]
    async fn never_healthy_instance_fails_immediately() {
        let (readiness, _, _) = flaky(false);
        readiness.refresh(policy()).await;
        assert!(matches!(
            readiness.reader().verdict(),
            Err(NotReady::ProbeFailed { .. })
        ));
    }

    #[tokio::test]
    async fn hanging_probe_is_bounded_by_the_budget() {
        let readiness = Readiness::new(vec![Box::new(Hanging)]);
        let started = Instant::now();
        readiness.refresh(policy()).await;
        let err = readiness.reader().verdict().unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(err, NotReady::TimedOut { .. }), "{err}");
    }

    #[tokio::test]
    async fn draining_wins_over_a_healthy_verdict_immediately() {
        let (readiness, _, _) = flaky(true);
        readiness.refresh(policy()).await;
        let mut reader = readiness.reader();
        readiness.start_drain();
        reader.changed().await;
        assert_eq!(reader.verdict(), Err(NotReady::Draining));
    }

    #[tokio::test(start_paused = true)]
    async fn stale_verdict_is_refused_when_the_refresher_stops() {
        let (readiness, _, _) = flaky(true);
        let cancel = CancellationToken::new();
        let watcher = tokio::spawn({
            let readiness = readiness.clone();
            let cancel = cancel.clone();
            async move { readiness.refresh_until(policy(), cancel).await }
        });
        let mut reader = readiness.reader();
        reader.changed().await;
        tokio::task::yield_now().await;
        assert_eq!(reader.verdict(), Ok(()));

        cancel.cancel();
        watcher.await.unwrap();
        tokio::time::advance(policy().stale_after() + Duration::from_millis(1)).await;
        assert!(
            matches!(reader.verdict(), Err(NotReady::Stale { .. })),
            "{:?}",
            reader.verdict()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn refreshes_on_the_interval_and_stops_on_cancel() {
        let (readiness, flag, calls) = flaky(true);
        let cancel = CancellationToken::new();
        let watcher = tokio::spawn({
            let readiness = readiness.clone();
            let cancel = cancel.clone();
            async move { readiness.refresh_until(policy(), cancel).await }
        });
        let mut reader = readiness.reader();
        reader.changed().await;
        assert_eq!(reader.verdict(), Ok(()));

        flag.store(false, Ordering::Relaxed);
        for _ in 0..3 {
            tokio::time::advance(policy().interval).await;
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            reader.verdict(),
            Err(NotReady::ProbeFailed { .. })
        ));
        assert!(calls.load(Ordering::Relaxed) >= 4);

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), watcher)
            .await
            .expect("refresh_until must return on cancel")
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_until_stops_while_an_evaluation_is_in_flight() {
        let readiness = Readiness::new(vec![Box::new(Hanging)]);
        let cancel = CancellationToken::new();
        let watcher = tokio::spawn({
            let readiness = readiness.clone();
            let cancel = cancel.clone();
            async move { readiness.refresh_until(policy(), cancel).await }
        });
        tokio::task::yield_now().await;
        cancel.cancel();
        tokio::task::yield_now().await;
        assert!(
            watcher.is_finished(),
            "cancel must drop the in-flight refresh, not wait for probe_budget"
        );
        watcher.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn an_already_cancelled_refresher_never_checks_a_probe() {
        let (readiness, _, calls) = flaky(true);
        let cancel = CancellationToken::new();
        cancel.cancel();
        readiness.refresh_until(policy(), cancel).await;
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(readiness.reader().verdict(), Err(NotReady::NotEvaluated));
    }

    #[tokio::test]
    async fn a_failed_probe_does_not_start_the_next_probe() {
        let first_calls = Arc::new(AtomicU32::new(0));
        let second_calls = Arc::new(AtomicU32::new(0));
        let readiness = Readiness::new(vec![
            Box::new(Flaky {
                healthy: Arc::new(AtomicBool::new(false)),
                calls: first_calls.clone(),
            }),
            Box::new(Flaky {
                healthy: Arc::new(AtomicBool::new(true)),
                calls: second_calls.clone(),
            }),
        ]);
        readiness.refresh(policy()).await;
        assert!(matches!(
            readiness.reader().verdict(),
            Err(NotReady::ProbeFailed { .. })
        ));
        assert_eq!(first_calls.load(Ordering::Relaxed), 1);
        assert_eq!(second_calls.load(Ordering::Relaxed), 0);
    }
}
