//! Staged teardown under the original process deadline, including partial startup.
//!
//! Stage order: readiness off and claiming stopped, drain, cleanup after a
//! forced drain, listeners, background join, pool close, telemetry flush.
//! Every stage takes the lesser of its ceiling and what is left of
//! `http.grace_period`.

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt;

use health::Readiness;
use infra_http::{Drained, Server};
// template:begin jobs:worker-shutdown-jobs-imports
use infra_jobs::Started;
// template:end jobs:worker-shutdown-jobs-imports
// template:begin messaging:worker-shutdown-messaging-imports
use infra_messaging::{CloseOutcome, ConsumerHandle, Messaging, MessagingStartup};
// template:end messaging:worker-shutdown-messaging-imports
// template:begin jobs:worker-shutdown-postgres-imports
use infra_postgres::PgPool;
// template:end jobs:worker-shutdown-postgres-imports
use infra_telemetry::{
    LoggerGuard, LoggerShutdown, ProviderShutdown, SHUTDOWN_JOIN_SLACK, TracerProviderHandle,
};
use service_config::HttpConfig;
use tokio::time::Instant;

use crate::background::Background;
use crate::signals::Signals;

/// Ceiling for finishing attempts after a forced drain.
const ATTEMPT_CLEANUP: Duration = Duration::from_secs(2);
const LISTENERS: Duration = Duration::from_secs(2);
const BACKGROUND_JOIN: Duration = Duration::from_secs(3);
pub(crate) const DEPENDENCY_CLOSE: Duration = Duration::from_secs(5);
const TELEMETRY_FLUSH: Duration = Duration::from_secs(5);
/// The telemetry stage: the flush plus the SDK's join slack.
const TELEMETRY_STAGE: Duration = TELEMETRY_FLUSH.saturating_add(SHUTDOWN_JOIN_SLACK);
/// Part of [`TELEMETRY_FLUSH`] kept for log closure after the trace flush.
const LOG_CLOSE_RESERVE: Duration = Duration::from_secs(1);
/// Whole process tail, including the runtime shutdown reserve.
const SHUTDOWN_TAIL: Duration = ATTEMPT_CLEANUP
    .saturating_add(LISTENERS)
    .saturating_add(BACKGROUND_JOIN)
    .saturating_add(DEPENDENCY_CLOSE)
    .saturating_add(TELEMETRY_STAGE)
    .saturating_add(RUNTIME_SHUTDOWN_TIMEOUT);

/// Final bounded runtime wait, reserved inside the process grace period.
/// Its return does not establish that already-running blocking work terminated.
pub(crate) const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
#[error(
    "http.grace_period ({grace:?}) must be >= http.drain_timeout ({drain_timeout:?}) plus the \
     {tail:?} jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush, SDK join slack, runtime shutdown)"
)]
pub(crate) struct GraceBudgetError {
    grace: Duration,
    drain_timeout: Duration,
    tail: Duration,
}

/// Reject a grace period that cannot hold the drain plus the teardown tail.
pub(crate) fn validate_grace_budget(http: &HttpConfig) -> Result<(), GraceBudgetError> {
    if http.grace_period < http.drain_timeout.saturating_add(SHUTDOWN_TAIL) {
        return Err(GraceBudgetError {
            grace: http.grace_period,
            drain_timeout: http.drain_timeout,
            tail: SHUTDOWN_TAIL,
        });
    }
    Ok(())
}

/// How the teardown ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Graceful,
    /// A stage overran or failed, a background task or the signal receiver
    /// failed, or telemetry did not close; the process still exited on its own.
    Degraded,
}

/// The process deadline: `grace` after the first stop, or after now when
/// no stop arrived.
pub(crate) fn stop_deadline(first_stop: Option<Instant>, grace: Duration) -> Instant {
    first_stop.unwrap_or_else(Instant::now) + grace
}

/// Async stages reserve the final runtime allowance inside the process deadline.
struct Budget {
    deadline: Instant,
}

impl Budget {
    fn until(process_deadline: Instant) -> Self {
        Self {
            deadline: process_deadline - RUNTIME_SHUTDOWN_TIMEOUT,
        }
    }

    fn remaining(&self, want: Duration) -> Duration {
        want.min(self.deadline.saturating_duration_since(Instant::now()))
    }

    fn stage_deadline(&self, want: Duration) -> Instant {
        Instant::now() + self.remaining(want)
    }
}

/// The listeners bound so far. Common shutdown closes whichever are present.
#[derive(Debug, Default)]
pub(crate) struct Listeners {
    pub(crate) health: Option<Server>,
    pub(crate) diagnostics: Option<Server>,
}

impl Listeners {
    /// Names each listener's watch task, its stop failure, and its close stage.
    pub(crate) const HEALTH: &'static str = "health_listener";
    pub(crate) const DIAGNOSTICS: &'static str = "diagnostics_listener";
}

/// What startup opened and teardown closes.
#[derive(Default)]
pub(crate) struct Resources {
    pub(crate) tracer_provider: Option<TracerProviderHandle>,
    pub(crate) logger: Option<LoggerGuard>,
    // template:begin jobs:worker-shutdown-resources-started
    pub(crate) started: Vec<Started>,
    // template:end jobs:worker-shutdown-resources-started
    // template:begin messaging:worker-shutdown-resources-messaging
    pub(crate) consumer: Option<ConsumerHandle>,
    pub(crate) messaging: Option<Messaging>,
    pub(crate) messaging_startup: Option<MessagingStartup>,
    // template:end messaging:worker-shutdown-resources-messaging
    // template:begin jobs:worker-shutdown-resources-pool
    pub(crate) pool: Option<PgPool>,
    // template:end jobs:worker-shutdown-resources-pool
    pub(crate) listeners: Listeners,
    pub(crate) readiness: Option<Readiness>,
}

/// Concrete partial resources and the first stop/failure deadline.
pub(crate) struct Plan<'a> {
    pub(crate) http: &'a HttpConfig,
    pub(crate) resources: Resources,
    pub(crate) background: Background,
    pub(crate) signals: &'a mut Signals,
    pub(crate) deadline: Instant,
}

/// Each independent stage survives an unwind; every wait spends the same deadline.
pub(crate) async fn run(plan: Plan<'_>) -> Outcome {
    let Plan {
        http,
        mut resources,
        background,
        signals,
        deadline,
    } = plan;
    let budget = Budget::until(deadline);
    let mut degraded = stage("stop_work", async {
        stop_work(http, &resources);
        false
    })
    .await;
    let forced = stage(
        "drain",
        drain(&mut resources, http.drain_timeout, &budget, signals),
    )
    .await;
    degraded |= forced;
    if forced {
        degraded |= stage(
            "attempt_cleanup",
            clean_up_after_forced_drain(&mut resources, budget.stage_deadline(ATTEMPT_CLEANUP)),
        )
        .await;
    }
    // template:begin messaging:worker-shutdown-drop-consumer
    // The native owner requests abort on Drop; only finish above establishes completion.
    drop(resources.consumer.take());
    // template:end messaging:worker-shutdown-drop-consumer
    degraded |= close_listeners(
        std::mem::take(&mut resources.listeners),
        budget.remaining(LISTENERS),
    )
    .await;
    let background_failed = stage(
        "background_join",
        join_background(&background, budget.remaining(BACKGROUND_JOIN)),
    )
    .await;
    // Forced acknowledgement and dependency close share this single allocation.
    let dependency_deadline = budget.stage_deadline(DEPENDENCY_CLOSE);
    if !background.tracker.is_empty() {
        background.abort();
        tracing::warn!("background join expired; abort requested");
        degraded |= stage(
            "background_abort",
            finish_background(&background, dependency_deadline),
        )
        .await;
    }
    // template:begin jobs:worker-shutdown-native-failures
    let background_failed = background_failed
        || resources
            .started
            .iter()
            .any(|engine| engine.failed().now_or_never().is_some());
    // template:end jobs:worker-shutdown-native-failures
    degraded |= background_failed;
    if background_failed {
        tracing::warn!("background_join_failed");
    } else {
        tracing::info!("background_joined");
    }
    degraded |= close_dependencies(&mut resources, dependency_deadline).await;
    degraded |= background.failure().is_some();
    if signals.take_pending().is_err() {
        tracing::error!("stop signal receiver failed during shutdown");
        degraded = true;
    }
    finish_telemetry(
        &mut resources,
        budget.stage_deadline(TELEMETRY_STAGE),
        degraded,
    )
    .await
}

/// Run one teardown stage. `true`, or an unwind, votes for a degraded stop.
async fn stage(name: &'static str, operation: impl Future<Output = bool>) -> bool {
    if let Ok(degraded) = AssertUnwindSafe(operation).catch_unwind().await {
        degraded
    } else {
        tracing::error!(stage = name, "shutdown_stage_panicked");
        true
    }
}

fn stop_work(http: &HttpConfig, resources: &Resources) {
    tracing::info!(grace = ?http.grace_period, "shutdown_started");
    if let Some(readiness) = resources.readiness.as_ref() {
        readiness.start_drain();
        tracing::info!("readiness_disabled");
    }
    // template:begin jobs:worker-shutdown-stop-jobs
    for started in &resources.started {
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
) -> bool {
    #[allow(unused_variables, reason = "retained profiles supply started work")]
    let work_started = false;
    // template:begin jobs:worker-shutdown-drain-has-jobs
    let work_started = !resources.started.is_empty();
    // template:end jobs:worker-shutdown-drain-has-jobs
    // template:begin messaging:worker-shutdown-drain-has-messaging
    let work_started = work_started || resources.consumer.is_some();
    // template:end messaging:worker-shutdown-drain-has-messaging
    if !work_started {
        return false;
    }
    let drain_budget = budget.remaining(drain_timeout);
    tracing::info!(
        budget = ?drain_budget,
        // template:begin jobs:worker-shutdown-drain-start-log
        in_flight = resources.started.iter().map(Started::in_flight).sum::<usize>(),
        // template:end jobs:worker-shutdown-drain-start-log
        "drain_started"
    );
    let deadline = Instant::now() + drain_budget;
    let joined = async {
        let jobs = async {
            // template:begin jobs:worker-shutdown-drain-jobs
            futures_util::future::join_all(resources.started.iter().map(Started::drained)).await;
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
    };
    // A drain that completed together with the deadline or a second signal
    // counts as completed.
    let forced = tokio::select! {
        biased;
        messaging_error = joined => messaging_error.map(|error| ("messaging", Some(error))),
        () = tokio::time::sleep_until(deadline) => Some(("budget", None)),
        result = signals.wait() => Some((if result.is_ok() { "second_signal" } else { "signal_receiver_failed" }, None)),
    };
    match forced {
        None => {
            tracing::info!("drain_completed");
            false
        }
        Some((reason, error)) => {
            tracing::warn!(
                // template:begin jobs:worker-shutdown-drain-forced-log
                in_flight = resources
                    .started
                    .iter()
                    .map(Started::in_flight)
                    .sum::<usize>(),
                // template:end jobs:worker-shutdown-drain-forced-log
                reason,
                error,
                "drain_forced"
            );
            true
        }
    }
}

async fn clean_up_after_forced_drain(resources: &mut Resources, deadline: Instant) -> bool {
    let jobs = async {
        // template:begin jobs:worker-shutdown-finish-jobs
        futures_util::future::join_all(
            resources
                .started
                .iter()
                .map(|engine| finish_attempts(engine, deadline)),
        )
        .await;
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
        return true;
    }
    false
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

async fn close_listeners(listeners: Listeners, budget: Duration) -> bool {
    let (health_degraded, diagnostics_degraded) = tokio::join!(
        stage(Listeners::HEALTH, close_health(listeners.health, budget)),
        stage(
            Listeners::DIAGNOSTICS,
            close_diagnostics(listeners.diagnostics, budget)
        ),
    );
    health_degraded || diagnostics_degraded
}

async fn close_health(server: Option<Server>, budget: Duration) -> bool {
    let Some(server) = server else {
        return false;
    };
    match server.drain(budget).await {
        Ok(Drained::Complete) => {
            tracing::info!("health_listener_stopped");
            false
        }
        Ok(Drained::TimedOut {
            remaining_connections,
        }) => {
            tracing::warn!(
                remaining = remaining_connections,
                "health listener outlived its close budget"
            );
            true
        }
        Err(err) => {
            tracing::error!(error = %err, "health listener drain failed");
            true
        }
    }
}

async fn close_diagnostics(server: Option<Server>, budget: Duration) -> bool {
    let Some(server) = server else {
        return false;
    };
    match server.drain(budget).await {
        Ok(Drained::Complete) => tracing::info!("diagnostics_stopped"),
        // Only a scrape that outlived the budget after the accept loop joined
        // is exempt from the degraded vote.
        Ok(Drained::TimedOut { .. }) => {
            tracing::warn!(
                reason = "scrape_outlived_shutdown_budget",
                "diagnostics_forced"
            );
        }
        Err(err) => {
            tracing::warn!(error = %err, "diagnostics_shutdown_failed");
            return true;
        }
    }
    false
}

async fn join_background(background: &Background, budget: Duration) -> bool {
    background.cancel.cancel();
    background.tracker.close();
    // `run` aborts what is still tracked and logs the summary event.
    match tokio::time::timeout(budget, background.tracker.wait()).await {
        Ok(()) => background.failure().is_some(),
        Err(_) => true,
    }
}

async fn finish_background(background: &Background, deadline: Instant) -> bool {
    if tokio::time::timeout_at(deadline, background.tracker.wait())
        .await
        .is_ok()
    {
        tracing::warn!("background_abort_acknowledged");
    } else {
        tracing::warn!("background_completion_unconfirmed");
    }
    // Forced termination never becomes a graceful join.
    true
}

// template:begin jobs:worker-shutdown-close-pool
async fn close_pool(pool: &PgPool, budget: Duration) -> bool {
    match infra_postgres::close(pool, budget).await {
        infra_postgres::CloseOutcome::Complete => {
            tracing::info!("postgres_pool_closed");
            false
        }
        infra_postgres::CloseOutcome::TimedOut => {
            tracing::warn!("postgres pool outlived its close budget");
            true
        }
    }
}
// template:end jobs:worker-shutdown-close-pool

// template:begin messaging:worker-shutdown-close-messaging-fn
async fn close_messaging(
    messaging: Option<Messaging>,
    startup: Option<MessagingStartup>,
    deadline: Instant,
) -> bool {
    // The root token is already cancelled here, and close gives up at
    // once under a cancelled token, so close gets a fresh one.
    let cancel = tokio_util::sync::CancellationToken::new();
    let closed = if let Some(messaging) = messaging {
        messaging.close(deadline, &cancel).await
    } else if let Some(startup) = startup {
        startup.close(deadline, &cancel).await
    } else {
        return false;
    };
    match closed {
        CloseOutcome::Complete => {
            tracing::info!("messaging_closed");
            false
        }
        CloseOutcome::WaitInterrupted | CloseOutcome::UnobservedClose => {
            tracing::warn!(outcome = ?closed, "messaging resource close incomplete");
            true
        }
    }
}
// template:end messaging:worker-shutdown-close-messaging-fn

async fn close_dependencies(resources: &mut Resources, deadline: Instant) -> bool {
    let pool_close = async {
        // template:begin jobs:worker-shutdown-close-jobs
        if let Some(pool) = resources.pool.as_ref() {
            return close_pool(pool, deadline.saturating_duration_since(Instant::now())).await;
        }
        // template:end jobs:worker-shutdown-close-jobs
        false
    };
    #[allow(
        clippy::let_and_return,
        reason = "the messaging profile rebinds degraded before it is returned"
    )]
    let messaging_close = async {
        #[allow(unused_variables, reason = "the messaging profile supplies the close")]
        let degraded = false;
        // template:begin messaging:worker-shutdown-close-messaging
        let degraded = close_messaging(
            resources.messaging.take(),
            resources.messaging_startup.take(),
            deadline,
        )
        .await;
        // template:end messaging:worker-shutdown-close-messaging
        degraded
    };
    let (pool_degraded, messaging_degraded) = tokio::join!(
        stage("postgres_close", pool_close),
        stage("messaging_close", messaging_close),
    );
    pool_degraded || messaging_degraded
}

async fn finish_telemetry(
    resources: &mut Resources,
    deadline: Instant,
    mut degraded: bool,
) -> Outcome {
    if let Some(logger) = &resources.logger {
        logger.begin_shutdown();
    }
    let log_reserve = if resources.logger.is_some() {
        LOG_CLOSE_RESERVE.min(deadline.saturating_duration_since(Instant::now()))
    } else {
        Duration::ZERO
    };
    if let Some(provider) = resources.tracer_provider.take() {
        degraded |= stage("telemetry", async {
            match provider.shutdown(deadline - log_reserve).await {
                ProviderShutdown::Completed => {
                    tracing::info!(delivery_confirmed = false, "trace_shutdown_completed");
                    false
                }
                ProviderShutdown::Incomplete(reasons) => {
                    tracing::warn!(reasons = reasons.bits(), "trace_shutdown_incomplete");
                    true
                }
            }
        })
        .await;
    }
    let mut outcome = if degraded {
        Outcome::Degraded
    } else {
        Outcome::Graceful
    };
    tracing::info!(?outcome, logger_pending = true, "shutdown_finishing");
    if let Some(logger) = resources.logger.take() {
        let incomplete = stage("logger", async {
            let close = tokio::task::spawn_blocking(move || logger.shutdown(deadline.into_std()));
            !matches!(
                tokio::time::timeout_at(deadline, close).await,
                Ok(Ok(LoggerShutdown::Completed(_)))
            )
        })
        .await;
        if incomplete {
            outcome = Outcome::Degraded;
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[test]
    fn default_budgets_leave_one_and_a_half_seconds() {
        let http = HttpConfig::default();
        validate_grace_budget(&http).unwrap();
        assert_eq!(
            http.grace_period
                .checked_sub(http.drain_timeout)
                .and_then(|left| left.checked_sub(SHUTDOWN_TAIL)),
            Some(Duration::from_millis(1_500))
        );
    }

    #[test]
    fn grace_budget_boundary_is_drain_plus_tail() {
        let pass = HttpConfig {
            drain_timeout: Duration::from_secs(25),
            grace_period: Duration::from_millis(43_500),
            ..HttpConfig::default()
        };
        validate_grace_budget(&pass).unwrap();
        let fail = HttpConfig {
            grace_period: Duration::from_millis(43_500) - Duration::from_nanos(1),
            ..pass
        };
        assert!(validate_grace_budget(&fail).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn stages_are_clamped_to_the_remaining_deadline() {
        let budget = Budget::until(Instant::now() + Duration::from_secs(11));
        assert_eq!(
            budget.remaining(Duration::from_secs(4)),
            Duration::from_secs(4)
        );
        tokio::time::advance(Duration::from_secs(8)).await;
        assert_eq!(
            budget.remaining(Duration::from_secs(4)),
            Duration::from_secs(2)
        );
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(budget.remaining(Duration::from_secs(4)), Duration::ZERO);
    }

    #[tokio::test]
    async fn cooperative_background_completion_is_graceful() {
        let background = Background::new();
        background.spawn("waits", CancellationToken::cancelled_owned);
        assert!(!join_background(&background, BACKGROUND_JOIN).await);
        assert!(background.cancel.is_cancelled());
        assert!(background.tracker.is_closed());
        assert!(background.tracker.is_empty());
        assert!(
            background.failure().is_none(),
            "a task that ends at its cancellation is not a failure"
        );
    }

    #[tokio::test]
    async fn a_task_that_returns_early_is_reported_by_name() {
        let background = Background::new();
        background.spawn("waits", CancellationToken::cancelled_owned);
        background.spawn("returns", |_cancel| async {});
        assert_eq!(
            tokio::time::timeout(BACKGROUND_JOIN, background.stopped())
                .await
                .unwrap(),
            "returns"
        );
        assert!(join_background(&background, BACKGROUND_JOIN).await);
    }

    #[tokio::test]
    async fn panic_after_cancellation_degrades_common_shutdown() {
        let background = Background::new();
        background.spawn("cleanup_panics", |cancel| async move {
            cancel.cancelled().await;
            panic!("cleanup defect");
        });
        let http = HttpConfig::default();
        let mut signals = Signals::install().unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            run(Plan {
                http: &http,
                resources: Resources::default(),
                background: background.clone(),
                signals: &mut signals,
                deadline: Instant::now() + http.grace_period,
            }),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Degraded);
        assert!(background.tracker.is_empty());
        assert_eq!(background.failure(), Some("cleanup_panics"));
    }

    #[tokio::test(start_paused = true)]
    async fn ignored_cancellation_is_aborted_and_acknowledged_before_shutdown_returns() {
        let background = Background::new();
        let (ended, observed_end) = tokio::sync::oneshot::channel::<()>();
        background.spawn("ignores_stop", |_cancel| async move {
            let _held = ended;
            std::future::pending::<()>().await;
        });
        let http = HttpConfig::default();
        let mut signals = Signals::install().unwrap();
        let started = Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_secs(9),
            run(Plan {
                http: &http,
                resources: Resources::default(),
                background: background.clone(),
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
        assert!(background.tracker.is_empty());
        std::assert_matches!(observed_end.now_or_never(), Some(Err(_)));
        assert!(
            background.failure().is_none(),
            "forced cancellation is not an unexpected return"
        );
    }
}
