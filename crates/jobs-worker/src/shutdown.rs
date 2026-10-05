//! Staged teardown under one grace-period deadline, and the startup abort.
//!
//! Stage order: readiness off and claiming stopped, drain, cleanup after a
//! forced drain, listeners, background join, pool close, telemetry flush.
//! Every stage takes the lesser of its ceiling and what is left of
//! `http.grace_period`.

use std::time::Duration;

use health::Readiness;
use infra_http::{Drained, Server};
// template:begin jobs:worker-shutdown-jobs-imports
use infra_jobs::Started;
// template:end jobs:worker-shutdown-jobs-imports
// template:begin messaging:worker-shutdown-messaging-imports
use infra_messaging::{CloseOutcome, ConsumerHandle, Messaging};
// template:end messaging:worker-shutdown-messaging-imports
// template:begin jobs:worker-shutdown-postgres-imports
use infra_postgres::{Closed, PgPool};
// template:end jobs:worker-shutdown-postgres-imports
use infra_telemetry::{LoggerGuard, LoggerShutdown, ProviderShutdown, TracerProviderHandle};
use service_config::HttpConfig;
use tokio::sync::watch;
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
/// Worst case after the drain: cleanup, listeners, background join, dependency close, and telemetry flush.
pub(crate) const SHUTDOWN_TAIL: Duration = CLEANUP
    .saturating_add(LISTENERS)
    .saturating_add(BACKGROUND_JOIN)
    .saturating_add(DEPENDENCY_CLOSE)
    .saturating_add(TELEMETRY_FLUSH);

#[derive(Debug, thiserror::Error)]
#[error(
    "http.grace_period ({grace:?}) must be >= http.drain_timeout ({drain_timeout:?}) plus the \
     {tail:?} jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush)"
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

/// The one deadline every stage draws from. The clock starts when teardown
/// begins. The grace period is validated to at most 10 minutes, so
/// `Instant::now() + grace` cannot overflow.
struct Budget {
    deadline: Instant,
}

impl Budget {
    fn remaining(&self, want: Duration) -> Duration {
        want.min(self.deadline.saturating_duration_since(Instant::now()))
    }
}

/// Stop signals. Installed before anything can send one. One listener task
/// owns the streams for the process lifetime: tokio's handler
/// (signal-hook-registry) is never unregistered, so a dropped stream would
/// swallow a later signal instead of letting it terminate the process.
#[derive(Clone)]
pub(crate) struct Signals {
    stop: watch::Receiver<u64>,
}

impl Signals {
    pub(crate) fn install() -> std::io::Result<Self> {
        let (tx, stop) = watch::channel(0_u64);
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            // Install SIGINT first so a failed SIGTERM install cannot drop a live SIGTERM stream.
            let mut interrupt = signal(SignalKind::interrupt())?;
            let mut terminate = signal(SignalKind::terminate())?;
            // Not in the TaskTracker: this listener must outlive background join.
            tokio::spawn(async move {
                loop {
                    let name = tokio::select! {
                        Some(()) = terminate.recv() => "SIGTERM",
                        Some(()) = interrupt.recv() => "SIGINT",
                        else => return,
                    };
                    tracing::info!(signal = name, "stop requested");
                    tx.send_modify(|count| *count = count.wrapping_add(1));
                }
            });
            Ok(Self { stop })
        }
        #[cfg(windows)]
        {
            let mut ctrl_c = tokio::signal::windows::ctrl_c()?;
            // Not in the TaskTracker: this listener must outlive background join.
            tokio::spawn(async move {
                while ctrl_c.recv().await.is_some() {
                    tracing::info!(signal = "ctrl-c", "stop requested");
                    tx.send_modify(|count| *count = count.wrapping_add(1));
                }
            });
            Ok(Self { stop })
        }
    }

    /// Resolve on the next stop signal.
    pub(crate) async fn wait(&mut self) {
        if self.stop.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    /// Whether a stop signal arrived since the last [`Self::wait`] or
    /// [`Self::pending`]. Consumes what it finds, so the next `wait` waits
    /// for a new signal.
    pub(crate) fn pending(&mut self) -> bool {
        match self.stop.has_changed() {
            Ok(true) => {
                let _ = self.stop.borrow_and_update();
                true
            }
            Ok(false) | Err(_) => false,
        }
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
}

impl Background {
    pub(crate) fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            tracker: TaskTracker::new(),
            stopped: watch::Sender::new(None),
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
        self.tracker.spawn(async move {
            let _guard = guard;
            task.await;
        });
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
        if self.cancel.is_cancelled() {
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

/// The listeners bound so far. The shutdown plan and `abort_startup` close
/// whichever are present.
#[derive(Debug, Default)]
pub(crate) struct Listeners {
    pub(crate) health: Option<Server>,
    pub(crate) diagnostics: Option<Server>,
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
    // template:end messaging:worker-shutdown-resources-messaging
    // template:begin jobs:worker-shutdown-resources-pool
    pub(crate) pool: Option<PgPool>,
    // template:end jobs:worker-shutdown-resources-pool
    pub(crate) listeners: Listeners,
}

/// What the staged teardown owns.
pub(crate) struct Plan<'a> {
    pub(crate) http: &'a HttpConfig,
    pub(crate) readiness: &'a Readiness,
    pub(crate) resources: Resources,
    pub(crate) background: Background,
    pub(crate) deadline: Instant,
    pub(crate) signals: &'a mut Signals,
}

/// Runs the stages in order under one deadline started now, and returns
/// whether any stage voted degraded.
pub(crate) async fn run(plan: Plan<'_>) -> Outcome {
    let Plan {
        http,
        readiness,
        mut resources,
        background,
        deadline,
        signals,
    } = plan;
    let budget = Budget { deadline };
    stop_work(http, readiness, &resources);
    let mut degraded = drain(&mut resources, http.drain_timeout, &budget, signals).await;
    if degraded {
        finish_work(&mut resources, Instant::now() + budget.remaining(CLEANUP)).await;
    }
    // template:begin messaging:worker-shutdown-drop-consumer
    // Exhausted cleanup still aborts the owner before dependencies close.
    // The forced drain already selected the degraded process outcome.
    drop(resources.consumer.take());
    // template:end messaging:worker-shutdown-drop-consumer
    degraded |= close_listeners(
        std::mem::take(&mut resources.listeners),
        budget.remaining(LISTENERS),
    )
    .await;
    degraded |= join_background(&background, budget.remaining(BACKGROUND_JOIN)).await;
    degraded |= close_dependencies(&mut resources, budget.remaining(DEPENDENCY_CLOSE)).await;
    finish_telemetry(
        &mut resources,
        Instant::now() + budget.remaining(TELEMETRY_FLUSH),
        degraded,
    )
    .await
}

/// The one teardown for every refusal after the runtime started.
///
/// Each stage is bounded by its own ceiling. There is no grace deadline,
/// because no stop signal started it. It closes acquired telemetry too;
/// the primary startup failure keeps exit code 1.
pub(crate) async fn abort_startup(mut resources: Resources, background: &Background) {
    finish_work(&mut resources, Instant::now() + CLEANUP).await;
    // template:begin messaging:worker-shutdown-abort-drop-consumer
    drop(resources.consumer.take());
    // template:end messaging:worker-shutdown-abort-drop-consumer
    let mut degraded = close_listeners(std::mem::take(&mut resources.listeners), LISTENERS).await;
    degraded |= join_background(background, BACKGROUND_JOIN).await;
    degraded |= close_dependencies(&mut resources, DEPENDENCY_CLOSE).await;
    let _ = finish_telemetry(&mut resources, Instant::now() + TELEMETRY_FLUSH, degraded).await;
}

fn stop_work(http: &HttpConfig, readiness: &Readiness, resources: &Resources) {
    tracing::info!(grace = ?http.grace_period, "shutdown_started");
    readiness.start_drain();
    tracing::info!("readiness_disabled");
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
        () = signals.wait() => Some("second_signal"),
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

async fn finish_work(resources: &mut Resources, deadline: Instant) {
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
    tokio::join!(jobs, messages);
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
    let had_listener = listeners.health.is_some() || listeners.diagnostics.is_some();
    let (health_overran, ()) = tokio::join!(
        close_health(listeners.health, budget),
        close_diagnostics(listeners.diagnostics, budget),
    );
    if had_listener {
        tracing::info!("listeners_stopped");
    }
    health_overran
}

async fn close_health(server: Option<Server>, budget: Duration) -> bool {
    let Some(server) = server else {
        return false;
    };
    match server.drain(budget).await {
        Ok(Drained::Complete) => false,
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

async fn close_diagnostics(server: Option<Server>, budget: Duration) {
    let Some(server) = server else {
        return;
    };
    match server.drain(budget).await {
        Ok(Drained::Complete) => {}
        Ok(Drained::TimedOut { .. }) => {
            tracing::warn!(
                reason = "scrape_outlived_shutdown_budget",
                "diagnostics_forced"
            );
        }
        Err(err) => tracing::warn!(error = %err, "diagnostics_shutdown_failed"),
    }
}

async fn join_background(background: &Background, budget: Duration) -> bool {
    background.cancel.cancel();
    background.tracker.close();
    match tokio::time::timeout(budget, background.tracker.wait()).await {
        Ok(()) => {
            tracing::info!("background_joined");
            false
        }
        Err(_elapsed) => {
            tracing::warn!("background tasks outlived their join budget");
            true
        }
    }
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

async fn close_dependencies(resources: &mut Resources, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
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
        if let Some(messaging) = resources.messaging.take() {
            return match messaging.close(deadline, &CancellationToken::new()).await {
                CloseOutcome::Complete => {
                    tracing::info!("messaging_closed");
                    false
                }
                CloseOutcome::TimedOut | CloseOutcome::UnobservedClose => {
                    tracing::warn!("messaging resource outlived its close budget");
                    true
                }
            };
        }
        // template:end messaging:worker-shutdown-close-messaging
        false
    };
    let (pool_overran, messaging_overran) = tokio::join!(close_pool, close_messaging);
    pool_overran || messaging_overran
}

async fn finish_telemetry(
    resources: &mut Resources,
    deadline: Instant,
    mut degraded: bool,
) -> Outcome {
    if let Some(logger) = &resources.logger {
        logger.begin_shutdown();
    }
    let trace_deadline =
        deadline - Duration::from_secs(1).min(deadline.saturating_duration_since(Instant::now()));
    if let Some(provider) = resources.tracer_provider.take() {
        match provider.shutdown(trace_deadline).await {
            ProviderShutdown::Completed => {
                tracing::info!(delivery_confirmed = false, "trace_shutdown_completed");
            }
            ProviderShutdown::Incomplete(reasons) => {
                tracing::warn!(reasons = reasons.bits(), "trace_shutdown_incomplete");
                degraded = true;
            }
        }
    }
    let outcome = if degraded {
        Outcome::Degraded
    } else {
        Outcome::Graceful
    };
    tracing::info!(outcome = ?outcome, logger_pending = true, "shutdown_finishing");
    if let Some(logger) = resources.logger.take() {
        let close = tokio::task::spawn_blocking(move || logger.shutdown(deadline.into_std()));
        if !matches!(
            tokio::time::timeout_at(deadline, close).await,
            Ok(Ok(LoggerShutdown::Completed(_)))
        ) {
            return Outcome::Degraded;
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budgets_leave_three_seconds() {
        let http = HttpConfig::default();
        validate_grace_budget(&http).unwrap();
        assert_eq!(
            http.grace_period
                .checked_sub(http.drain_timeout)
                .and_then(|left| left.checked_sub(SHUTDOWN_TAIL)),
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn grace_budget_boundary_is_drain_plus_tail() {
        let pass = HttpConfig {
            drain_timeout: Duration::from_secs(25),
            grace_period: Duration::from_secs(42),
            ..HttpConfig::default()
        };
        validate_grace_budget(&pass).unwrap();
        let fail = HttpConfig {
            grace_period: Duration::from_secs(42) - Duration::from_nanos(1),
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
            "http.grace_period (41s) must be >= http.drain_timeout (25s) plus the 17s jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush)"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stages_are_clamped_to_the_remaining_deadline() {
        let budget = Budget {
            deadline: Instant::now() + Duration::from_secs(10),
        };
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
    async fn abort_startup_cancels_and_joins_a_tracked_task() {
        let background = Background::new();
        background.spawn("waits", CancellationToken::cancelled_owned);
        abort_startup(Resources::default(), &background).await;
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
        assert_eq!(background.stopped().await, "returns");
    }

    #[tokio::test]
    async fn a_task_that_panics_is_reported_by_name() {
        let background = Background::new();
        background.spawn("panics", |_cancel| async { panic!("task defect") });
        assert_eq!(background.stopped().await, "panics");
        background.tracker.close();
        background.tracker.wait().await;
        background.spawn("later", |_cancel| async {});
        background.tracker.wait().await;
        assert_eq!(background.stopped().await, "panics", "the first end stands");
    }
}
