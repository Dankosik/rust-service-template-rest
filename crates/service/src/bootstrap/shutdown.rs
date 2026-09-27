//! Ordered teardown under one grace-period deadline.
//!
//! readiness off → propagation delay → HTTP drain → diagnostics → cancel and
//! join background tasks → close dependencies → flush telemetry. Every stage
//! draws from what is left of the platform grace period, so a slow stage
//! shortens the ones after it instead of pushing the process into SIGKILL.
//! A startup that failed or was stopped runs the same teardown without the
//! listener stages.

use std::time::Duration;

use health::Readiness;
use infra_http::{Drained, Server};
// template:begin cache:service-shutdown-cache-imports
use infra_cache::Cache;
// template:end cache:service-shutdown-cache-imports
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
const BACKGROUND_JOIN: Duration = Duration::from_secs(5);
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

    /// The deadline of a stage that may take at most `want`.
    fn stage_deadline(&self, want: Duration) -> Instant {
        Instant::now() + self.remaining(want)
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
    }
}

/// Dependencies startup opened. Teardown closes them after the background
/// tasks that use them have joined, on every exit path.
#[derive(Default)]
pub(crate) struct Dependencies {
    // template:begin postgres:shutdown-dependencies-postgres-field
    /// HTTP connection tasks are not in the tracker; `close` waits for any
    /// pooled connections they still hold. Work that outlives close is
    /// dropped by `runtime.shutdown_timeout`.
    pub(crate) postgres: Option<PgPool>,
    // template:end postgres:shutdown-dependencies-postgres-field
    // template:begin cache:service-shutdown-dependencies-cache-field
    /// Not a readiness probe; the connection closes when it drops.
    pub(crate) cache: Option<Cache>,
    // template:end cache:service-shutdown-dependencies-cache-field
}

impl Dependencies {
    /// Close every opened dependency concurrently. Returns whether one
    /// outlived `deadline`.
    async fn close(
        self,
        #[allow(unused_variables, reason = "dependency-free profiles perform no close")]
        deadline: Instant,
    ) -> bool {
        let Self {
            // template:begin postgres:shutdown-dependencies-postgres-destructure
            postgres,
            // template:end postgres:shutdown-dependencies-postgres-destructure
            // template:begin cache:service-shutdown-dependencies-cache-destructure
            cache,
            // template:end cache:service-shutdown-dependencies-cache-destructure
        } = self;
        // template:begin cache:service-shutdown-dependencies-cache-close
        drop(cache);
        // template:end cache:service-shutdown-dependencies-cache-close
        let postgres_close = async {
            // template:begin postgres:shutdown-dependencies-postgres-close
            if let Some(pool) = postgres {
                return match infra_postgres::close(
                    &pool,
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
            // template:end postgres:shutdown-dependencies-postgres-close
            false
        };
        postgres_close.await
    }
}

/// The listeners of an admitted startup.
pub(crate) struct Serving {
    pub(crate) readiness: Readiness,
    pub(crate) app_listener: Server,
    pub(crate) diagnostics: Option<Server>,
    // template:begin grpc:shutdown-serving-grpc-field
    pub(crate) grpc_listener: Option<Server>,
    // template:end grpc:shutdown-serving-grpc-field
}

/// Everything teardown releases.
pub(crate) struct Plan<'a> {
    pub(crate) http_config: &'a HttpConfig,
    pub(crate) signals: &'a mut Signals,
    /// `None` when startup failed or a stop signal ended it before the
    /// listeners were admitted: teardown then skips the listener stages.
    pub(crate) serving: Option<Serving>,
    pub(crate) cancel: CancellationToken,
    pub(crate) tracker: TaskTracker,
    pub(crate) dependencies: Dependencies,
    pub(crate) tracer_provider: TracerProviderHandle,
}

pub(crate) async fn run(plan: Plan<'_>) -> Outcome {
    let Plan {
        http_config,
        signals,
        serving,
        cancel,
        tracker,
        dependencies,
        tracer_provider,
    } = plan;
    let budget = Budget::start(http_config.grace_period);
    tracing::info!(grace = ?http_config.grace_period, "shutdown_started");

    let drain_overran = match serving {
        Some(serving) => stop_serving(serving, http_config, signals, &budget).await,
        None => false,
    };

    cancel.cancel();
    tracker.close();
    let joined = tokio::time::timeout(budget.remaining(BACKGROUND_JOIN), tracker.wait())
        .await
        .is_ok();
    if joined {
        tracing::info!("background_joined");
    } else {
        tracing::warn!("background tasks outlived their join budget");
    }

    let dependency_overran = dependencies
        .close(budget.stage_deadline(DEPENDENCY_CLOSE))
        .await;

    let telemetry_overran = match tracer_provider
        .shutdown(budget.remaining(TELEMETRY_FLUSH))
        .await
    {
        ProviderShutdown::Flushed => {
            tracing::info!("telemetry_flushed");
            false
        }
        ProviderShutdown::Incomplete => true,
    };

    let outcome = if drain_overran || !joined || dependency_overran || telemetry_overran {
        Outcome::Degraded
    } else {
        Outcome::Graceful
    };
    tracing::info!(outcome = ?outcome, "shutdown_completed");
    outcome
}

/// Take the service out of rotation and drain its listeners. Returns whether
/// the drain overran; a diagnostics overrun is forced closed without a vote.
async fn stop_serving(
    serving: Serving,
    http_config: &HttpConfig,
    signals: &mut Signals,
    budget: &Budget,
) -> bool {
    let Serving {
        readiness,
        app_listener,
        diagnostics,
        // template:begin grpc:shutdown-serving-grpc-destructure
        grpc_listener,
        // template:end grpc:shutdown-serving-grpc-destructure
    } = serving;
    readiness.start_drain();
    tracing::info!("readiness_disabled");

    // Keep serving while load balancers notice readiness failing. A second
    // stop signal skips the wait: the operator has decided to hurry.
    let delay = budget.remaining(http_config.readiness_propagation_delay);
    if !delay.is_zero() {
        tracing::info!(delay = ?delay, "readiness_propagation_wait");
        tokio::select! {
            () = tokio::time::sleep(delay) => {},
            () = signals.wait() => tracing::warn!("second stop signal: skipping propagation delay"),
        }
    }

    let http_drain_budget = budget.remaining(http_config.effective_drain_budget());
    tracing::info!(budget = ?http_drain_budget, "drain_started");
    let http_drain = async {
        match app_listener.drain(http_drain_budget).await {
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
        }
    };
    // template:begin grpc:shutdown-concurrent-grpc-drain
    let grpc_drain = async {
        match grpc_listener {
            Some(listener) => match listener.drain(http_drain_budget).await {
                Ok(Drained::Complete) => {
                    tracing::info!("grpc_drain_completed");
                    false
                }
                Ok(Drained::TimedOut {
                    remaining_connections: remaining,
                }) => {
                    tracing::warn!(
                        remaining,
                        reason = "in_flight_requests_outlived_drain_budget",
                        "grpc_drain_forced"
                    );
                    true
                }
                Err(error) => {
                    tracing::error!(error = %error, "grpc_drain_failed");
                    true
                }
            },
            None => false,
        }
    };
    let http_drain = async {
        let (http_overran, grpc_overran) = tokio::join!(http_drain, grpc_drain);
        http_overran || grpc_overran
    };
    // template:end grpc:shutdown-concurrent-grpc-drain
    let drain_overran = http_drain.await;

    if let Some(diagnostics) = diagnostics {
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
    drain_overran
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
