//! Ordered teardown under one grace-period deadline.
//!
//! readiness off → propagation delay → HTTP drain → diagnostics → cancel and
//! join background tasks → close dependencies → flush telemetry. Every stage
//! draws from what is left of the platform grace period, so a slow stage
//! shortens the ones after it instead of pushing the process into SIGKILL.

use std::time::Duration;

use health::Readiness;
use infra_http::{Drained, Server};
// template:begin messaging:service-shutdown-messaging-imports
use infra_messaging::{CloseOutcome, Messaging};
// template:end messaging:service-shutdown-messaging-imports
// template:begin postgres:shutdown-imports
use infra_postgres::{Closed, PgPool};
// template:end postgres:shutdown-imports
use infra_telemetry::{ProviderShutdown, TracerProviderHandle};
use service_config::HttpConfig;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Ceilings for the stages after the HTTP drain. They are process
/// structure, not configuration, so they live here.
const DIAGNOSTICS_SHUTDOWN: Duration = Duration::from_secs(2);
pub(crate) const BACKGROUND_JOIN: Duration = Duration::from_secs(5);
/// Dependency-close ceiling retained in the common grace-period contract.
pub(crate) const DEPENDENCY_CLOSE: Duration = Duration::from_secs(5);
const TELEMETRY_FLUSH: Duration = Duration::from_secs(5);

/// What the stages after the drain need at worst: the four ceilings above,
/// summed as durations. Tracer-provider shutdown also waits a short join
/// slack around `spawn_blocking` after its SDK timeout; that slack is not
/// part of this tail and may use leftover grace after these stages.
pub(crate) const SHUTDOWN_TAIL: Duration = DIAGNOSTICS_SHUTDOWN
    .saturating_add(BACKGROUND_JOIN)
    .saturating_add(DEPENDENCY_CLOSE)
    .saturating_add(TELEMETRY_FLUSH);

#[derive(Debug, thiserror::Error)]
#[error(
    "http.grace_period ({grace:?}) must be >= http.drain_timeout ({drain_timeout:?}) plus the \
     {tail:?} teardown tail (diagnostics, background join, dependency close, telemetry flush)"
)]
pub(crate) struct GraceBudgetError {
    grace: Duration,
    drain_timeout: Duration,
    tail: Duration,
}

/// Reject a drain budget that cannot fit inside the grace period alongside
/// the teardown that follows it.
pub(crate) fn validate_grace_budget(http: &HttpConfig) -> Result<(), GraceBudgetError> {
    if http.grace_period < http.drain_timeout + SHUTDOWN_TAIL {
        return Err(GraceBudgetError {
            grace: http.grace_period,
            drain_timeout: http.drain_timeout,
            tail: SHUTDOWN_TAIL,
        });
    }
    Ok(())
}

/// Cancel and join background work. Used on partial startup so the same
/// owner story as [`run`] applies.
pub(crate) async fn cancel_and_join_background_tasks(
    cancel: &CancellationToken,
    tracker: &TaskTracker,
) {
    let _ = join_background_then_close(cancel, tracker, BACKGROUND_JOIN).await;
}

/// Cancel tracked work and wait for it.
///
/// Returns whether the join finished in time.
async fn join_background_then_close(
    cancel: &CancellationToken,
    tracker: &TaskTracker,
    join_budget: Duration,
) -> bool {
    cancel.cancel();
    tracker.close();
    tokio::time::timeout(join_budget, tracker.wait())
        .await
        .is_ok()
}

/// Close every opened dependency under one deadline, including partial startup.
pub(crate) async fn close_dependencies(
    // template:begin postgres:shutdown-startup-pool-close
    pool: Option<&PgPool>,
    // template:end postgres:shutdown-startup-pool-close
    // template:begin messaging:service-shutdown-startup-messaging-close
    messaging: Option<Messaging>,
    // template:end messaging:service-shutdown-startup-messaging-close
    #[allow(unused_variables, reason = "dependency-free profiles perform no close")]
    deadline: Instant,
) -> bool {
    let postgres_close = async {
        // template:begin postgres:shutdown-dependency-pool-close
        if let Some(pool) = pool {
            return match infra_postgres::close(
                pool,
                deadline.saturating_duration_since(Instant::now()),
            )
            .await
            {
                Closed::Complete => {
                    tracing::info!("postgres_pool_closed");
                    false
                }
                Closed::TimedOut => {
                    tracing::warn!("postgres pool outlived its close budget");
                    true
                }
            };
        }
        // template:end postgres:shutdown-dependency-pool-close
        false
    };
    let messaging_close = async {
        // template:begin messaging:service-shutdown-dependency-messaging-close
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
        // template:end messaging:service-shutdown-dependency-messaging-close
        false
    };
    let (postgres_overran, messaging_overran) = tokio::join!(postgres_close, messaging_close);
    postgres_overran || messaging_overran
}

/// A startup stop has no admitted listener, but owns the usual bounded tail.
pub(crate) async fn finish_stopped_startup(
    cancel: &CancellationToken,
    tracker: &TaskTracker,
    // template:begin postgres:shutdown-stopped-startup-pool-parameter
    pool: Option<&PgPool>,
    // template:end postgres:shutdown-stopped-startup-pool-parameter
    // template:begin messaging:service-shutdown-stopped-startup-messaging-parameter
    messaging: Option<Messaging>,
    // template:end messaging:service-shutdown-stopped-startup-messaging-parameter
    provider: TracerProviderHandle,
    deadline: Instant,
) -> Outcome {
    let budget = Budget { deadline };
    let joined =
        join_background_then_close(cancel, tracker, budget.remaining(BACKGROUND_JOIN)).await;
    let close_overran = close_dependencies(
        // template:begin postgres:shutdown-stopped-startup-pool-argument
        pool,
        // template:end postgres:shutdown-stopped-startup-pool-argument
        // template:begin messaging:service-shutdown-stopped-startup-messaging-argument
        messaging,
        // template:end messaging:service-shutdown-stopped-startup-messaging-argument
        Instant::now() + budget.remaining(DEPENDENCY_CLOSE),
    )
    .await;
    let flushed = matches!(
        provider.shutdown(budget.remaining(TELEMETRY_FLUSH)).await,
        ProviderShutdown::Flushed
    );
    if joined && !close_overran && flushed {
        Outcome::Graceful
    } else {
        Outcome::Degraded
    }
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
}

pub(crate) struct Plan<'a> {
    pub(crate) http_config: &'a HttpConfig,
    pub(crate) readiness: &'a Readiness,
    pub(crate) app_listener: Server,
    pub(crate) diagnostics: Option<Server>,
    pub(crate) cancel: CancellationToken,
    pub(crate) tracker: TaskTracker,
    /// Closed after tracked background tasks joined. HTTP connection tasks
    /// are not in the tracker; `close` waits for any pooled connections they
    /// still hold. Work that outlives close is dropped by
    /// `runtime.shutdown_timeout`.
    // template:begin postgres:shutdown-plan-pool
    pub(crate) postgres_pool: Option<PgPool>,
    // template:end postgres:shutdown-plan-pool
    // template:begin messaging:service-shutdown-plan-messaging
    pub(crate) messaging: Option<Messaging>,
    // template:end messaging:service-shutdown-plan-messaging
    pub(crate) tracer_provider: TracerProviderHandle,
    pub(crate) signals: &'a mut Signals,
}

pub(crate) async fn run(plan: Plan<'_>) -> Outcome {
    let budget = Budget::start(plan.http_config.grace_period);
    tracing::info!(grace = ?plan.http_config.grace_period, "shutdown_started");

    plan.readiness.start_drain();
    tracing::info!("readiness_disabled");

    // Keep serving while load balancers notice readiness failing. A second
    // stop signal skips the wait: the operator has decided to hurry.
    let delay = budget.remaining(plan.http_config.readiness_propagation_delay);
    if !delay.is_zero() {
        tracing::info!(delay = ?delay, "readiness_propagation_wait");
        tokio::select! {
            () = tokio::time::sleep(delay) => {},
            () = plan.signals.wait() => tracing::warn!("second stop signal: skipping propagation delay"),
        }
    }

    let http_drain_budget = budget.remaining(plan.http_config.effective_drain_budget());
    tracing::info!(budget = ?http_drain_budget, "drain_started");
    let drain_overran = match plan.app_listener.drain(http_drain_budget).await {
        Ok(Drained::Complete) => {
            tracing::info!("drain_completed");
            false
        }
        Ok(Drained::TimedOut {
            remaining_connections: remaining,
        }) => {
            // Remaining HTTP connection tasks are not in TaskTracker. The
            // next wait for pooled connections they still hold is
            // `pool.close`; `runtime.shutdown_timeout` is the last drop.
            tracing::warn!(
                remaining,
                reason = "in_flight_requests_outlived_drain_budget",
                "shutdown_forced"
            );
            true
        }
        Err(err) => {
            tracing::error!(error = %err, "drain_failed");
            true
        }
    };

    if let Some(diagnostics) = plan.diagnostics {
        // An in-flight scrape must not park the process past the telemetry
        // flush. A scrape overrun is forced closed so telemetry can flush;
        // it does not vote `degraded`.
        match diagnostics
            .drain(budget.remaining(DIAGNOSTICS_SHUTDOWN))
            .await
        {
            Ok(Drained::Complete) => tracing::info!("diagnostics_stopped"),
            Ok(Drained::TimedOut { .. }) => {
                tracing::warn!(
                    reason = "scrape_outlived_shutdown_budget",
                    "diagnostics_forced"
                );
            }
            Err(err) => tracing::warn!(error = %err, "diagnostics_shutdown_failed"),
        }
    }

    let joined = join_background_then_close(
        &plan.cancel,
        &plan.tracker,
        budget.remaining(BACKGROUND_JOIN),
    )
    .await;
    let join_overran = if joined {
        tracing::info!("background_joined");
        false
    } else {
        tracing::warn!("background tasks outlived their join budget");
        true
    };
    let dependency_overran = close_dependencies(
        // template:begin postgres:shutdown-pool-close-prefix
        plan.postgres_pool.as_ref(),
        // template:end postgres:shutdown-pool-close-prefix
        // template:begin messaging:service-shutdown-close-messaging-argument
        plan.messaging,
        // template:end messaging:service-shutdown-close-messaging-argument
        Instant::now() + budget.remaining(DEPENDENCY_CLOSE),
    )
    .await;

    let telemetry_overran = match plan
        .tracer_provider
        .shutdown(budget.remaining(TELEMETRY_FLUSH))
        .await
    {
        ProviderShutdown::Flushed => {
            tracing::info!("telemetry_flushed");
            false
        }
        ProviderShutdown::Incomplete => true,
    };

    let outcome = if drain_overran || join_overran || dependency_overran || telemetry_overran {
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
            drain_timeout: Duration::from_secs(25),
            ..HttpConfig::default()
        };
        let err = validate_grace_budget(&http).unwrap_err();
        assert!(err.to_string().contains("http.grace_period"), "{err}");
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
}
