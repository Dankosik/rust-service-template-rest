use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::join_all;
use health::ReadinessControl;
use infra_http_transport::{Drained, Server};
use infra_telemetry::{
    LoggerGuard, LoggerShutdown, ProviderShutdown, SHUTDOWN_JOIN_SLACK, TracerProviderHandle,
};
use infra_temporal::worker::WorkerFatalError;
use infra_temporal::{CloseOutcome, TemporalStartup};
use service_config::{ShutdownTail, TemporalWorkerRuntimeConfig, WorkerHttpConfig};
use tokio::time::Instant;

use crate::background::Background;
use crate::resources::{CloseHook, Dependencies, Listeners, Resources, Telemetry, Work};
use crate::signals::Signals;

const LISTENER_CLOSE_BUDGET: Duration = Duration::from_secs(2);
const BACKGROUND_JOIN: Duration = Duration::from_secs(3);
const DEPENDENCY_CLOSE_BUDGET: Duration = Duration::from_secs(5);
const TELEMETRY_FLUSH: Duration = Duration::from_secs(5);
const TELEMETRY_STAGE: Duration = TELEMETRY_FLUSH.saturating_add(SHUTDOWN_JOIN_SLACK);
const LOG_CLOSE_RESERVE: Duration = Duration::from_secs(1);

pub(crate) const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

pub const TEMPORAL_SHUTDOWN_TAIL: Duration = LISTENER_CLOSE_BUDGET
    .saturating_add(BACKGROUND_JOIN)
    .saturating_add(DEPENDENCY_CLOSE_BUDGET)
    .saturating_add(TELEMETRY_STAGE)
    .saturating_add(RUNTIME_SHUTDOWN_TIMEOUT);

pub(crate) const TAIL: ShutdownTail = ShutdownTail::new(
    TEMPORAL_SHUTDOWN_TAIL,
    "temporal worker teardown tail (listeners, background join, dependency close, telemetry flush, SDK join slack, runtime shutdown)",
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Outcome {
    Graceful,
    Degraded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShutdownReport {
    pub(crate) outcome: Outcome,
    pub(crate) worker_fatal: Option<WorkerFatalError>,
}

pub(crate) fn stop_deadline(first_stop: Option<Instant>, grace: Duration) -> Instant {
    first_stop.unwrap_or_else(Instant::now) + grace
}

struct Budget {
    deadline: Instant,
}

impl Budget {
    fn for_process_deadline(process_deadline: Instant) -> Self {
        Self {
            deadline: process_deadline - RUNTIME_SHUTDOWN_TIMEOUT,
        }
    }

    fn clamp_at(&self, now: Instant, want: Duration) -> Duration {
        want.min(self.deadline.saturating_duration_since(now))
    }

    fn clamp(&self, want: Duration) -> Duration {
        self.clamp_at(Instant::now(), want)
    }

    fn stage_deadline(&self, want: Duration) -> Instant {
        let now = Instant::now();
        now + self.clamp_at(now, want)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DrainBudgets {
    pub(crate) graceful_shutdown_period: Duration,
    pub(crate) activity_cancel_cleanup: Duration,
    pub(crate) finalize_budget: Duration,
}

impl DrainBudgets {
    pub(crate) const fn from_worker(worker: &TemporalWorkerRuntimeConfig) -> Self {
        Self {
            graceful_shutdown_period: worker.graceful_shutdown_period(),
            activity_cancel_cleanup: worker.activity_cancel_cleanup(),
            finalize_budget: worker.finalize_budget(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DrainDeadlines {
    drain_budget: Duration,
    drain_deadline: Instant,
    preempt_deadline: Instant,
}

fn drain_deadlines(
    drain_start: Instant,
    drain_timeout: Duration,
    drain: DrainBudgets,
    budget: &Budget,
) -> DrainDeadlines {
    let drain_budget = budget.clamp_at(drain_start, drain_timeout);
    let drain_deadline = drain_start + drain_budget;
    let cooperative_window = drain
        .graceful_shutdown_period
        .saturating_add(drain.activity_cancel_cleanup);
    let preempt_deadline = drain_start
        .checked_add(cooperative_window)
        .map_or(drain_deadline, |deadline| deadline.min(drain_deadline));
    DrainDeadlines {
        drain_budget,
        drain_deadline,
        preempt_deadline,
    }
}

fn forced_finalize_deadline(
    now: Instant,
    finalize_budget: Duration,
    drain_deadline: Instant,
) -> Instant {
    now.checked_add(finalize_budget)
        .map_or(drain_deadline, |deadline| deadline.min(drain_deadline))
}

pub(crate) struct Plan<'a> {
    pub(crate) http: &'a WorkerHttpConfig,
    pub(crate) drain: DrainBudgets,
    pub(crate) resources: Resources,
    pub(crate) background: Background,
    pub(crate) signals: &'a mut Signals,
    pub(crate) deadline: Instant,
}

pub(crate) async fn run(plan: Plan<'_>) -> ShutdownReport {
    let Plan {
        http,
        drain,
        resources,
        background,
        signals,
        deadline,
    } = plan;
    let Resources {
        telemetry,
        mut work,
        dependencies,
        listeners,
        readiness,
    } = resources;
    let budget = Budget::for_process_deadline(deadline);
    let mut outcome = isolate_stage("stop_work", async {
        stop_work(http, readiness.as_ref(), &work);
        Outcome::Graceful
    })
    .await;
    let (drain_outcome, worker_fatal) =
        drain_work(&mut work, http.drain_timeout, drain, &budget, signals).await;
    outcome = outcome.max(drain_outcome);
    let listeners_outcome = close_listeners(listeners, budget.clamp(LISTENER_CLOSE_BUDGET)).await;
    outcome = outcome.max(listeners_outcome);
    drop(readiness);
    let background_outcome = isolate_stage(
        "background_join",
        join_background(&background, budget.clamp(BACKGROUND_JOIN)),
    )
    .await;
    let dependency_deadline = budget.stage_deadline(DEPENDENCY_CLOSE_BUDGET);
    let background_outcome =
        finish_background(&background, background_outcome, dependency_deadline).await;
    outcome = outcome.max(background_outcome);
    let dependencies_outcome = close_dependencies(dependencies, dependency_deadline).await;
    outcome = outcome.max(dependencies_outcome);
    if background.failure().is_some() {
        outcome = Outcome::Degraded;
    }
    if let Err(error) = signals.take_pending() {
        tracing::error!(error = %error, "stop signal receiver failed during shutdown");
        outcome = Outcome::Degraded;
    }
    let outcome =
        finish_telemetry(telemetry, budget.stage_deadline(TELEMETRY_STAGE), outcome).await;
    ShutdownReport {
        outcome,
        worker_fatal,
    }
}

enum DrainRace {
    Drained,
    Forced(&'static str, Option<String>),
}

async fn drain_work(
    work: &mut Work,
    drain_timeout: Duration,
    drain: DrainBudgets,
    budget: &Budget,
    signals: &mut Signals,
) -> (Outcome, Option<WorkerFatalError>) {
    isolate_stage_or(
        "drain",
        drain_temporal(work, drain_timeout, drain, budget, signals),
        (Outcome::Degraded, None),
    )
    .await
}

async fn drain_temporal(
    work: &mut Work,
    drain_timeout: Duration,
    drain: DrainBudgets,
    budget: &Budget,
    signals: &mut Signals,
) -> (Outcome, Option<WorkerFatalError>) {
    let Some(mut handle) = work.temporal.take() else {
        return (Outcome::Graceful, None);
    };
    let drain_start = Instant::now();
    let DrainDeadlines {
        drain_budget,
        drain_deadline,
        preempt_deadline,
    } = drain_deadlines(drain_start, drain_timeout, drain, budget);
    tracing::info!(
        budget = ?drain_budget,
        preempt_budget = ?preempt_deadline.saturating_duration_since(drain_start),
        "temporal_drain_started"
    );
    let race = tokio::select! {
        biased;
        () = handle.drained() => DrainRace::Drained,
        () = tokio::time::sleep_until(preempt_deadline) => DrainRace::Forced("budget", None),
        result = signals.wait() => match result {
            Ok(()) => DrainRace::Forced("second_signal", None),
            Err(error) => DrainRace::Forced("signal_receiver_failed", Some(error.to_string())),
        },
    };
    match race {
        DrainRace::Drained => match handle.finish(drain_deadline).await {
            Ok(CloseOutcome::Complete) => {
                tracing::info!("temporal_drain_completed");
                (Outcome::Graceful, None)
            }
            Ok(CloseOutcome::TimedOut) => {
                tracing::warn!("temporal_drain_finish_timed_out");
                (Outcome::Degraded, None)
            }
            Err(fatal) => {
                tracing::error!(error = %fatal, "temporal_worker_finish_failed");
                (Outcome::Degraded, Some(fatal))
            }
        },
        DrainRace::Forced(reason, error) => {
            handle.force_cancel();
            tracing::warn!(reason, error, "temporal_drain_forced");
            let finalize_deadline =
                forced_finalize_deadline(Instant::now(), drain.finalize_budget, drain_deadline);
            match handle.finish(finalize_deadline).await {
                Ok(CloseOutcome::Complete) => (Outcome::Degraded, None),
                Ok(CloseOutcome::TimedOut) => {
                    tracing::warn!("temporal_forced_finish_timed_out");
                    (Outcome::Degraded, None)
                }
                Err(fatal) => {
                    tracing::error!(error = %fatal, "temporal_worker_finish_failed");
                    (Outcome::Degraded, Some(fatal))
                }
            }
        }
    }
}

async fn finish_background(
    background: &Background,
    mut outcome: Outcome,
    abort_deadline: Instant,
) -> Outcome {
    if !background.is_empty() {
        outcome = Outcome::Degraded;
        background.abort();
        tracing::warn!("background join expired; abort requested");
        isolate_stage_or(
            "background_abort",
            wait_for_background_abort(background, abort_deadline),
            (),
        )
        .await;
    }
    if outcome == Outcome::Degraded {
        tracing::warn!("background_join_failed");
    } else {
        tracing::info!("background_joined");
    }
    outcome
}

async fn isolate_stage(name: &'static str, operation: impl Future<Output = Outcome>) -> Outcome {
    isolate_stage_or(name, operation, Outcome::Degraded).await
}

async fn isolate_stage_or<T>(
    name: &'static str,
    operation: impl Future<Output = T>,
    on_panic: T,
) -> T {
    if let Ok(outcome) = AssertUnwindSafe(operation).catch_unwind().await {
        outcome
    } else {
        tracing::error!(stage = name, "shutdown_stage_panicked");
        on_panic
    }
}

fn stop_work(http: &WorkerHttpConfig, readiness: Option<&ReadinessControl>, work: &Work) {
    tracing::info!(grace = ?http.grace_period, "shutdown_started");
    if let Some(readiness) = readiness {
        readiness.start_drain();
        tracing::info!("readiness_disabled");
    }
    if let Some(worker) = &work.temporal {
        worker.initiate_shutdown();
        tracing::info!("temporal_polling_stopped");
    }
}

async fn close_listeners(listeners: Listeners, budget: Duration) -> Outcome {
    let Listeners {
        health,
        diagnostics,
    } = listeners;
    let (health_outcome, diagnostics_outcome) = tokio::join!(
        isolate_stage(Listeners::HEALTH, close_health(health, budget)),
        isolate_stage(
            Listeners::DIAGNOSTICS,
            close_diagnostics(diagnostics, budget)
        ),
    );
    health_outcome.max(diagnostics_outcome)
}

async fn close_health(server: Option<Server>, budget: Duration) -> Outcome {
    let Some(server) = server else {
        return Outcome::Graceful;
    };
    match server.drain(budget).await {
        Ok(Drained::Complete) => {
            tracing::info!("health_listener_stopped");
            Outcome::Graceful
        }
        Ok(Drained::TimedOut {
            remaining_at_deadline,
            ..
        }) => {
            tracing::warn!(
                remaining = remaining_at_deadline,
                "health listener outlived its close budget"
            );
            Outcome::Degraded
        }
        Err(err) => {
            tracing::error!(error = %err, "health listener drain failed");
            Outcome::Degraded
        }
    }
}

async fn close_diagnostics(server: Option<Server>, budget: Duration) -> Outcome {
    let Some(server) = server else {
        return Outcome::Graceful;
    };
    match server.drain(budget).await {
        Ok(Drained::Complete) => tracing::info!("diagnostics_stopped"),
        Ok(Drained::TimedOut { .. }) => {
            tracing::warn!(
                reason = "scrape_outlived_shutdown_budget",
                "diagnostics_forced"
            );
        }
        Err(err) => {
            tracing::warn!(error = %err, "diagnostics_shutdown_failed");
            return Outcome::Degraded;
        }
    }
    Outcome::Graceful
}

async fn join_background(background: &Background, budget: Duration) -> Outcome {
    if background.join_within(budget).await {
        Outcome::Graceful
    } else {
        Outcome::Degraded
    }
}

async fn wait_for_background_abort(background: &Background, deadline: Instant) {
    if background.wait_until(deadline).await {
        tracing::warn!("background_abort_acknowledged");
    } else {
        tracing::warn!("background_completion_unconfirmed");
    }
}

async fn close_dependencies(dependencies: Dependencies, deadline: Instant) -> Outcome {
    let Dependencies {
        temporal,
        close_hooks,
    } = dependencies;
    let hooks_outcome = close_registered_hooks(close_hooks, deadline).await;
    let temporal_outcome = match temporal {
        Some(startup) => isolate_stage("temporal_close", close_temporal(startup, deadline)).await,
        None => Outcome::Graceful,
    };
    hooks_outcome.max(temporal_outcome)
}

async fn close_registered_hooks(hooks: Vec<CloseHook>, deadline: Instant) -> Outcome {
    if hooks.is_empty() {
        return Outcome::Graceful;
    }
    let outcomes = join_all(hooks.into_iter().map(|hook| {
        isolate_stage("close_hook", async move {
            match tokio::time::timeout_at(deadline, hook(deadline)).await {
                Ok(CloseOutcome::Complete) => Outcome::Graceful,
                Ok(CloseOutcome::TimedOut) | Err(_) => {
                    tracing::warn!("registered close hook outlived its close budget");
                    Outcome::Degraded
                }
            }
        })
    }))
    .await;
    outcomes.into_iter().fold(Outcome::Graceful, Ord::max)
}

async fn close_temporal(startup: TemporalStartup, deadline: Instant) -> Outcome {
    match startup.close(deadline).await {
        CloseOutcome::Complete => {
            tracing::info!("temporal_closed");
            Outcome::Graceful
        }
        CloseOutcome::TimedOut => {
            tracing::warn!("temporal connection outlived its close budget");
            Outcome::Degraded
        }
    }
}

async fn finish_telemetry(
    telemetry: Telemetry,
    deadline: Instant,
    mut outcome: Outcome,
) -> Outcome {
    let Telemetry {
        tracer_provider,
        logger,
    } = telemetry;
    let log_reserve = if let Some(logger) = &logger {
        logger.begin_shutdown();
        LOG_CLOSE_RESERVE.min(deadline.saturating_duration_since(Instant::now()))
    } else {
        Duration::ZERO
    };
    if let Some(provider) = tracer_provider {
        let trace_outcome = close_tracer_provider(provider, deadline - log_reserve).await;
        outcome = outcome.max(trace_outcome);
    }
    tracing::info!(?outcome, logger_pending = true, "shutdown_finishing");
    if let Some(logger) = logger {
        let logger_outcome = close_logger(logger, deadline).await;
        outcome = outcome.max(logger_outcome);
    }
    outcome
}

async fn close_tracer_provider(provider: TracerProviderHandle, deadline: Instant) -> Outcome {
    isolate_stage("telemetry", async {
        match provider.shutdown(deadline).await {
            ProviderShutdown::Completed => {
                tracing::info!(delivery_confirmed = false, "trace_shutdown_completed");
                Outcome::Graceful
            }
            ProviderShutdown::Incomplete(reasons) => {
                tracing::warn!(
                    reasons = reasons.bits(),
                    reason_flags = ?reasons,
                    "trace_shutdown_incomplete"
                );
                Outcome::Degraded
            }
        }
    })
    .await
}

async fn close_logger(logger: LoggerGuard, deadline: Instant) -> Outcome {
    isolate_stage("logger", async {
        match logger.shutdown_async(deadline).await {
            LoggerShutdown::Completed => Outcome::Graceful,
            LoggerShutdown::Incomplete(_) => Outcome::Degraded,
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    fn worker_config(
        environment: &[(&str, &str)],
    ) -> Result<service_config::TemporalWorkerConfig, service_config::Error> {
        service_config::test_support::temporal_worker(environment, crate::BUILD_INFO, TAIL)
    }

    #[test]
    fn shutdown_tail_matches_budget_arithmetic_and_leaves_platform_margin() {
        assert_eq!(TEMPORAL_SHUTDOWN_TAIL, Duration::from_millis(16_500));
        let config = worker_config(&[]).unwrap();
        let http = config.http();
        assert_eq!(http.grace_period, Duration::from_secs(90));
        assert_eq!(http.drain_timeout, Duration::from_mins(1));
        assert_eq!(
            http.grace_period
                .checked_sub(http.drain_timeout)
                .and_then(|left| left.checked_sub(TEMPORAL_SHUTDOWN_TAIL)),
            Some(Duration::from_millis(13_500))
        );
    }

    #[test]
    fn grace_budget_boundary_is_drain_plus_tail() {
        worker_config(&[("APP__HTTP__GRACE_PERIOD", "76.5s")]).unwrap();
        let refused = worker_config(&[("APP__HTTP__GRACE_PERIOD", "76.499999999s")]);
        assert!(
            refused
                .unwrap_err()
                .validation_errors()
                .is_some_and(|errors| errors.names("http.grace_period"))
        );
    }

    #[tokio::test]
    async fn close_hooks_run_and_degrade_on_timeout_or_panic() {
        let called = Arc::new(AtomicBool::new(false));
        let called_flag = Arc::clone(&called);
        let hooks: Vec<CloseHook> = vec![
            Box::new(move |_deadline| {
                Box::pin(async move {
                    called_flag.store(true, Ordering::Release);
                    CloseOutcome::Complete
                })
            }),
            Box::new(|_deadline| Box::pin(async { CloseOutcome::TimedOut })),
        ];
        let outcome = close_registered_hooks(hooks, Instant::now() + Duration::from_secs(1)).await;
        assert!(called.load(Ordering::Acquire));
        assert_eq!(outcome, Outcome::Degraded);

        let panicking: Vec<CloseHook> = vec![Box::new(|_deadline| {
            Box::pin(async {
                panic!("hook defect");
            })
        })];
        let outcome =
            close_registered_hooks(panicking, Instant::now() + Duration::from_secs(1)).await;
        assert_eq!(outcome, Outcome::Degraded);
    }

    #[tokio::test(start_paused = true)]
    async fn stages_are_clamped_to_the_remaining_deadline() {
        let budget = Budget::for_process_deadline(Instant::now() + Duration::from_secs(11));
        assert_eq!(budget.clamp(Duration::from_secs(4)), Duration::from_secs(4));
        tokio::time::advance(Duration::from_secs(8)).await;
        assert_eq!(budget.clamp(Duration::from_secs(4)), Duration::from_secs(2));
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(budget.clamp(Duration::from_secs(4)), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn ignored_background_cancellation_is_aborted_within_budget() {
        let mut background = Background::new();
        let (ended, observed_end) = tokio::sync::oneshot::channel::<()>();
        background.spawn("ignores_stop", |_cancel| async move {
            let _held = ended;
            std::future::pending::<()>().await;
        });
        let tracker = background.tracker();
        let config = worker_config(&[]).unwrap();
        let http = config.http();
        let mut signals = Signals::install().unwrap();
        let started = Instant::now();
        let report = tokio::time::timeout(
            Duration::from_secs(9),
            run(Plan {
                http,
                drain: DrainBudgets::from_worker(config.temporal_worker()),
                resources: Resources::default(),
                background,
                signals: &mut signals,
                deadline: started + http.grace_period,
            }),
        )
        .await
        .unwrap();
        assert_eq!(report.outcome, Outcome::Degraded);
        assert!(report.worker_fatal.is_none());
        assert_eq!(
            Instant::now().duration_since(started),
            Duration::from_secs(3)
        );
        assert!(tracker.is_empty());
        std::assert_matches!(observed_end.now_or_never(), Some(Err(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn nested_drain_deadlines_never_starve_shutdown_tail_under_tight_admitted_configs() {
        let cases = [
            (
                &[("APP__HTTP__GRACE_PERIOD", "76.5s")][..],
                Duration::from_secs(55),
                Duration::from_mins(1),
                Duration::from_secs(5),
            ),
            (
                &[
                    ("APP__TEMPORAL_WORKER__GRACEFUL_SHUTDOWN_PERIOD", "5s"),
                    ("APP__TEMPORAL_WORKER__ACTIVITY_CANCEL_CLEANUP", "5s"),
                    ("APP__TEMPORAL_WORKER__FINALIZE_BUDGET", "20s"),
                    ("APP__HTTP__DRAIN_TIMEOUT", "30s"),
                    ("APP__HTTP__GRACE_PERIOD", "46.5s"),
                ][..],
                Duration::from_secs(10),
                Duration::from_secs(30),
                Duration::from_secs(20),
            ),
        ];

        for (env, expected_preempt, expected_drain, expected_finalize) in cases {
            let config = worker_config(env).unwrap();
            let http = config.http();
            let drain = DrainBudgets::from_worker(config.temporal_worker());
            let start = Instant::now();
            let budget = Budget::for_process_deadline(start + http.grace_period);
            let deadlines = drain_deadlines(start, http.drain_timeout, drain, &budget);

            assert_eq!(deadlines.drain_budget, expected_drain);
            assert_eq!(deadlines.drain_deadline, start + expected_drain);
            assert_eq!(deadlines.preempt_deadline, start + expected_preempt);

            let budget_forced_finalize = forced_finalize_deadline(
                deadlines.preempt_deadline,
                drain.finalize_budget,
                deadlines.drain_deadline,
            );
            assert_eq!(
                budget_forced_finalize,
                deadlines.preempt_deadline + expected_finalize
            );
            assert!(budget_forced_finalize <= deadlines.drain_deadline);

            let early_signal_at = start + Duration::from_secs(1);
            let early_forced_finalize = forced_finalize_deadline(
                early_signal_at,
                drain.finalize_budget,
                deadlines.drain_deadline,
            );
            assert_eq!(early_forced_finalize, early_signal_at + expected_finalize);
            assert!(early_forced_finalize <= deadlines.drain_deadline);

            let remaining_for_tail = budget
                .deadline
                .saturating_duration_since(deadlines.drain_deadline);
            assert_eq!(
                remaining_for_tail + RUNTIME_SHUTDOWN_TIMEOUT,
                TEMPORAL_SHUTDOWN_TAIL
            );
        }
    }
}
