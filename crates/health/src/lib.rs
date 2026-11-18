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
    fn fresh_check(&self, now: Instant, stale_after: Duration) -> Result<&Check, NotReady> {
        if self.draining {
            return Err(NotReady::Draining);
        }
        let Some(check) = &self.last_check else {
            return Err(NotReady::NotEvaluated);
        };
        freshness(check.at, now, stale_after)?;
        Ok(check)
    }

    fn ready_check(&self, now: Instant, stale_after: Duration) -> Option<&Check> {
        self.fresh_check(now, stale_after)
            .ok()
            .filter(|check| check.verdict.is_ok())
    }

    fn verdict(&self, now: Instant, stale_after: Duration) -> Result<(), NotReady> {
        self.fresh_check(now, stale_after)?
            .verdict
            .clone()
            .map_err(NotReady::from)
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
        self.verdict.is_ok() && freshness(self.at, now, stale_after).is_ok()
    }
}

fn freshness(at: Instant, now: Instant, stale_after: Duration) -> Result<(), NotReady> {
    let age = now.duration_since(at);
    if age > stale_after {
        return Err(NotReady::Stale { age, stale_after });
    }
    Ok(())
}

pub struct Readiness {
    control: ReadinessControl,
    probes: Vec<Box<dyn Probe>>,
}

#[derive(Clone, Debug)]
pub struct ReadinessControl {
    tx: watch::Sender<State>,
    draining: Arc<AtomicBool>,
    policy: RefreshPolicy,
}

#[derive(Debug)]
pub struct PreparedReadiness {
    readiness: Readiness,
    checked_at: Instant,
}

#[derive(Debug)]
pub struct ReadinessRefresher {
    readiness: Readiness,
    first_tick: Instant,
}

impl fmt::Debug for Readiness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.probes.iter().map(|probe| probe.name()).collect();
        f.debug_struct("Readiness")
            .field("draining", &self.control.draining.load(Ordering::Acquire))
            .field("probes", &names)
            .field("policy", &self.control.policy)
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
        Self {
            control: ReadinessControl {
                tx: watch::Sender::new(State {
                    draining: false,
                    last_check: None,
                }),
                draining: Arc::new(AtomicBool::new(false)),
                policy,
            },
            probes,
        }
    }

    #[must_use]
    pub fn control(&self) -> ReadinessControl {
        self.control.clone()
    }

    pub async fn prepare(self) -> Result<PreparedReadiness, NotReady> {
        let checked_at = self.check_startup().await?;
        Ok(PreparedReadiness {
            readiness: self,
            checked_at,
        })
    }

    async fn check_startup(&self) -> Result<Instant, NotReady> {
        self.control.ensure_running()?;
        let observed = self.check_probes().await;
        self.control.ensure_running()?;
        observed?;
        Ok(Instant::now())
    }
}

impl ReadinessControl {
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
            true
        });
    }

    fn ensure_running(&self) -> Result<(), NotReady> {
        if self.draining.load(Ordering::Acquire) {
            return Err(NotReady::Draining);
        }
        Ok(())
    }
}

impl PreparedReadiness {
    pub async fn ensure_fresh(&mut self) -> Result<(), NotReady> {
        self.readiness.control.ensure_running()?;
        let stale_after = self.readiness.control.policy.stale_after();
        if freshness(self.checked_at, Instant::now(), stale_after).is_err() {
            self.checked_at = self.readiness.check_startup().await?;
        }
        Ok(())
    }

    pub fn activate(self) -> Result<ReadinessRefresher, NotReady> {
        let control = &self.readiness.control;
        let mut admission = Ok(());
        control.tx.send_if_modified(|state| {
            admission = if state.draining {
                Err(NotReady::Draining)
            } else {
                freshness(
                    self.checked_at,
                    Instant::now(),
                    control.policy.stale_after(),
                )
            };
            if admission.is_err() {
                return false;
            }
            state.last_check = Some(Check {
                at: self.checked_at,
                verdict: Ok(()),
                consecutive_failures: 0,
            });
            true
        });
        admission?;
        Ok(ReadinessRefresher {
            first_tick: self.checked_at + control.policy.interval,
            readiness: self.readiness,
        })
    }
}

impl ReadinessRefresher {
    async fn refresh(&mut self) {
        let control = &self.readiness.control;
        if control.draining.load(Ordering::Acquire) {
            return;
        }
        let observed = self.readiness.check_probes().await;
        let failure_threshold = control.policy.failure_threshold;
        let stale_after = control.policy.stale_after();
        let mut transition_to_log = None;
        let mut absorbed_failure = None;
        let mut late_by = None;
        control.tx.send_if_modified(|state| {
            if state.draining {
                return false;
            }
            let at = Instant::now();
            let previous = state.last_check.as_ref();
            let was_ready = previous.is_some_and(|check| check.is_ready_at(at, stale_after));
            let (next, absorbed) =
                apply_failure_threshold(previous, observed, failure_threshold, at, was_ready);
            let now_ready = next.verdict.is_ok();
            let changed = if now_ready {
                !was_ready
            } else {
                previous.is_some_and(|check| check.verdict.is_ok())
            };
            if changed {
                transition_to_log = Some(next.verdict.clone());
            }
            absorbed_failure = absorbed.map(|reason| (reason, next.consecutive_failures));
            late_by = previous
                .map(|previous| at.duration_since(previous.at))
                .filter(|age| *age > stale_after);
            state.last_check = Some(next);
            true
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

    pub async fn refresh_until(mut self, cancel: CancellationToken) {
        let _ = cancel
            .run_until_cancelled(async {
                let mut ticker = tokio::time::interval_at(
                    self.first_tick,
                    self.readiness.control.policy.interval,
                );
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    self.refresh().await;
                }
            })
            .await;
    }
}

impl Readiness {
    async fn check_probes(&self) -> Result<(), ProbeFailure> {
        let budget = self.control.policy.probe_budget;
        let deadline = Instant::now() + budget;
        let checks = self
            .probes
            .iter()
            .map(|probe| check_probe(probe.as_ref(), deadline, budget));
        let outcomes = join_all(checks).await;
        let observed = outcomes.into_iter().collect();
        metrics::counter!(CHECKS_METRIC, "outcome" => outcome_label(&observed)).increment(1);
        observed
    }
}

async fn check_probe(
    probe: &dyn Probe,
    deadline: Instant,
    budget: Duration,
) -> Result<(), ProbeFailure> {
    let probe_name = probe.name();
    let result = tokio::time::timeout_at(deadline, probe.check()).await;
    let outcome = match result.ok().filter(|_| Instant::now() <= deadline) {
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
}

fn apply_failure_threshold(
    previous: Option<&Check>,
    observed: Result<(), ProbeFailure>,
    failure_threshold: NonZeroU32,
    at: Instant,
    was_ready: bool,
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
            let (verdict, absorbed) = if was_ready && consecutive_failures < failure_threshold.get()
            {
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

impl ReadinessReader {
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.rx
            .borrow()
            .ready_check(Instant::now(), self.stale_after)
            .is_some()
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
                let ready_check = state.ready_check(Instant::now(), self.stale_after);
                let ready = ready_check.is_some();
                if ready != seen {
                    return Some(ready);
                }
                if state.draining {
                    return None;
                }
                ready_check.map(|check| check.at + self.stale_after + Duration::from_nanos(1))
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
