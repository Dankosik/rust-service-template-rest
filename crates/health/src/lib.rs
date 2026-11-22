use std::borrow::Cow;
use std::fmt;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::future::join_all;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub trait Probe: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn check(&self) -> impl Future<Output = Result<(), ProbeError>> + Send;
}

type ProbeFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ProbeError>> + Send + 'a>>;

trait ErasedProbe: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn check(&self) -> ProbeFuture<'_>;
}

impl<P: Probe> ErasedProbe for P {
    fn name(&self) -> &'static str {
        Probe::name(self)
    }

    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(Probe::check(self))
    }
}

pub struct BoxedProbe(Box<dyn ErasedProbe>);

impl BoxedProbe {
    #[must_use]
    pub fn new(probe: impl Probe) -> Self {
        Self(Box::new(probe))
    }

    fn name(&self) -> &'static str {
        self.0.name()
    }

    fn check(&self) -> ProbeFuture<'_> {
        self.0.check()
    }
}

impl fmt::Debug for BoxedProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("BoxedProbe").field(&self.name()).finish()
    }
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

const CHECKS_METRIC: &str = "readiness_checks_total";

const PROBE_CHECKS_METRIC: &str = "readiness_probe_checks_total";

static CHECK_METRIC_METADATA: metrics::Metadata<'static> =
    metrics::Metadata::new(module_path!(), metrics::Level::INFO, Some(module_path!()));

#[derive(Clone, Copy)]
enum CheckOutcome {
    Ok,
    TimedOut,
    Failed,
}

impl CheckOutcome {
    const fn of(outcome: &Result<(), ProbeFailure>) -> Self {
        match outcome {
            Ok(()) => Self::Ok,
            Err(ProbeFailure::TimedOut { .. }) => Self::TimedOut,
            Err(ProbeFailure::Failed { .. }) => Self::Failed,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::TimedOut => "timed_out",
            Self::Failed => "failed",
        }
    }

    fn metric_key(self) -> &'static metrics::Key {
        static OK_LABELS: [metrics::Label; 1] = [metrics::Label::from_static_parts(
            "outcome",
            CheckOutcome::Ok.label(),
        )];
        static TIMED_OUT_LABELS: [metrics::Label; 1] = [metrics::Label::from_static_parts(
            "outcome",
            CheckOutcome::TimedOut.label(),
        )];
        static FAILED_LABELS: [metrics::Label; 1] = [metrics::Label::from_static_parts(
            "outcome",
            CheckOutcome::Failed.label(),
        )];
        static OK: metrics::Key = metrics::Key::from_static_parts(CHECKS_METRIC, &OK_LABELS);
        static TIMED_OUT: metrics::Key =
            metrics::Key::from_static_parts(CHECKS_METRIC, &TIMED_OUT_LABELS);
        static FAILED: metrics::Key =
            metrics::Key::from_static_parts(CHECKS_METRIC, &FAILED_LABELS);
        match self {
            Self::Ok => &OK,
            Self::TimedOut => &TIMED_OUT,
            Self::Failed => &FAILED,
        }
    }
}

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
    const STALE_AFTER_PERIODS: u32 = 3;

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
        self.probe_budget
            .saturating_add(period.saturating_mul(Self::STALE_AFTER_PERIODS))
    }
}

#[derive(Clone, Debug)]
enum State {
    NotEvaluated,
    Evaluated(Evaluation),
    Draining,
}

enum RefreshEvent {
    Unchanged,
    Recovered,
    Lost(ProbeFailure),
    AbsorbedFailure {
        reason: ProbeFailure,
        consecutive_failures: u32,
    },
}

struct RefreshLog {
    stale_age: Option<Duration>,
    event: RefreshEvent,
}

impl RefreshLog {
    fn emit(self, stale_after: Duration, failure_threshold: NonZeroU32) {
        if let Some(age) = self.stale_age {
            tracing::warn!(?age, ?stale_after, "readiness_refresh_late");
        }
        match self.event {
            RefreshEvent::Unchanged => {}
            RefreshEvent::Recovered => tracing::info!("readiness_recovered"),
            RefreshEvent::Lost(reason) => {
                tracing::warn!(reason = %NotReady::from(reason), "readiness_lost");
            }
            RefreshEvent::AbsorbedFailure {
                reason,
                consecutive_failures,
            } => {
                tracing::warn!(
                    reason = %NotReady::from(reason),
                    consecutive_failures,
                    failure_threshold,
                    "readiness_check_failed"
                );
            }
        }
    }
}

impl State {
    fn apply_round(
        &mut self,
        observed: Result<(), ProbeFailure>,
        now: Instant,
        failure_threshold: NonZeroU32,
        stale_after: Duration,
    ) -> Option<RefreshLog> {
        let previous = match self {
            Self::Draining => return None,
            Self::NotEvaluated => None,
            Self::Evaluated(check) => Some(&*check),
        };
        let stale_age = previous.and_then(|check| check.stale_age(now));
        let is_fresh = stale_age.is_none();
        let (verdict, event) = match (observed, previous.map(|check| &check.verdict)) {
            (Ok(()), Some(Verdict::Ready { .. })) if is_fresh => {
                (Verdict::READY, RefreshEvent::Unchanged)
            }
            (Ok(()), _) => (Verdict::READY, RefreshEvent::Recovered),
            (
                Err(reason),
                Some(Verdict::Ready {
                    consecutive_failures: previous_failures,
                }),
            ) => {
                let consecutive_failures = previous_failures.saturating_add(1);
                if is_fresh && consecutive_failures < failure_threshold.get() {
                    (
                        Verdict::Ready {
                            consecutive_failures,
                        },
                        RefreshEvent::AbsorbedFailure {
                            reason,
                            consecutive_failures,
                        },
                    )
                } else {
                    (
                        Verdict::NotReady(reason.clone()),
                        RefreshEvent::Lost(reason),
                    )
                }
            }
            (Err(reason), Some(Verdict::NotReady(_)) | None) => {
                (Verdict::NotReady(reason), RefreshEvent::Unchanged)
            }
        };
        *self = Self::Evaluated(Evaluation::new(now, stale_after, verdict));
        Some(RefreshLog { stale_age, event })
    }

    fn readiness(&self) -> (bool, Option<Instant>) {
        match self {
            Self::Evaluated(check)
                if check.verdict.is_ready() && check.stale_age(Instant::now()).is_none() =>
            {
                (true, check.stale_at)
            }
            _ => (false, None),
        }
    }

    fn verdict(&self, stale_after: Duration) -> Result<(), NotReady> {
        let check = match self {
            Self::Draining => return Err(NotReady::Draining),
            Self::NotEvaluated => return Err(NotReady::NotEvaluated),
            Self::Evaluated(check) => check,
        };
        if let Some(age) = check.stale_age(Instant::now()) {
            return Err(NotReady::Stale { age, stale_after });
        }
        check.verdict.to_result()
    }
}

#[derive(Clone, Debug)]
enum Verdict {
    Ready { consecutive_failures: u32 },
    NotReady(ProbeFailure),
}

impl Verdict {
    const READY: Self = Self::Ready {
        consecutive_failures: 0,
    };

    fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    fn to_result(&self) -> Result<(), NotReady> {
        match self {
            Self::Ready { .. } => Ok(()),
            Self::NotReady(failure) => Err(NotReady::from(failure.clone())),
        }
    }
}

#[derive(Clone, Debug)]
struct Evaluation {
    checked_at: Instant,
    stale_at: Option<Instant>,
    verdict: Verdict,
}

impl Evaluation {
    fn new(checked_at: Instant, stale_after: Duration, verdict: Verdict) -> Self {
        Self {
            checked_at,
            stale_at: stale_deadline(checked_at, stale_after),
            verdict,
        }
    }

    fn stale_age(&self, now: Instant) -> Option<Duration> {
        self.stale_at
            .filter(|&deadline| now >= deadline)
            .map(|_| now.duration_since(self.checked_at))
    }
}

fn stale_deadline(checked_at: Instant, stale_after: Duration) -> Option<Instant> {
    checked_at
        .checked_add(stale_after)?
        .checked_add(Duration::from_nanos(1))
}

fn check_freshness(
    checked_at: Instant,
    now: Instant,
    stale_after: Duration,
) -> Result<(), NotReady> {
    if stale_deadline(checked_at, stale_after).is_some_and(|deadline| now >= deadline) {
        Err(NotReady::Stale {
            age: now.duration_since(checked_at),
            stale_after,
        })
    } else {
        Ok(())
    }
}

pub struct Readiness {
    control: ReadinessControl,
    probes: Vec<BoxedProbe>,
}

#[derive(Clone, Debug)]
pub struct ReadinessControl {
    tx: watch::Sender<State>,
    draining: Arc<AtomicBool>,
    policy: RefreshPolicy,
}

#[must_use = "activate PreparedReadiness to publish readiness state"]
#[derive(Debug)]
pub struct PreparedReadiness {
    readiness: Readiness,
    checked_at: Instant,
}

#[must_use = "run ReadinessRefresher to keep readiness fresh"]
#[derive(Debug)]
pub struct ReadinessRefresher {
    readiness: Readiness,
    first_tick: Instant,
}

impl fmt::Debug for Readiness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let probes = fmt::from_fn(|f| {
            f.debug_list()
                .entries(self.probes.iter().map(BoxedProbe::name))
                .finish()
        });
        f.debug_struct("Readiness")
            .field("draining", &self.control.draining.load(Ordering::Acquire))
            .field("probes", &probes)
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
    pub fn new(probes: Vec<BoxedProbe>, policy: RefreshPolicy) -> Self {
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
                tx: watch::Sender::new(State::NotEvaluated),
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
            if matches!(state, State::Draining) {
                return false;
            }
            *state = State::Draining;
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
        if check_freshness(self.checked_at, Instant::now(), stale_after).is_err() {
            self.checked_at = self.readiness.check_startup().await?;
        }
        Ok(())
    }

    pub fn activate(self) -> Result<ReadinessRefresher, NotReady> {
        let control = &self.readiness.control;
        let mut admission = Ok(());
        let stale_after = control.policy.stale_after();
        control.tx.send_if_modified(|state| {
            admission = if matches!(state, State::Draining) {
                Err(NotReady::Draining)
            } else {
                check_freshness(self.checked_at, Instant::now(), stale_after)
            };
            if admission.is_err() {
                return false;
            }
            *state = State::Evaluated(Evaluation::new(
                self.checked_at,
                stale_after,
                Verdict::READY,
            ));
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
        let mut log = None;
        control.tx.send_if_modified(|state| {
            log = state.apply_round(observed, Instant::now(), failure_threshold, stale_after);
            log.is_some()
        });
        if let Some(log) = log {
            log.emit(stale_after, failure_threshold);
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
        let observed = match self.probes.as_slice() {
            [probe] => check_probe(probe, deadline, budget).await,
            probes => {
                let checks = probes
                    .iter()
                    .map(|probe| check_probe(probe, deadline, budget));
                join_all(checks)
                    .await
                    .into_iter()
                    .collect::<Result<(), _>>()
            }
        };
        let key = CheckOutcome::of(&observed).metric_key();
        metrics::with_recorder(|recorder| recorder.register_counter(key, &CHECK_METRIC_METADATA))
            .increment(1);
        observed
    }
}

async fn check_probe(
    probe: &BoxedProbe,
    deadline: Instant,
    budget: Duration,
) -> Result<(), ProbeFailure> {
    let probe_name = probe.name();
    let result = tokio::time::timeout_at(deadline, probe.check()).await;
    let outcome = match result {
        Ok(probe_result) if Instant::now() <= deadline => {
            probe_result.map_err(|error| ProbeFailure::Failed {
                probe: probe_name,
                error,
            })
        }
        _ => Err(ProbeFailure::TimedOut {
            probe: probe_name,
            budget,
        }),
    };
    metrics::counter!(
        PROBE_CHECKS_METRIC,
        "probe" => probe_name,
        "outcome" => CheckOutcome::of(&outcome).label()
    )
    .increment(1);
    outcome
}

impl ReadinessReader {
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.rx.borrow().readiness().0
    }

    pub fn verdict(&self) -> Result<(), NotReady> {
        self.rx.borrow().verdict(self.stale_after)
    }

    // template:begin grpc:health-changed-verdict
    pub async fn wait_for_drain_or_owner_drop(&mut self) {
        let _ = self
            .rx
            .wait_for(|state| matches!(state, State::Draining))
            .await;
    }

    pub async fn wait_until_ready_is_not(&mut self, seen: bool) -> Option<bool> {
        loop {
            let stale_deadline = {
                let state = self.rx.borrow_and_update();
                let (ready, stale_deadline) = state.readiness();
                if ready != seen {
                    return Some(ready);
                }
                if matches!(*state, State::Draining) {
                    return None;
                }
                stale_deadline
            };
            match stale_deadline {
                Some(deadline) => {
                    if let Ok(recv_result) =
                        tokio::time::timeout_at(deadline, self.rx.changed()).await
                    {
                        recv_result.ok()?;
                    }
                }
                None => self.rx.changed().await.ok()?,
            }
        }
    }
    // template:end grpc:health-changed-verdict
}

#[cfg(test)]
mod tests;
