use std::borrow::Cow;
use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::future::join_all;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[async_trait::async_trait]
pub trait Probe: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    async fn check(&self) -> Result<(), ProbeError>;
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProbeError(Cow<'static, str>);

impl ProbeError {
    #[must_use]
    pub const fn from_static(message: &'static str) -> Self {
        Self(Cow::Borrowed(message))
    }

    #[must_use]
    pub fn format(args: fmt::Arguments<'_>) -> Self {
        Self(Cow::Owned(fmt::format(args)))
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("refresh interval must be between {min:?} and {max:?}, got {0:?}", min = RefreshPolicy::MIN_DURATION, max = RefreshPolicy::MAX_DURATION)]
    Interval(Duration),
    #[error("probe budget must be between {min:?} and {max:?}, got {0:?}", min = RefreshPolicy::MIN_DURATION, max = RefreshPolicy::MAX_DURATION)]
    ProbeBudget(Duration),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshPolicy {
    interval: Duration,
    probe_budget: Duration,
    failure_threshold: NonZeroU32,
}

impl RefreshPolicy {
    const MIN_DURATION: Duration = Duration::from_millis(1);
    const MAX_DURATION: Duration = Duration::from_hours(1);

    pub const fn try_new(
        interval: Duration,
        probe_budget: Duration,
        failure_threshold: NonZeroU32,
    ) -> Result<Self, PolicyError> {
        const MIN_NANOS: u128 = RefreshPolicy::MIN_DURATION.as_nanos();
        const MAX_NANOS: u128 = RefreshPolicy::MAX_DURATION.as_nanos();
        if !matches!(interval.as_nanos(), MIN_NANOS..=MAX_NANOS) {
            return Err(PolicyError::Interval(interval));
        }
        if !matches!(probe_budget.as_nanos(), MIN_NANOS..=MAX_NANOS) {
            return Err(PolicyError::ProbeBudget(probe_budget));
        }
        Ok(Self {
            interval,
            probe_budget,
            failure_threshold,
        })
    }

    #[must_use]
    pub const fn stale_after(&self) -> Duration {
        let period = if self.interval.as_nanos() >= self.probe_budget.as_nanos() {
            self.interval
        } else {
            self.probe_budget
        };
        self.probe_budget.saturating_add(period.saturating_mul(3))
    }
}

#[derive(Clone, Debug)]
struct State {
    draining: bool,
    last_check: Option<Check>,
}

impl State {
    fn verdict(&self, now: Instant, stale_after: Duration) -> Result<(), NotReady> {
        if self.draining {
            return Err(NotReady::Draining);
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
    at: Instant,
    verdict: Result<(), ProbeFailure>,
    consecutive_failures: u32,
}

impl Check {
    fn is_ready_at(&self, now: Instant, stale_after: Duration) -> bool {
        self.verdict.is_ok() && self.is_fresh_at(now, stale_after)
    }

    fn is_fresh_at(&self, now: Instant, stale_after: Duration) -> bool {
        now.duration_since(self.at) <= stale_after
    }
}

#[derive(Clone)]
pub struct Readiness {
    tx: watch::Sender<State>,
    draining: Arc<AtomicBool>,
    probes: Arc<[Box<dyn Probe>]>,
    policy: RefreshPolicy,
}

impl fmt::Debug for Readiness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.probes.iter().map(|probe| probe.name()).collect();
        f.debug_struct("Readiness")
            .field("draining", &self.draining.load(Ordering::Acquire))
            .field("probes", &names)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct ReadinessReader {
    rx: watch::Receiver<State>,
    draining: Arc<AtomicBool>,
    stale_after: Duration,
}

impl Readiness {
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
            "1 while the last completed check published a ready verdict and the service is not draining, 0 otherwise."
        );
        metrics::gauge!(READY_METRIC).set(0.0);
        Self {
            tx: watch::Sender::new(State {
                draining: false,
                last_check: None,
            }),
            draining: Arc::new(AtomicBool::new(false)),
            probes: probes.into(),
            policy,
        }
    }

    #[must_use]
    pub fn reader(&self) -> ReadinessReader {
        ReadinessReader {
            rx: self.tx.subscribe(),
            draining: Arc::clone(&self.draining),
            stale_after: self.policy.stale_after(),
        }
    }

    pub fn start_drain(&self) {
        self.tx.send_if_modified(|state| {
            if state.draining {
                return false;
            }
            state.draining = true;
            self.draining.store(true, Ordering::Release);
            metrics::gauge!(READY_METRIC).set(0.0);
            true
        });
    }

    pub async fn admit(&self) -> Result<(), NotReady> {
        self.refresh().await;
        self.tx
            .borrow()
            .verdict(Instant::now(), self.policy.stale_after())
    }

    pub async fn refresh(&self) {
        let observed = self.check_probes().await;
        metrics::counter!(CHECKS_METRIC, "outcome" => outcome_label(&observed)).increment(1);
        let failure_threshold = self.policy.failure_threshold;
        let stale_after = self.policy.stale_after();
        let mut transition_to_log = None;
        let mut absorbed_failure = None;
        let mut late_by = None;
        self.tx.send_if_modified(|state| {
            let at = Instant::now();
            let previous = state.last_check.as_ref();
            let was_ready =
                !state.draining && previous.is_some_and(|check| check.is_ready_at(at, stale_after));
            let (next, absorbed) =
                apply_failure_threshold(previous, observed, failure_threshold, at, stale_after);
            if !state.draining {
                transition_to_log = readiness_transition(previous, &next);
                absorbed_failure = absorbed.map(|reason| (reason, next.consecutive_failures));
                late_by = previous
                    .filter(|previous| !previous.is_fresh_at(at, stale_after))
                    .map(|previous| at.duration_since(previous.at));
            }
            let now_ready = !state.draining && next.verdict.is_ok();
            let notify = !state.draining && (previous.is_none() || was_ready != now_ready);
            metrics::gauge!(READY_METRIC).set(f64::from(now_ready));
            state.last_check = Some(next);
            notify
        });
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

    pub async fn refresh_until(&self, cancel: CancellationToken) {
        let _ = cancel
            .run_until_cancelled(async {
                if self.tx.borrow().last_check.is_none() {
                    self.refresh().await;
                }
                let mut ticker = tokio::time::interval_at(
                    Instant::now() + self.policy.interval,
                    self.policy.interval,
                );
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    self.refresh().await;
                }
            })
            .await;
    }

    async fn check_probes(&self) -> Result<(), ProbeFailure> {
        let budget = self.policy.probe_budget;
        let deadline = Instant::now() + budget;
        let outcomes = join_all(self.probes.iter().map(|probe| async move {
            let probe_name = probe.name();
            let (outcome, label) = match tokio::time::timeout_at(deadline, probe.check()).await {
                Ok(Ok(())) => (Ok(()), "ok"),
                Ok(Err(error)) => (
                    Err(ProbeFailure::Failed {
                        probe: probe_name,
                        error,
                    }),
                    "failed",
                ),
                Err(_elapsed) => (
                    Err(ProbeFailure::TimedOut {
                        probe: probe_name,
                        budget,
                    }),
                    "timed_out",
                ),
            };
            metrics::counter!(
                PROBE_CHECKS_METRIC,
                "probe" => probe_name,
                "outcome" => label
            )
            .increment(1);
            outcome
        }))
        .await;
        outcomes.into_iter().collect()
    }
}

fn apply_failure_threshold(
    previous: Option<&Check>,
    observed: Result<(), ProbeFailure>,
    failure_threshold: NonZeroU32,
    at: Instant,
    stale_after: Duration,
) -> (Check, Option<ProbeFailure>) {
    match observed {
        Ok(()) => (
            Check {
                at,
                verdict: Ok(()),
                consecutive_failures: 0,
            },
            None,
        ),
        Err(reason) => {
            let consecutive_failures = previous
                .map_or(0, |p| p.consecutive_failures)
                .saturating_add(1);
            let was_published_ready =
                previous.is_some_and(|check| check.is_ready_at(at, stale_after));
            let (verdict, absorbed) =
                if was_published_ready && consecutive_failures < failure_threshold.get() {
                    (Ok(()), Some(reason))
                } else {
                    (Err(reason), None)
                };
            (
                Check {
                    at,
                    verdict,
                    consecutive_failures,
                },
                absorbed,
            )
        }
    }
}

fn readiness_transition(
    previous: Option<&Check>,
    next: &Check,
) -> Option<Result<(), ProbeFailure>> {
    let previous = previous?;
    (previous.verdict.is_ok() != next.verdict.is_ok()).then(|| next.verdict.clone())
}

impl ReadinessReader {
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.verdict().is_ok()
    }

    pub fn verdict(&self) -> Result<(), NotReady> {
        self.rx.borrow().verdict(Instant::now(), self.stale_after)
    }

    // template:begin grpc:health-changed-verdict
    pub async fn wait_for_drain_or_owner_drop(&mut self) {
        let _ = self.rx.wait_for(|state| state.draining).await;
    }

    pub async fn wait_until_ready_is_not(&mut self, seen: bool) -> Option<bool> {
        loop {
            let stale_at = {
                let state = self.rx.borrow_and_update();
                let ready = state.verdict(Instant::now(), self.stale_after).is_ok();
                if ready != seen {
                    return Some(ready);
                }
                if state.draining {
                    return None;
                }
                state
                    .last_check
                    .as_ref()
                    .filter(|_| ready)
                    .map(|check| check.at + self.stale_after + Duration::from_nanos(1))
            };
            let changed = self.rx.changed();
            match stale_at {
                Some(stale_at) => {
                    if let Ok(changed) = tokio::time::timeout_at(stale_at, changed).await {
                        changed.ok()?;
                    }
                }
                None => changed.await.ok()?,
            }
        }
    }
    // template:end grpc:health-changed-verdict
}

#[cfg(test)]
mod tests;
