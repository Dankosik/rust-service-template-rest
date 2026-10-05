//! Staged teardown under the original process deadline, including partial startup.
//!
//! Stage order: readiness off and claiming stopped, drain, cleanup after a
//! forced drain, listeners, background join, pool close, telemetry flush.
//! Every stage takes the lesser of its ceiling and what is left of
//! `http.grace_period`.

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, PoisonError};
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
use infra_postgres::{Closed, PgPool};
// template:end jobs:worker-shutdown-postgres-imports
use infra_telemetry::{ProviderShutdown, SHUTDOWN_JOIN_SLACK, TracerProviderHandle};
use service_config::HttpConfig;
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Ceiling for finishing attempts after a forced drain.
pub(crate) const CLEANUP: Duration = Duration::from_secs(2);
/// Ceiling for closing the health and diagnostics listeners.
pub(crate) const LISTENERS: Duration = Duration::from_secs(2);
/// Ceiling for cancelling and joining background tasks.
pub(crate) const BACKGROUND_JOIN: Duration = Duration::from_secs(3);
/// Ceiling for closing the PostgreSQL pool.
pub(crate) const DEPENDENCY_CLOSE: Duration = Duration::from_secs(5);
/// Ceiling for flushing telemetry.
pub(crate) const TELEMETRY_FLUSH: Duration = Duration::from_secs(5);
/// Whole process tail, including SDK join slack and the runtime shutdown reserve.
pub(crate) const SHUTDOWN_TAIL: Duration = CLEANUP
    .saturating_add(LISTENERS)
    .saturating_add(BACKGROUND_JOIN)
    .saturating_add(DEPENDENCY_CLOSE)
    .saturating_add(TELEMETRY_FLUSH)
    .saturating_add(SHUTDOWN_JOIN_SLACK)
    .saturating_add(crate::RUNTIME_SHUTDOWN_TIMEOUT);

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
    /// Every stage completed inside its budget.
    Graceful,
    /// At least one stage overran; the process still exited on its own.
    Degraded,
}

/// Async stages reserve the final runtime allowance inside the process deadline.
struct Budget {
    deadline: Instant,
}

impl Budget {
    fn until(process_deadline: Instant) -> Self {
        Self {
            deadline: process_deadline - crate::RUNTIME_SHUTDOWN_TIMEOUT,
        }
    }

    fn remaining(&self, want: Duration) -> Duration {
        want.min(self.deadline.saturating_duration_since(Instant::now()))
    }

    fn stage_deadline(&self, want: Duration) -> Instant {
        Instant::now() + self.remaining(want)
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("stop signal receiver closed unexpectedly")]
pub(crate) struct SignalError;

/// Native streams remain owned by the synchronous entrypoint through runtime shutdown.
pub(crate) struct Signals {
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    first_stop: Option<Instant>,
}

impl Signals {
    pub(crate) fn install() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let interrupt = signal(SignalKind::interrupt())?;
            let terminate = signal(SignalKind::terminate())?;
            Ok(Self {
                interrupt,
                terminate,
                first_stop: None,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                ctrl_c: tokio::signal::windows::ctrl_c()?,
                first_stop: None,
            })
        }
    }

    /// Consume one native notification and retain its first observation time.
    pub(crate) async fn wait(&mut self) -> Result<(), SignalError> {
        #[cfg(unix)]
        let (notification, name) = tokio::select! {
            signal = self.terminate.recv() => (signal, "SIGTERM"),
            signal = self.interrupt.recv() => (signal, "SIGINT"),
        };
        #[cfg(windows)]
        let (notification, name) = (self.ctrl_c.recv().await, "ctrl-c");
        notification.ok_or(SignalError)?;
        self.first_stop.get_or_insert_with(Instant::now);
        tracing::info!(signal = name, "stop requested");
        Ok(())
    }

    /// Poll and consume only an already pending notification.
    pub(crate) fn pending(&mut self) -> Result<bool, SignalError> {
        match self.wait().now_or_never() {
            Some(result) => result.map(|()| true),
            None => Ok(false),
        }
    }

    pub(crate) fn first_stop(&self) -> Option<Instant> {
        self.first_stop
    }
}

/// The worker's background tasks and their root cancellation token.
///
/// Every task runs until its token is cancelled, and nothing cancels before
/// teardown, so a task that ends earlier is a panic or a defect: the worker
/// stops rather than run without it, as the service does. A task spawned
/// through [`Self::spawn`] reports that end by name. The engines spawn their
/// loops on [`Self::tracker`] and report through `Started::failed`.
#[derive(Clone)]
pub(crate) struct Background {
    pub(crate) cancel: CancellationToken,
    pub(crate) tracker: TaskTracker,
    stopped: watch::Sender<Option<&'static str>>,
    aborts: Arc<Mutex<Vec<AbortHandle>>>,
}

impl Background {
    pub(crate) fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            tracker: TaskTracker::new(),
            stopped: watch::Sender::new(None),
            aborts: Arc::default(),
        }
    }

    /// Spawn the task `start` builds from a child of the root token.
    pub(crate) fn spawn<F>(&self, name: &'static str, start: impl FnOnce(CancellationToken) -> F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let cancel = self.cancel.child_token();
        let task = start(cancel.clone());
        let guard = ReportUnlessCancelled {
            name,
            cancel,
            stopped: self.stopped.clone(),
        };
        let task = self.tracker.spawn(async move {
            let _guard = guard;
            task.await;
        });
        self.aborts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(task.abort_handle());
    }

    pub(crate) fn failure(&self) -> Option<&'static str> {
        *self.stopped.borrow()
    }

    fn abort(&self) {
        for task in self
            .aborts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            task.abort();
        }
    }

    /// Resolve with the name of the first task that ended while its token
    /// was live. Pending while every task runs.
    pub(crate) async fn stopped(&self) -> &'static str {
        let mut stopped = self.stopped.subscribe();
        loop {
            if let Some(name) = *stopped.borrow_and_update() {
                return name;
            }
            if stopped.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Dropped when its task ends, a panic included.
struct ReportUnlessCancelled {
    name: &'static str,
    cancel: CancellationToken,
    stopped: watch::Sender<Option<&'static str>>,
}

impl Drop for ReportUnlessCancelled {
    fn drop(&mut self) {
        if self.cancel.is_cancelled() && !std::thread::panicking() {
            return;
        }
        tracing::error!(
            task = self.name,
            panicked = std::thread::panicking(),
            "background_task_stopped"
        );
        self.stopped.send_if_modified(|first| {
            let unset = first.is_none();
            if unset {
                *first = Some(self.name);
            }
            unset
        });
    }
}

/// The listeners bound so far. Common shutdown closes whichever are present.
#[derive(Debug, Default)]
pub(crate) struct Listeners {
    pub(crate) health: Option<Server>,
    pub(crate) diagnostics: Option<Server>,
}

/// What startup opened and teardown closes.
#[derive(Default)]
pub(crate) struct Resources {
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
    pub(crate) tracer_provider: Option<TracerProviderHandle>,
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
            finish_work(&mut resources, budget.stage_deadline(CLEANUP)),
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
    degraded |= background_failed;
    // Forced acknowledgement and dependency close share this single allocation.
    let dependency_deadline = budget.stage_deadline(DEPENDENCY_CLOSE);
    if !background.tracker.is_empty() {
        background.abort();
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
    if let Some(provider) = resources.tracer_provider.take() {
        degraded |= stage(
            "telemetry_flush",
            flush_telemetry(
                provider,
                budget.stage_deadline(TELEMETRY_FLUSH.saturating_add(SHUTDOWN_JOIN_SLACK)),
            ),
        )
        .await;
    }
    degraded |= background.failure().is_some();
    if signals.pending().is_err() {
        tracing::error!("stop signal receiver failed during shutdown");
        degraded = true;
    }
    let outcome = if degraded {
        Outcome::Degraded
    } else {
        Outcome::Graceful
    };
    tracing::info!(?outcome, "shutdown_completed");
    outcome
}

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
                return consumer.finish(deadline).await.is_err();
            }
            // template:end messaging:worker-shutdown-drain-messaging
            false
        };
        let ((), messaging_failed) = tokio::join!(jobs, messages);
        messaging_failed
    };
    let forced = tokio::select! {
        biased;
        messaging_failed = joined => messaging_failed.then_some("messaging"),
        () = tokio::time::sleep_until(deadline) => Some("budget"),
        result = signals.wait() => Some(if result.is_ok() { "second_signal" } else { "signal_receiver_failed" }),
    };
    match forced {
        None => {
            tracing::info!("drain_completed");
            false
        }
        Some(reason) => {
            tracing::warn!(
                // template:begin jobs:worker-shutdown-drain-forced-log
                in_flight = resources
                    .started
                    .iter()
                    .map(Started::in_flight)
                    .sum::<usize>(),
                // template:end jobs:worker-shutdown-drain-forced-log
                reason,
                "drain_forced"
            );
            true
        }
    }
}

async fn finish_work(resources: &mut Resources, deadline: Instant) -> bool {
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
            if consumer.finish(deadline).await.is_err() {
                tracing::warn!("messaging consumer cleanup failed");
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
    let (health_failed, diagnostics_failed) = tokio::join!(
        stage("health_listener", close_health(listeners.health, budget)),
        stage(
            "diagnostics_listener",
            close_diagnostics(listeners.diagnostics, budget)
        ),
    );
    health_failed || diagnostics_failed
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
    match tokio::time::timeout(budget, background.tracker.wait()).await {
        Ok(()) if background.failure().is_none() => false,
        Ok(()) => {
            tracing::error!("background_join_failed");
            true
        }
        Err(_) => {
            background.abort();
            tracing::warn!("background join expired; abort requested");
            true
        }
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
        Closed::Complete => {
            tracing::info!("postgres_pool_closed");
            false
        }
        Closed::TimedOut => {
            tracing::warn!("postgres pool outlived its close budget");
            true
        }
    }
}
// template:end jobs:worker-shutdown-close-pool

async fn close_dependencies(resources: &mut Resources, deadline: Instant) -> bool {
    let close_pool = async {
        // template:begin jobs:worker-shutdown-close-jobs
        if let Some(pool) = resources.pool.as_ref() {
            return close_pool(pool, deadline.saturating_duration_since(Instant::now())).await;
        }
        // template:end jobs:worker-shutdown-close-jobs
        false
    };
    let close_messaging = async {
        // template:begin messaging:worker-shutdown-close-messaging
        let cancel = CancellationToken::new();
        let closed = if let Some(messaging) = resources.messaging.take() {
            Some(messaging.close(deadline, &cancel).await)
        } else if let Some(startup) = resources.messaging_startup.take() {
            Some(startup.close(deadline, &cancel).await)
        } else {
            None
        };
        if let Some(closed) = closed {
            return match closed {
                CloseOutcome::Complete => {
                    tracing::info!("messaging_closed");
                    false
                }
                CloseOutcome::TimedOut | CloseOutcome::UnobservedClose => {
                    tracing::warn!("messaging resource close incomplete");
                    true
                }
            };
        }
        // template:end messaging:worker-shutdown-close-messaging
        false
    };
    let (pool_overran, messaging_overran) = tokio::join!(
        stage("postgres_close", close_pool),
        stage("messaging_close", close_messaging),
    );
    pool_overran || messaging_overran
}

async fn flush_telemetry(provider: TracerProviderHandle, deadline: Instant) -> bool {
    match provider.shutdown_until(TELEMETRY_FLUSH, deadline).await {
        ProviderShutdown::Flushed => {
            tracing::info!("telemetry_flushed");
            false
        }
        ProviderShutdown::Incomplete => {
            tracing::warn!("telemetry_flush_incomplete");
            true
        }
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn grace_budget_refusal_names_the_tail() {
        let http = HttpConfig {
            grace_period: Duration::from_secs(41),
            drain_timeout: Duration::from_secs(25),
            ..HttpConfig::default()
        };
        let err = validate_grace_budget(&http).unwrap_err();
        assert_eq!(
            err.to_string(),
            "http.grace_period (41s) must be >= http.drain_timeout (25s) plus the 18.5s jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush, SDK join slack, runtime shutdown)"
        );
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
            background.stopped.borrow().is_none(),
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
    async fn a_task_that_panics_is_reported_by_name() {
        let background = Background::new();
        background.spawn("panics", |_cancel| async { panic!("task defect") });
        assert_eq!(
            tokio::time::timeout(BACKGROUND_JOIN, background.stopped())
                .await
                .unwrap(),
            "panics"
        );
        background.tracker.close();
        tokio::time::timeout(BACKGROUND_JOIN, background.tracker.wait())
            .await
            .unwrap();
        background.spawn("later", |_cancel| async {});
        tokio::time::timeout(BACKGROUND_JOIN, background.tracker.wait())
            .await
            .unwrap();
        assert_eq!(background.failure(), Some("panics"), "the first end stands");
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
        assert!(matches!(observed_end.now_or_never(), Some(Err(_))));
        assert!(
            background.failure().is_none(),
            "forced cancellation is not an unexpected return"
        );
    }
}
