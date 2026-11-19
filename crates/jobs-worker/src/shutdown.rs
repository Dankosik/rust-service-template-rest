use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt;

use infra_http_transport::{Drained, Server};
// template:begin jobs:worker-shutdown-jobs-imports
use infra_jobs::Started;
// template:end jobs:worker-shutdown-jobs-imports
// template:begin messaging:worker-shutdown-messaging-imports
use infra_messaging::CloseOutcome;
// template:end messaging:worker-shutdown-messaging-imports
// template:begin jobs:worker-shutdown-postgres-imports
use infra_postgres::PgPool;
// template:end jobs:worker-shutdown-postgres-imports
use infra_telemetry::{
    LoggerGuard, LoggerShutdown, ProviderShutdown, SHUTDOWN_JOIN_SLACK, TracerProviderHandle,
};
use service_config::{ShutdownTail, WorkerHttpConfig};
use tokio::time::Instant;

use crate::background::Background;
// template:begin messaging:worker-shutdown-broker-import
use crate::resources::BrokerResource;
// template:end messaging:worker-shutdown-broker-import
use crate::resources::{Listeners, Resources};
use crate::signals::Signals;

const ATTEMPT_CLEANUP: Duration = Duration::from_secs(2);
const LISTENER_CLOSE_BUDGET: Duration = Duration::from_secs(2);
const BACKGROUND_JOIN: Duration = Duration::from_secs(3);
pub(crate) const DEPENDENCY_CLOSE_BUDGET: Duration = Duration::from_secs(5);
const TELEMETRY_FLUSH: Duration = Duration::from_secs(5);
const TELEMETRY_STAGE: Duration = TELEMETRY_FLUSH.saturating_add(SHUTDOWN_JOIN_SLACK);
const LOG_CLOSE_RESERVE: Duration = Duration::from_secs(1);
const SHUTDOWN_TAIL: Duration = ATTEMPT_CLEANUP
    .saturating_add(LISTENER_CLOSE_BUDGET)
    .saturating_add(BACKGROUND_JOIN)
    .saturating_add(DEPENDENCY_CLOSE_BUDGET)
    .saturating_add(TELEMETRY_STAGE)
    .saturating_add(RUNTIME_SHUTDOWN_TIMEOUT);

pub(crate) const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

pub(crate) const TAIL: ShutdownTail = ShutdownTail::new(
    SHUTDOWN_TAIL,
    "jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush, SDK join slack, runtime shutdown)",
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Outcome {
    Graceful,
    Degraded,
}

enum DrainOutcome {
    NoCleanupNeeded,
    NeedsForcedCleanup,
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

    fn clamp(&self, want: Duration) -> Duration {
        want.min(self.deadline.saturating_duration_since(Instant::now()))
    }

    fn stage_deadline(&self, want: Duration) -> Instant {
        Instant::now() + self.clamp(want)
    }
}

pub(crate) struct Plan<'a> {
    pub(crate) http: &'a WorkerHttpConfig,
    pub(crate) resources: Resources,
    pub(crate) background: Background,
    pub(crate) signals: &'a mut Signals,
    pub(crate) deadline: Instant,
}

pub(crate) async fn run(plan: Plan<'_>) -> Outcome {
    let Plan {
        http,
        mut resources,
        background,
        signals,
        deadline,
    } = plan;
    let budget = Budget::for_process_deadline(deadline);
    let mut outcome = isolate_stage("stop_work", async {
        stop_work(http, &resources);
        Outcome::Graceful
    })
    .await;
    let drain_outcome = drain_work(&mut resources, http.drain_timeout, &budget, signals).await;
    outcome = outcome.max(drain_outcome);
    let listeners_outcome = close_listeners(
        std::mem::take(&mut resources.listeners),
        budget.clamp(LISTENER_CLOSE_BUDGET),
    )
    .await;
    outcome = outcome.max(listeners_outcome);
    let background_outcome = isolate_stage(
        "background_join",
        join_background(&background, budget.clamp(BACKGROUND_JOIN)),
    )
    .await;
    let dependency_deadline = budget.stage_deadline(DEPENDENCY_CLOSE_BUDGET);
    let background_outcome = finish_background(
        &background,
        &resources,
        background_outcome,
        dependency_deadline,
    )
    .await;
    outcome = outcome.max(background_outcome);
    let dependencies_outcome = close_dependencies(&mut resources, dependency_deadline).await;
    outcome = outcome.max(dependencies_outcome);
    if background.failure().is_some() {
        outcome = Outcome::Degraded;
    }
    if let Err(error) = signals.take_pending() {
        tracing::error!(error = %error, "stop signal receiver failed during shutdown");
        outcome = Outcome::Degraded;
    }
    finish_telemetry(
        &mut resources,
        budget.stage_deadline(TELEMETRY_STAGE),
        outcome,
    )
    .await
}

async fn drain_work(
    resources: &mut Resources,
    drain_timeout: Duration,
    budget: &Budget,
    signals: &mut Signals,
) -> Outcome {
    let drain_outcome = isolate_stage_or(
        "drain",
        drain(resources, drain_timeout, budget, signals),
        DrainOutcome::NeedsForcedCleanup,
    )
    .await;
    let forced = matches!(drain_outcome, DrainOutcome::NeedsForcedCleanup);
    if forced {
        isolate_stage_or(
            "attempt_cleanup",
            clean_up_after_forced_drain(resources, budget.stage_deadline(ATTEMPT_CLEANUP)),
            (),
        )
        .await;
    }
    // template:begin messaging:worker-shutdown-drop-consumer
    drop(resources.consumer.take());
    // template:end messaging:worker-shutdown-drop-consumer
    if forced {
        Outcome::Degraded
    } else {
        Outcome::Graceful
    }
}

async fn finish_background(
    background: &Background,
    #[allow(
        unused_variables,
        reason = "the jobs profile inspects native engine failures"
    )]
    resources: &Resources,
    mut outcome: Outcome,
    abort_deadline: Instant,
) -> Outcome {
    if !background.tracker.is_empty() {
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
    // template:begin jobs:worker-shutdown-native-failures
    if outcome == Outcome::Graceful
        && resources
            .started_engine
            .as_ref()
            .is_some_and(|engine| engine.failed().now_or_never().is_some())
    {
        outcome = Outcome::Degraded;
    }
    // template:end jobs:worker-shutdown-native-failures
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

fn stop_work(http: &WorkerHttpConfig, resources: &Resources) {
    tracing::info!(grace = ?http.grace_period, "shutdown_started");
    if let Some(readiness) = resources.readiness.as_ref() {
        readiness.start_drain();
        tracing::info!("readiness_disabled");
    }
    // template:begin jobs:worker-shutdown-stop-jobs
    if let Some(started) = &resources.started_engine {
        started.stop_claiming();
        tracing::info!(in_flight = started.in_flight(), "claiming_stopped");
    }
    // template:end jobs:worker-shutdown-stop-jobs
    // template:begin messaging:worker-shutdown-stop-messaging
    if let Some(consumer) = &resources.consumer {
        consumer.drain();
        tracing::info!("messaging_pulls_stopped");
    }
    // template:end messaging:worker-shutdown-stop-messaging
}

async fn drain(
    resources: &mut Resources,
    drain_timeout: Duration,
    budget: &Budget,
    signals: &mut Signals,
) -> DrainOutcome {
    #[allow(unused_variables, reason = "retained profiles supply started work")]
    let work_started = false;
    // template:begin jobs:worker-shutdown-drain-has-jobs
    let work_started = resources.started_engine.is_some();
    // template:end jobs:worker-shutdown-drain-has-jobs
    // template:begin messaging:worker-shutdown-drain-has-messaging
    let work_started = work_started || resources.consumer.is_some();
    // template:end messaging:worker-shutdown-drain-has-messaging
    if !work_started {
        return DrainOutcome::NoCleanupNeeded;
    }
    let drain_budget = budget.clamp(drain_timeout);
    tracing::info!(
        budget = ?drain_budget,
        // template:begin jobs:worker-shutdown-drain-start-log
        in_flight = resources.started_engine.as_ref().map_or(0, Started::in_flight),
        // template:end jobs:worker-shutdown-drain-start-log
        "drain_started"
    );
    let deadline = Instant::now() + drain_budget;
    let forced = tokio::select! {
        biased;
        messaging_error = drain_active_work(resources, deadline) => {
            messaging_error.map(|error| ("messaging", Some(error)))
        }
        () = tokio::time::sleep_until(deadline) => Some(("budget", None)),
        result = signals.wait() => Some(match result {
            Ok(()) => ("second_signal", None),
            Err(error) => ("signal_receiver_failed", Some(error.to_string())),
        }),
    };
    match forced {
        None => {
            tracing::info!("drain_completed");
            DrainOutcome::NoCleanupNeeded
        }
        Some((reason, error)) => {
            tracing::warn!(
                // template:begin jobs:worker-shutdown-drain-forced-log
                in_flight = resources
                    .started_engine
                    .as_ref()
                    .map_or(0, Started::in_flight),
                // template:end jobs:worker-shutdown-drain-forced-log
                reason,
                error,
                "drain_forced"
            );
            DrainOutcome::NeedsForcedCleanup
        }
    }
}

async fn drain_active_work(
    #[allow(unused_variables, reason = "retained profiles drain started work")]
    resources: &mut Resources,
    #[allow(
        unused_variables,
        reason = "the messaging profile bounds consumer finish"
    )]
    deadline: Instant,
) -> Option<String> {
    let jobs = async {
        // template:begin jobs:worker-shutdown-drain-jobs
        if let Some(engine) = &resources.started_engine {
            engine.drained().await;
        }
        // template:end jobs:worker-shutdown-drain-jobs
    };
    let messages = async {
        // template:begin messaging:worker-shutdown-drain-messaging
        if let Some(consumer) = resources.consumer.as_mut() {
            return consumer
                .finish(deadline)
                .await
                .err()
                .map(|error| error.to_string());
        }
        // template:end messaging:worker-shutdown-drain-messaging
        None::<String>
    };
    let ((), messaging_error) = tokio::join!(jobs, messages);
    messaging_error
}

async fn clean_up_after_forced_drain(resources: &mut Resources, deadline: Instant) {
    let jobs = async {
        // template:begin jobs:worker-shutdown-finish-jobs
        if let Some(engine) = &resources.started_engine {
            finish_attempts(engine, deadline).await;
        }
        // template:end jobs:worker-shutdown-finish-jobs
    };
    let messages = async {
        // template:begin messaging:worker-shutdown-finish-messaging
        if let Some(consumer) = resources.consumer.as_mut() {
            consumer.abort();
            if let Err(error) = consumer.finish(deadline).await {
                tracing::warn!(%error, "messaging consumer cleanup failed");
            }
        }
        // template:end messaging:worker-shutdown-finish-messaging
    };
    if tokio::time::timeout_at(deadline, async {
        tokio::join!(jobs, messages);
    })
    .await
    .is_err()
    {
        tracing::warn!("attempt cleanup completion unconfirmed");
    }
}

// template:begin jobs:worker-shutdown-finish-attempts
async fn finish_attempts(started: &Started, deadline: Instant) {
    let end = started
        .cancel_and_finish(deadline.saturating_duration_since(Instant::now()))
        .await;
    if end.timed_out {
        tracing::warn!(
            cancelled = end.cancelled,
            released = end.released,
            known_results = end.known_results,
            uncertain = end.uncertain,
            timed_out = end.timed_out,
            "attempts_finished"
        );
    } else {
        tracing::info!(
            cancelled = end.cancelled,
            released = end.released,
            known_results = end.known_results,
            uncertain = end.uncertain,
            timed_out = end.timed_out,
            "attempts_finished"
        );
    }
}
// template:end jobs:worker-shutdown-finish-attempts

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
            remaining_connections,
        }) => {
            tracing::warn!(
                remaining = remaining_connections,
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
    background.cancel.cancel();
    background.tracker.close();
    if tokio::time::timeout(budget, background.tracker.wait())
        .await
        .is_err()
        || background.failure().is_some()
    {
        Outcome::Degraded
    } else {
        Outcome::Graceful
    }
}

async fn wait_for_background_abort(background: &Background, deadline: Instant) {
    if tokio::time::timeout_at(deadline, background.tracker.wait())
        .await
        .is_ok()
    {
        tracing::warn!("background_abort_acknowledged");
    } else {
        tracing::warn!("background_completion_unconfirmed");
    }
}

// template:begin jobs:worker-shutdown-close-pool
async fn close_pool(pool: &PgPool, budget: Duration) -> Outcome {
    match infra_postgres::close(pool, budget).await {
        infra_postgres::CloseOutcome::Complete => {
            tracing::info!("postgres_pool_closed");
            Outcome::Graceful
        }
        infra_postgres::CloseOutcome::TimedOut => {
            tracing::warn!("postgres pool outlived its close budget");
            Outcome::Degraded
        }
    }
}
// template:end jobs:worker-shutdown-close-pool

// template:begin messaging:worker-shutdown-close-messaging-fn
async fn close_messaging(broker: Option<BrokerResource>, deadline: Instant) -> Outcome {
    let cancel = tokio_util::sync::CancellationToken::new();
    let closed = match broker {
        Some(BrokerResource::Starting(startup)) => startup.close(deadline, &cancel).await,
        Some(BrokerResource::Admitted(messaging)) => messaging.close(deadline, &cancel).await,
        None => return Outcome::Graceful,
    };
    match closed {
        CloseOutcome::Complete => {
            tracing::info!("messaging_closed");
            Outcome::Graceful
        }
        CloseOutcome::WaitInterrupted | CloseOutcome::UnobservedClose => {
            tracing::warn!(outcome = ?closed, "messaging resource close incomplete");
            Outcome::Degraded
        }
    }
}
// template:end messaging:worker-shutdown-close-messaging-fn

async fn close_dependencies(resources: &mut Resources, deadline: Instant) -> Outcome {
    let pool_close = async {
        // template:begin jobs:worker-shutdown-close-jobs
        use crate::resources::PoolResource;

        if let Some(PoolResource::Starting(pool) | PoolResource::Admitted(pool)) =
            resources.pool.as_ref()
        {
            return close_pool(pool, deadline.saturating_duration_since(Instant::now())).await;
        }
        // template:end jobs:worker-shutdown-close-jobs
        Outcome::Graceful
    };
    #[allow(
        clippy::let_and_return,
        reason = "the messaging profile rebinds the outcome before it is returned"
    )]
    let messaging_close = async {
        #[allow(unused_variables, reason = "the messaging profile supplies the close")]
        let outcome = Outcome::Graceful;
        // template:begin messaging:worker-shutdown-close-messaging
        let outcome = close_messaging(resources.broker.take(), deadline).await;
        // template:end messaging:worker-shutdown-close-messaging
        outcome
    };
    let (pool_outcome, messaging_outcome) = tokio::join!(
        isolate_stage("postgres_close", pool_close),
        isolate_stage("messaging_close", messaging_close),
    );
    pool_outcome.max(messaging_outcome)
}

async fn finish_telemetry(
    resources: &mut Resources,
    deadline: Instant,
    mut outcome: Outcome,
) -> Outcome {
    let log_reserve = if let Some(logger) = &resources.logger {
        logger.begin_shutdown();
        LOG_CLOSE_RESERVE.min(deadline.saturating_duration_since(Instant::now()))
    } else {
        Duration::ZERO
    };
    if let Some(provider) = resources.tracer_provider.take() {
        let trace_outcome = close_tracer_provider(provider, deadline - log_reserve).await;
        outcome = outcome.max(trace_outcome);
    }
    tracing::info!(?outcome, logger_pending = true, "shutdown_finishing");
    if let Some(logger) = resources.logger.take() {
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
        let close = tokio::task::spawn_blocking(move || logger.shutdown(deadline.into_std()));
        if matches!(
            tokio::time::timeout_at(deadline, close).await,
            Ok(Ok(LoggerShutdown::Completed(_)))
        ) {
            Outcome::Graceful
        } else {
            Outcome::Degraded
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn worker_config(
        environment: &[(&str, &str)],
    ) -> Result<service_config::WorkerConfig, service_config::Error> {
        service_config::fixtures::worker(environment, crate::BUILD_INFO, TAIL)
    }

    #[test]
    fn default_budgets_leave_one_and_a_half_seconds() {
        let config = worker_config(&[]).unwrap();
        let http = &config.http;
        assert_eq!(
            http.grace_period
                .checked_sub(http.drain_timeout)
                .and_then(|left| left.checked_sub(SHUTDOWN_TAIL)),
            Some(Duration::from_millis(1_500))
        );
    }

    #[test]
    fn grace_budget_boundary_is_drain_plus_tail() {
        worker_config(&[("APP__HTTP__GRACE_PERIOD", "43.5s")]).unwrap();
        let refused = worker_config(&[("APP__HTTP__GRACE_PERIOD", "43.499999999s")]);
        std::assert_matches!(
            refused,
            Err(service_config::Error::Validate(errors)) if errors.names("http.grace_period")
        );
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

    #[tokio::test]
    async fn cooperative_background_completion_is_graceful() {
        let background = Background::new();
        background.spawn("waits", CancellationToken::cancelled_owned);
        assert_eq!(
            join_background(&background, BACKGROUND_JOIN).await,
            Outcome::Graceful
        );
        assert!(background.cancel.is_cancelled());
        assert!(background.tracker.is_closed());
        assert!(background.tracker.is_empty());
        assert!(
            background.failure().is_none(),
            "a task that ends at its cancellation is not a failure"
        );
    }

    #[tokio::test]
    async fn a_task_that_cancels_itself_and_returns_is_reported_by_name() {
        let background = Background::new();
        background.spawn("waits", CancellationToken::cancelled_owned);
        background.spawn("returns", |cancel| async move { cancel.cancel() });
        assert_eq!(
            tokio::time::timeout(BACKGROUND_JOIN, background.wait_for_failure())
                .await
                .unwrap(),
            "returns"
        );
        assert!(!background.cancel.is_cancelled());
        assert_eq!(
            join_background(&background, BACKGROUND_JOIN).await,
            Outcome::Degraded
        );
    }

    #[tokio::test]
    async fn panic_after_cancellation_degrades_common_shutdown() {
        let background = Background::new();
        background.spawn("cleanup_panics", |cancel| async move {
            cancel.cancelled().await;
            panic!("cleanup defect");
        });
        let tracker = background.tracker.clone();
        let failures = background.subscribe_failures();
        let config = worker_config(&[]).unwrap();
        let http = &config.http;
        let mut signals = Signals::install().unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            run(Plan {
                http,
                resources: Resources::default(),
                background,
                signals: &mut signals,
                deadline: Instant::now() + http.grace_period,
            }),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Degraded);
        assert!(tracker.is_empty());
        assert_eq!(*failures.borrow(), Some("cleanup_panics"));
    }

    #[tokio::test(start_paused = true)]
    async fn ignored_cancellation_is_aborted_and_acknowledged_before_shutdown_returns() {
        let background = Background::new();
        let (ended, observed_end) = tokio::sync::oneshot::channel::<()>();
        background.spawn("ignores_stop", |_cancel| async move {
            let _held = ended;
            std::future::pending::<()>().await;
        });
        let tracker = background.tracker.clone();
        let failures = background.subscribe_failures();
        let config = worker_config(&[]).unwrap();
        let http = &config.http;
        let mut signals = Signals::install().unwrap();
        let started = Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_secs(9),
            run(Plan {
                http,
                resources: Resources::default(),
                background,
                signals: &mut signals,
                deadline: started + http.grace_period,
            }),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Degraded);
        assert_eq!(
            Instant::now().duration_since(started),
            Duration::from_secs(3)
        );
        assert!(tracker.is_empty());
        std::assert_matches!(observed_end.now_or_never(), Some(Err(_)));
        assert!(
            failures.borrow().is_none(),
            "forced cancellation is not an unexpected return"
        );
    }
}
