//! Ordered teardown under one grace-period deadline.
//!
//! readiness off → propagation delay → HTTP drain → diagnostics → cancel and
//! join background tasks → close dependencies → flush telemetry. Every stage
//! draws from what is left of the platform grace period, so a slow stage
//! shortens the ones after it instead of pushing the process into SIGKILL.

use std::time::Duration;

use health::Readiness;
use infra_http::{Drained, Server};
use infra_postgres::{Closed, PgPool};
use infra_telemetry::{ProviderShutdown, TracerProviderHandle};
use service_config::HttpConfig;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Ceilings for the stages after the HTTP drain. They are process
/// structure, not configuration, so they live here.
const DIAGNOSTICS_SHUTDOWN: Duration = Duration::from_secs(2);
pub(crate) const BACKGROUND_JOIN: Duration = Duration::from_secs(5);
/// Also the bound for closing a pool that outlived a failed startup.
pub(crate) const DEPENDENCY_CLOSE: Duration = Duration::from_secs(5);
const TELEMETRY_FLUSH: Duration = Duration::from_secs(5);

/// What the stages after the drain need at worst.
pub(crate) const SHUTDOWN_TAIL: Duration = Duration::from_secs(
    DIAGNOSTICS_SHUTDOWN.as_secs()
        + BACKGROUND_JOIN.as_secs()
        + DEPENDENCY_CLOSE.as_secs()
        + TELEMETRY_FLUSH.as_secs(),
);

#[derive(Debug, thiserror::Error)]
#[error(
    "http.grace_period ({grace:?}) must be >= http.shutdown_timeout ({shutdown_timeout:?}) plus the \
     {tail:?} teardown tail (diagnostics, background join, dependency close, telemetry flush)"
)]
pub(crate) struct GraceBudgetError {
    grace: Duration,
    shutdown_timeout: Duration,
    tail: Duration,
}

/// Reject a drain budget that cannot fit inside the grace period alongside
/// the teardown that follows it.
pub(crate) fn validate_grace_budget(http: &HttpConfig) -> Result<(), GraceBudgetError> {
    if http.grace_period < http.shutdown_timeout + SHUTDOWN_TAIL {
        return Err(GraceBudgetError {
            grace: http.grace_period,
            shutdown_timeout: http.shutdown_timeout,
            tail: SHUTDOWN_TAIL,
        });
    }
    Ok(())
}

/// Cancel and join background work, then close an opened pool. Used on
/// partial startup so the same owner story as [`run`] applies: no leftover
/// clone holds a connection while `close` waits.
pub(crate) async fn close_opened_dependencies(
    cancel: &CancellationToken,
    tracker: &TaskTracker,
    postgres_pool: Option<&PgPool>,
) {
    let _ = join_background_then_close(
        cancel,
        tracker,
        postgres_pool,
        BACKGROUND_JOIN,
        DEPENDENCY_CLOSE,
    )
    .await;
}

/// Cancel tracked work, wait for it, then close an opened pool.
///
/// Returns whether the join finished in time and the pool close outcome.
async fn join_background_then_close(
    cancel: &CancellationToken,
    tracker: &TaskTracker,
    postgres_pool: Option<&PgPool>,
    join_budget: Duration,
    close_budget: Duration,
) -> (bool, Option<Closed>) {
    cancel.cancel();
    tracker.close();
    let joined = tokio::time::timeout(join_budget, tracker.wait())
        .await
        .is_ok();
    let closed = if let Some(pool) = postgres_pool {
        Some(infra_postgres::close(pool, close_budget).await)
    } else {
        None
    };
    (joined, closed)
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
/// begins, because that is when the platform's grace period starts.
struct Budget {
    deadline: Instant,
}

impl Budget {
    fn start(grace: Duration) -> Self {
        Self {
            deadline: Instant::now() + grace,
        }
    }

    fn stage(&self, want: Duration) -> Duration {
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
            Ok(Self {
                terminate: signal(SignalKind::terminate())?,
                interrupt: signal(SignalKind::interrupt())?,
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
}

pub(crate) struct Plan<'a> {
    pub(crate) http: &'a HttpConfig,
    pub(crate) readiness: &'a Readiness,
    pub(crate) api: Server,
    pub(crate) diagnostics: Option<Server>,
    pub(crate) cancel: CancellationToken,
    pub(crate) tracker: TaskTracker,
    /// Closed after background tasks joined, so no task still holds a
    /// connection when the pool waits for them to return.
    pub(crate) postgres_pool: Option<PgPool>,
    pub(crate) tracer_provider: TracerProviderHandle,
    pub(crate) signals: &'a mut Signals,
}

pub(crate) async fn run(plan: Plan<'_>) -> Outcome {
    let budget = Budget::start(plan.http.grace_period);
    let mut degraded = false;
    tracing::info!(grace = ?plan.http.grace_period, "shutdown_started");

    plan.readiness.start_drain();
    tracing::info!("readiness_disabled");

    // Keep serving while load balancers notice readiness failing. A second
    // stop signal skips the wait: the operator has decided to hurry.
    let delay = budget.stage(plan.http.readiness_propagation_delay);
    if !delay.is_zero() {
        tracing::info!(delay = ?delay, "readiness_propagation_wait");
        tokio::select! {
            () = tokio::time::sleep(delay) => {},
            () = plan.signals.wait() => tracing::warn!("second stop signal: skipping propagation delay"),
        }
    }

    let drain = budget.stage(plan.http.effective_drain_budget());
    tracing::info!(budget = ?drain, "drain_started");
    match plan.api.shutdown(drain).await {
        Ok(Drained::Complete) => tracing::info!("drain_completed"),
        Ok(Drained::TimedOut {
            remaining_connections: remaining,
        }) => {
            // The connections it gave up on are dropped by the runtime
            // shutdown; the alternative is the same abrupt end at SIGKILL,
            // minus the telemetry.
            tracing::warn!(
                remaining,
                reason = "in_flight_requests_outlived_shutdown_timeout",
                "shutdown_forced"
            );
            degraded = true;
        }
        Err(err) => {
            tracing::error!(error = %err, "drain_failed");
            degraded = true;
        }
    }

    if let Some(diagnostics) = plan.diagnostics {
        // An in-flight scrape must not park the process past the telemetry
        // flush.
        match diagnostics
            .shutdown(budget.stage(DIAGNOSTICS_SHUTDOWN))
            .await
        {
            Ok(Drained::Complete) => tracing::info!("diagnostics_stopped"),
            Ok(Drained::TimedOut { .. }) => {
                tracing::warn!(
                    reason = "scrape_outlived_shutdown_budget",
                    "diagnostics_forced"
                );
                // A scrape overrun is forced closed so telemetry can flush;
                // it does not vote `degraded`.
            }
            Err(err) => tracing::warn!(error = %err, "diagnostics_shutdown_failed"),
        }
    }

    let (joined, closed) = join_background_then_close(
        &plan.cancel,
        &plan.tracker,
        plan.postgres_pool.as_ref(),
        budget.stage(BACKGROUND_JOIN),
        budget.stage(DEPENDENCY_CLOSE),
    )
    .await;
    if joined {
        tracing::info!("background_joined");
    } else {
        tracing::warn!("background tasks outlived their join budget");
        degraded = true;
    }
    match closed {
        None => {}
        Some(Closed::Complete) => tracing::info!("postgres_pool_closed"),
        Some(Closed::TimedOut) => {
            tracing::warn!("postgres pool outlived its close budget");
            degraded = true;
        }
    }

    match plan
        .tracer_provider
        .shutdown(budget.stage(TELEMETRY_FLUSH))
        .await
    {
        ProviderShutdown::Flushed => tracing::info!("telemetry_flushed"),
        ProviderShutdown::Incomplete => degraded = true,
    }

    let outcome = if degraded {
        Outcome::Degraded
    } else {
        Outcome::Graceful
    };
    tracing::info!(outcome = ?outcome, "shutdown_completed");
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budgets_leave_the_tail_inside_the_grace_period() {
        let http = HttpConfig::default();
        validate_grace_budget(&http).unwrap();
        assert_eq!(SHUTDOWN_TAIL, Duration::from_secs(17));
    }

    #[test]
    fn a_drain_that_starves_the_tail_is_rejected() {
        let http = HttpConfig {
            grace_period: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(25),
            ..HttpConfig::default()
        };
        let err = validate_grace_budget(&http).unwrap_err();
        assert!(err.to_string().contains("http.grace_period"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn stages_are_clamped_to_the_remaining_deadline() {
        let budget = Budget::start(Duration::from_secs(10));
        assert_eq!(budget.stage(Duration::from_secs(4)), Duration::from_secs(4));
        tokio::time::advance(Duration::from_secs(8)).await;
        assert_eq!(budget.stage(Duration::from_secs(4)), Duration::from_secs(2));
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(budget.stage(Duration::from_secs(4)), Duration::ZERO);
    }
}
