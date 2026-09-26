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
use infra_telemetry::{ProviderShutdown, TracerProviderHandle};
use service_config::HttpConfig;
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
    fn start(grace: Duration) -> Self {
        Self {
            deadline: Instant::now() + grace,
        }
    }

    fn remaining(&self, want: Duration) -> Duration {
        want.min(self.deadline.saturating_duration_since(Instant::now()))
    }
}

/// Stop signals. Created before anything can send one and kept for the
/// process lifetime: hyper's libc handler is never uninstalled, so a dropped
/// stream would swallow a later SIGTERM instead of letting it kill us.
pub(crate) struct Signals {
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
}

impl Signals {
    pub(crate) fn install() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            // Install SIGINT first so a failed SIGTERM install cannot drop a
            // live SIGTERM stream: hyper never unregisters the libc handler.
            let interrupt = signal(SignalKind::interrupt())?;
            let terminate = signal(SignalKind::terminate())?;
            Ok(Self {
                terminate,
                interrupt,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                ctrl_c: tokio::signal::windows::ctrl_c()?,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {})
        }
    }

    /// Resolve on the next SIGTERM or SIGINT (Ctrl-C elsewhere).
    pub(crate) async fn wait(&mut self) {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.terminate.recv() => tracing::info!(signal = "SIGTERM", "stop requested"),
                _ = self.interrupt.recv() => tracing::info!(signal = "SIGINT", "stop requested"),
            }
        }
        #[cfg(windows)]
        {
            let _ = self.ctrl_c.recv().await;
            tracing::info!(signal = "ctrl-c", "stop requested");
        }
        #[cfg(not(any(unix, windows)))]
        {
            match tokio::signal::ctrl_c().await {
                Ok(()) => tracing::info!(signal = "ctrl-c", "stop requested"),
                Err(err) => tracing::error!(error = %err, "failed to listen for ctrl-c"),
            }
        }
    }

    /// Whether a stop signal arrived since the last [`Self::wait`] or
    /// [`Self::pending`]. Consumes what it finds, so the next `wait` waits
    /// for a new signal.
    pub(crate) fn pending(&mut self) -> bool {
        #[cfg(unix)]
        {
            let waker = std::task::Waker::noop();
            let mut context = std::task::Context::from_waker(waker);
            let terminate = signal_arrived(self.terminate.poll_recv(&mut context));
            let interrupt = signal_arrived(self.interrupt.poll_recv(&mut context));
            if terminate {
                tracing::info!(signal = "SIGTERM", "stop requested");
            }
            if interrupt {
                tracing::info!(signal = "SIGINT", "stop requested");
            }
            terminate || interrupt
        }
        #[cfg(windows)]
        {
            let waker = std::task::Waker::noop();
            let mut context = std::task::Context::from_waker(waker);
            let arrived = signal_arrived(self.ctrl_c.poll_recv(&mut context));
            if arrived {
                tracing::info!(signal = "ctrl-c", "stop requested");
            }
            arrived
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = self;
            false
        }
    }
}

#[cfg(any(unix, windows))]
fn signal_arrived(polled: std::task::Poll<Option<()>>) -> bool {
    matches!(polled, std::task::Poll::Ready(Some(())))
}

/// The listeners bound so far. The shutdown plan and `abort_startup` close
/// whichever are present.
#[derive(Debug, Default)]
pub(crate) struct Listeners {
    pub(crate) health: Option<Server>,
    pub(crate) diagnostics: Option<Server>,
}

/// What the staged teardown owns.
pub(crate) struct Plan<'a> {
    pub(crate) http: &'a HttpConfig,
    pub(crate) readiness: &'a Readiness,
    /// Every admitted jobs engine, empty before claiming has started.
    /// Ordinary work and reserved publication share the process deadlines.
    // template:begin jobs:worker-shutdown-plan-jobs
    pub(crate) started: &'a [Started],
    // template:end jobs:worker-shutdown-plan-jobs
    pub(crate) listeners: Listeners,
    pub(crate) cancel: CancellationToken,
    pub(crate) tracker: TaskTracker,
    // template:begin jobs:worker-shutdown-plan-jobs-pool
    pub(crate) pool: Option<PgPool>,
    // template:end jobs:worker-shutdown-plan-jobs-pool
    // template:begin messaging:worker-shutdown-plan-messaging
    pub(crate) consumer: Option<ConsumerHandle>,
    pub(crate) messaging: Option<Messaging>,
    // template:end messaging:worker-shutdown-plan-messaging
    pub(crate) tracer_provider: TracerProviderHandle,
    pub(crate) signals: &'a mut Signals,
}

/// Runs the stages in order under one deadline started now, and returns
/// whether any stage voted degraded.
pub(crate) async fn run(plan: Plan<'_>) -> Outcome {
    let Plan {
        http,
        readiness,
        // template:begin jobs:worker-shutdown-destructure-started
        started,
        // template:end jobs:worker-shutdown-destructure-started
        listeners,
        cancel,
        tracker,
        // template:begin jobs:worker-shutdown-destructure-pool
        pool,
        // template:end jobs:worker-shutdown-destructure-pool
        // template:begin messaging:worker-shutdown-destructure-messaging
        mut consumer,
        messaging,
        // template:end messaging:worker-shutdown-destructure-messaging
        tracer_provider,
        signals,
    } = plan;
    let budget = Budget::start(http.grace_period);
    stop_work(
        http,
        readiness,
        // template:begin jobs:worker-shutdown-stop-jobs-argument
        started,
        // template:end jobs:worker-shutdown-stop-jobs-argument
        // template:begin messaging:worker-shutdown-stop-messaging-argument
        consumer.as_ref(),
        // template:end messaging:worker-shutdown-stop-messaging-argument
    );
    let mut degraded = false;
    if drain(
        // template:begin jobs:worker-shutdown-drain-jobs-argument
        started,
        // template:end jobs:worker-shutdown-drain-jobs-argument
        // template:begin messaging:worker-shutdown-drain-messaging-argument
        consumer.as_mut(),
        // template:end messaging:worker-shutdown-drain-messaging-argument
        http.drain_timeout,
        &budget,
        signals,
    )
    .await
    {
        degraded = true;
        let _ = finish_work(
            // template:begin jobs:worker-shutdown-finish-jobs-argument
            started,
            // template:end jobs:worker-shutdown-finish-jobs-argument
            // template:begin messaging:worker-shutdown-finish-messaging-argument
            consumer.as_mut(),
            // template:end messaging:worker-shutdown-finish-messaging-argument
            Instant::now() + budget.remaining(CLEANUP),
        )
        .await;
    }
    // template:begin messaging:worker-shutdown-drop-consumer
    // Exhausted cleanup still aborts the owner before dependencies close.
    // The forced drain already selected the degraded process outcome.
    drop(consumer);
    // template:end messaging:worker-shutdown-drop-consumer
    if close_listeners(listeners, budget.remaining(LISTENERS)).await {
        degraded = true;
    }
    if join_background(&cancel, &tracker, budget.remaining(BACKGROUND_JOIN)).await {
        degraded = true;
    }
    if close_dependencies(
        // template:begin jobs:worker-shutdown-close-jobs-argument
        pool.as_ref(),
        // template:end jobs:worker-shutdown-close-jobs-argument
        // template:begin messaging:worker-shutdown-close-messaging-argument
        messaging,
        // template:end messaging:worker-shutdown-close-messaging-argument
        budget.remaining(DEPENDENCY_CLOSE),
    )
    .await
    {
        degraded = true;
    }
    if flush_telemetry(tracer_provider, budget.remaining(TELEMETRY_FLUSH)).await {
        degraded = true;
    }
    let outcome = if degraded {
        Outcome::Degraded
    } else {
        Outcome::Graceful
    };
    tracing::info!(outcome = ?outcome, "shutdown_completed");
    outcome
}

/// The one teardown for every refusal after the runtime started.
///
/// Each stage is bounded by its own ceiling. There is no grace deadline,
/// because no stop signal started it. It flushes no telemetry and returns
/// nothing: the exit code is 1 whatever it did.
pub(crate) async fn abort_startup(
    // template:begin jobs:worker-shutdown-abort-jobs-started
    started: &[Started],
    // template:end jobs:worker-shutdown-abort-jobs-started
    // template:begin messaging:worker-shutdown-abort-messaging-consumer
    mut consumer: Option<ConsumerHandle>,
    // template:end messaging:worker-shutdown-abort-messaging-consumer
    listeners: Listeners,
    cancel: &CancellationToken,
    tracker: &TaskTracker,
    // template:begin jobs:worker-shutdown-abort-jobs-pool
    pool: Option<&PgPool>,
    // template:end jobs:worker-shutdown-abort-jobs-pool
    // template:begin messaging:worker-shutdown-abort-messaging-resource
    messaging: Option<Messaging>,
    // template:end messaging:worker-shutdown-abort-messaging-resource
) {
    let _ = finish_work(
        // template:begin jobs:worker-shutdown-abort-jobs-finish
        started,
        // template:end jobs:worker-shutdown-abort-jobs-finish
        // template:begin messaging:worker-shutdown-abort-messaging-drain
        consumer.as_mut(),
        // template:end messaging:worker-shutdown-abort-messaging-drain
        Instant::now() + CLEANUP,
    )
    .await;
    // template:begin messaging:worker-shutdown-abort-drop-consumer
    drop(consumer);
    // template:end messaging:worker-shutdown-abort-drop-consumer
    let _ = close_listeners(listeners, LISTENERS).await;
    let _ = join_background(cancel, tracker, BACKGROUND_JOIN).await;
    let _ = close_dependencies(
        // template:begin jobs:worker-shutdown-abort-close-jobs-argument
        pool,
        // template:end jobs:worker-shutdown-abort-close-jobs-argument
        // template:begin messaging:worker-shutdown-abort-close-messaging-argument
        messaging,
        // template:end messaging:worker-shutdown-abort-close-messaging-argument
        DEPENDENCY_CLOSE,
    )
    .await;
}

fn stop_work(
    http: &HttpConfig,
    readiness: &Readiness,
    // template:begin jobs:worker-shutdown-stop-jobs-parameter
    started: &[Started],
    // template:end jobs:worker-shutdown-stop-jobs-parameter
    // template:begin messaging:worker-shutdown-stop-messaging-parameter
    consumer: Option<&ConsumerHandle>,
    // template:end messaging:worker-shutdown-stop-messaging-parameter
) {
    tracing::info!(grace = ?http.grace_period, "shutdown_started");
    readiness.start_drain();
    tracing::info!("readiness_disabled");
    // template:begin jobs:worker-shutdown-stop-jobs
    for started in started {
        started.stop_claiming();
        tracing::info!(in_flight = started.in_flight(), "claiming_stopped");
    }
    // template:end jobs:worker-shutdown-stop-jobs
    // template:begin messaging:worker-shutdown-stop-messaging
    if let Some(consumer) = consumer {
        consumer.drain();
        tracing::info!("messaging_pulls_stopped");
    }
    // template:end messaging:worker-shutdown-stop-messaging
}

async fn drain(
    // template:begin jobs:worker-shutdown-drain-jobs-parameter
    started: &[Started],
    // template:end jobs:worker-shutdown-drain-jobs-parameter
    // template:begin messaging:worker-shutdown-drain-messaging-parameter
    consumer: Option<&mut ConsumerHandle>,
    // template:end messaging:worker-shutdown-drain-messaging-parameter
    drain_timeout: Duration,
    budget: &Budget,
    signals: &mut Signals,
) -> bool {
    let drain_budget = budget.remaining(drain_timeout);
    tracing::info!(
        budget = ?drain_budget,
        // template:begin jobs:worker-shutdown-drain-start-log
        in_flight = started.iter().map(Started::in_flight).sum::<usize>(),
        // template:end jobs:worker-shutdown-drain-start-log
        "drain_started"
    );
    let deadline = Instant::now() + drain_budget;
    let joined = async {
        let jobs = async {
            // template:begin jobs:worker-shutdown-drain-jobs
            futures_util::future::join_all(started.iter().map(Started::drained)).await;
            // template:end jobs:worker-shutdown-drain-jobs
        };
        let messages = async move {
            // template:begin messaging:worker-shutdown-drain-messaging
            if let Some(consumer) = consumer {
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
                in_flight = started.iter().map(Started::in_flight).sum::<usize>(),
                // template:end jobs:worker-shutdown-drain-forced-log
                reason,
                "drain_forced"
            );
            true
        }
    }
}

async fn finish_work(
    // template:begin jobs:worker-shutdown-finish-jobs-parameter
    started: &[Started],
    // template:end jobs:worker-shutdown-finish-jobs-parameter
    // template:begin messaging:worker-shutdown-finish-messaging-parameter
    consumer: Option<&mut ConsumerHandle>,
    // template:end messaging:worker-shutdown-finish-messaging-parameter
    deadline: Instant,
) -> bool {
    let jobs = async {
        #[allow(unused_variables, reason = "retained jobs supply cleanup results")]
        let failed = false;
        // template:begin jobs:worker-shutdown-finish-jobs
        let failed = futures_util::future::join_all(
            started
                .iter()
                .map(|engine| finish_attempts(engine, deadline)),
        )
        .await
        .into_iter()
        .any(|timed_out| timed_out);
        // template:end jobs:worker-shutdown-finish-jobs
        failed
    };
    let messages = async {
        // template:begin messaging:worker-shutdown-finish-messaging
        if let Some(consumer) = consumer {
            consumer.abort();
            if consumer.finish(deadline).await.is_err() {
                tracing::warn!("messaging consumer cleanup failed");
                return true;
            }
        }
        // template:end messaging:worker-shutdown-finish-messaging
        false
    };
    let (jobs_failed, messaging_failed) = tokio::join!(jobs, messages);
    jobs_failed || messaging_failed
}

// template:begin jobs:worker-shutdown-finish-attempts
async fn finish_attempts(started: &Started, deadline: Instant) -> bool {
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
    end.timed_out
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

async fn join_background(
    cancel: &CancellationToken,
    tracker: &TaskTracker,
    budget: Duration,
) -> bool {
    cancel.cancel();
    tracker.close();
    match tokio::time::timeout(budget, tracker.wait()).await {
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

async fn close_dependencies(
    // template:begin jobs:worker-shutdown-close-jobs-parameter
    pool: Option<&PgPool>,
    // template:end jobs:worker-shutdown-close-jobs-parameter
    // template:begin messaging:worker-shutdown-close-messaging-parameter
    messaging: Option<Messaging>,
    // template:end messaging:worker-shutdown-close-messaging-parameter
    budget: Duration,
) -> bool {
    let deadline = Instant::now() + budget;
    let close_pool = async {
        // template:begin jobs:worker-shutdown-close-jobs
        if let Some(pool) = pool {
            return close_pool(pool, deadline.saturating_duration_since(Instant::now())).await;
        }
        // template:end jobs:worker-shutdown-close-jobs
        false
    };
    let close_messaging = async {
        // template:begin messaging:worker-shutdown-close-messaging
        if let Some(messaging) = messaging {
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

async fn flush_telemetry(provider: TracerProviderHandle, budget: Duration) -> bool {
    match provider.shutdown(budget).await {
        ProviderShutdown::Flushed => {
            tracing::info!("telemetry_flushed");
            false
        }
        ProviderShutdown::Incomplete => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_tail_is_seventeen_seconds() {
        assert_eq!(SHUTDOWN_TAIL, Duration::from_secs(17));
    }

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
        let budget = Budget::start(Duration::from_secs(10));
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
        let cancel = CancellationToken::new();
        let tracker = TaskTracker::new();
        let child = cancel.child_token();
        tracker.spawn(async move {
            child.cancelled().await;
        });
        abort_startup(
            // template:begin jobs:worker-shutdown-test-started-argument
            &[],
            // template:end jobs:worker-shutdown-test-started-argument
            // template:begin messaging:worker-shutdown-test-consumer-argument
            None,
            // template:end messaging:worker-shutdown-test-consumer-argument
            Listeners::default(),
            &cancel,
            &tracker,
            // template:begin jobs:worker-shutdown-test-pool-argument
            None,
            // template:end jobs:worker-shutdown-test-pool-argument
            // template:begin messaging:worker-shutdown-test-messaging-argument
            None,
            // template:end messaging:worker-shutdown-test-messaging-argument
        )
        .await;
        assert!(cancel.is_cancelled());
        assert!(tracker.is_closed());
        assert!(tracker.is_empty());
    }
}
