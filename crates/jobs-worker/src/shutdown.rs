//! Staged teardown under one grace-period deadline, and the startup abort.
//!
//! Stage order: readiness off and claiming stopped, drain, cleanup after a
//! forced drain, listeners, background join, pool close, telemetry flush.
//! Every stage takes the lesser of its ceiling and what is left of
//! `http.grace_period`.

use std::time::Duration;

use health::Readiness;
use infra_http::{Drained, Server};
use infra_jobs::Started;
use infra_postgres::{Closed, PgPool};
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
    /// `None` when a stop signal ended startup before claiming, so the drain
    /// and cleanup stages are skipped.
    pub(crate) started: Option<&'a Started>,
    pub(crate) listeners: Listeners,
    pub(crate) cancel: CancellationToken,
    pub(crate) tracker: TaskTracker,
    pub(crate) pool: PgPool,
    pub(crate) tracer_provider: TracerProviderHandle,
    pub(crate) signals: &'a mut Signals,
}

/// Runs the stages in order under one deadline started now, and returns
/// whether any stage voted degraded.
pub(crate) async fn run(plan: Plan<'_>) -> Outcome {
    let Plan {
        http,
        readiness,
        started,
        listeners,
        cancel,
        tracker,
        pool,
        tracer_provider,
        signals,
    } = plan;
    let budget = Budget::start(http.grace_period);
    stop_claiming(http, readiness, started);
    let mut degraded = false;
    if let Some(engine) = started
        && drain(engine, http.drain_timeout, &budget, signals).await
    {
        degraded = true;
        if finish_attempts(engine, budget.remaining(CLEANUP)).await {
            degraded = true;
        }
    }
    if close_listeners(listeners, budget.remaining(LISTENERS)).await {
        degraded = true;
    }
    if join_background(&cancel, &tracker, budget.remaining(BACKGROUND_JOIN)).await {
        degraded = true;
    }
    if close_pool(&pool, budget.remaining(DEPENDENCY_CLOSE)).await {
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
    started: Option<&Started>,
    listeners: Listeners,
    cancel: &CancellationToken,
    tracker: &TaskTracker,
    pool: Option<&PgPool>,
) {
    if let Some(started) = started {
        let _ = finish_attempts(started, CLEANUP).await;
    }
    let _ = close_listeners(listeners, LISTENERS).await;
    let _ = join_background(cancel, tracker, BACKGROUND_JOIN).await;
    if let Some(pool) = pool {
        let _ = close_pool(pool, DEPENDENCY_CLOSE).await;
    }
}

fn stop_claiming(http: &HttpConfig, readiness: &Readiness, started: Option<&Started>) {
    tracing::info!(grace = ?http.grace_period, "shutdown_started");
    readiness.start_drain();
    tracing::info!("readiness_disabled");
    if let Some(started) = started {
        started.stop_claiming();
        tracing::info!(in_flight = started.in_flight(), "claiming_stopped");
    }
}

async fn drain(
    started: &Started,
    drain_timeout: Duration,
    budget: &Budget,
    signals: &mut Signals,
) -> bool {
    let drain_budget = budget.remaining(drain_timeout);
    tracing::info!(
        budget = ?drain_budget,
        in_flight = started.in_flight(),
        "drain_started"
    );
    let forced = tokio::select! {
        biased;
        () = started.drained() => None,
        () = tokio::time::sleep(drain_budget) => Some("budget"),
        () = signals.wait() => Some("second_signal"),
    };
    match forced {
        None => {
            tracing::info!("drain_completed");
            false
        }
        Some(reason) => {
            tracing::warn!(in_flight = started.in_flight(), reason, "drain_forced");
            true
        }
    }
}

async fn finish_attempts(started: &Started, budget: Duration) -> bool {
    let end = started.cancel_and_finish(budget).await;
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
        abort_startup(None, Listeners::default(), &cancel, &tracker, None).await;
        assert!(cancel.is_cancelled());
        assert!(tracker.is_closed());
        assert!(tracker.is_empty());
    }
}
