use std::{panic::AssertUnwindSafe, time::Duration};

use futures_util::FutureExt;
use health::ReadinessControl;
use infra_http_transport::{Drained, Server};
// template:begin cache:service-shutdown-cache-imports
use infra_cache::Cache;
// template:end cache:service-shutdown-cache-imports
// template:begin object-storage:service-shutdown-object-storage-imports
use infra_object_storage::ObjectStorage;
// template:end object-storage:service-shutdown-object-storage-imports
// template:begin postgres:shutdown-imports
use infra_postgres::{CloseOutcome, PoolStartup};
// template:end postgres:shutdown-imports
use infra_telemetry::{
    LoggerGuard, LoggerShutdown, ProviderShutdown, SHUTDOWN_JOIN_SLACK, TracerProviderHandle,
};
use infra_temporal::TemporalStartup;
use service_config::{HttpConfig, ShutdownTail};
use tokio::time::Instant;

use super::background::{Background, JoinOutcome};
use super::signals::Signals;

const DIAGNOSTICS_SHUTDOWN: Duration = Duration::from_secs(2);
const FORCED_LISTENER_JOIN: Duration = Duration::from_millis(500);
const BACKGROUND_JOIN: Duration = Duration::from_secs(5);
const DEPENDENCY_CLOSE: Duration = Duration::from_secs(5);
const TELEMETRY_FLUSH: Duration = Duration::from_secs(5);
const LOGGER_FLUSH_RESERVE: Duration = Duration::from_secs(1);
const SHUTDOWN_TAIL: Duration = DIAGNOSTICS_SHUTDOWN
    .saturating_add(BACKGROUND_JOIN)
    .saturating_add(DEPENDENCY_CLOSE)
    .saturating_add(TELEMETRY_FLUSH)
    .saturating_add(SHUTDOWN_JOIN_SLACK)
    .saturating_add(super::RUNTIME_SHUTDOWN_TIMEOUT);

pub(crate) const TAIL: ShutdownTail = ShutdownTail::new(
    SHUTDOWN_TAIL,
    "teardown tail (diagnostics, background join, dependency close, telemetry flush, SDK join, runtime)",
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Outcome {
    Graceful,
    Degraded,
}

#[derive(Clone, Copy)]
struct DrainDeadlines {
    graceful: Instant,
    confirmation: Instant,
}

impl DrainDeadlines {
    fn remaining(self) -> Duration {
        self.graceful.saturating_duration_since(Instant::now())
    }
}

struct Budget {
    async_deadline: Instant,
}

impl Budget {
    fn until(deadline: Instant) -> Self {
        Self {
            async_deadline: deadline - super::RUNTIME_SHUTDOWN_TIMEOUT,
        }
    }

    fn stage_budget(&self, want: Duration) -> Duration {
        want.min(
            self.async_deadline
                .saturating_duration_since(Instant::now()),
        )
    }

    fn stage_deadline(&self, want: Duration) -> Instant {
        self.async_deadline.min(Instant::now() + want)
    }

    fn drain_deadlines(
        &self,
        requested_drain: Duration,
        overall_drain_deadline: Instant,
    ) -> DrainDeadlines {
        let graceful = self
            .stage_deadline(requested_drain)
            .min(overall_drain_deadline);
        let confirmation = self.async_deadline.min(graceful + FORCED_LISTENER_JOIN);
        DrainDeadlines {
            graceful,
            confirmation,
        }
    }
}

#[derive(Default)]
pub(crate) struct Dependencies {
    // template:begin postgres:shutdown-dependencies-postgres-field
    pub(crate) postgres: Option<PoolStartup>,
    // template:end postgres:shutdown-dependencies-postgres-field
    // template:begin cache:service-shutdown-dependencies-cache-field
    pub(crate) cache: Option<Cache>,
    // template:end cache:service-shutdown-dependencies-cache-field
    // template:begin object-storage:service-shutdown-dependencies-object-storage-field
    pub(crate) object_storage: Option<ObjectStorage>,
    // template:end object-storage:service-shutdown-dependencies-object-storage-field
    pub(crate) temporal: Option<TemporalStartup>,
}

impl Dependencies {
    async fn close(
        self,
        #[allow(unused_variables, reason = "dependency-free profiles perform no close")]
        deadline: Instant,
    ) -> Outcome {
        let Self {
            // template:begin postgres:shutdown-dependencies-postgres-destructure
            postgres,
            // template:end postgres:shutdown-dependencies-postgres-destructure
            // template:begin cache:service-shutdown-dependencies-cache-destructure
            cache,
            // template:end cache:service-shutdown-dependencies-cache-destructure
            // template:begin object-storage:service-shutdown-dependencies-object-storage-destructure
            object_storage,
            // template:end object-storage:service-shutdown-dependencies-object-storage-destructure
            temporal,
        } = self;
        // template:begin object-storage:service-shutdown-dependencies-object-storage-close
        drop(object_storage);
        // template:end object-storage:service-shutdown-dependencies-object-storage-close
        let cache_close = async {
            // template:begin cache:service-shutdown-dependencies-cache-close
            if let Some(cache) = cache {
                return match cache
                    .close(deadline.saturating_duration_since(Instant::now()))
                    .await
                {
                    infra_cache::CloseOutcome::Complete => {
                        tracing::info!("cache_closed");
                        Outcome::Graceful
                    }
                    infra_cache::CloseOutcome::TimedOut => {
                        tracing::warn!("cache outlived its close budget");
                        Outcome::Degraded
                    }
                };
            }
            // template:end cache:service-shutdown-dependencies-cache-close
            Outcome::Graceful
        };
        let postgres_close = async {
            // template:begin postgres:shutdown-dependencies-postgres-close
            if let Some(pool) = postgres {
                return match pool
                    .close(deadline.saturating_duration_since(Instant::now()))
                    .await
                {
                    CloseOutcome::Complete => {
                        tracing::info!("postgres_pool_closed");
                        Outcome::Graceful
                    }
                    CloseOutcome::TimedOut => {
                        tracing::warn!("postgres pool outlived its close budget");
                        Outcome::Degraded
                    }
                };
            }
            // template:end postgres:shutdown-dependencies-postgres-close
            Outcome::Graceful
        };
        let temporal_close = async {
            if let Some(temporal) = temporal {
                return match temporal.close(deadline).await {
                    infra_temporal::CloseOutcome::Complete => {
                        tracing::info!("temporal_closed");
                        Outcome::Graceful
                    }
                    infra_temporal::CloseOutcome::TimedOut => {
                        tracing::warn!("temporal outlived its close budget");
                        Outcome::Degraded
                    }
                };
            }
            Outcome::Graceful
        };
        let (cache_outcome, postgres_outcome, temporal_outcome) =
            tokio::join!(cache_close, postgres_close, temporal_close);
        cache_outcome.max(postgres_outcome).max(temporal_outcome)
    }
}

#[derive(Default)]
pub(crate) struct Telemetry {
    pub(crate) tracer_provider: Option<TracerProviderHandle>,
    pub(crate) logger: Option<LoggerGuard>,
}

#[derive(Default)]
pub(crate) enum ReadinessState {
    #[default]
    Uninstalled,
    Prepared(ReadinessControl),
    Admitted(ReadinessControl),
}

impl ReadinessState {
    pub(crate) fn control(&self) -> Option<&ReadinessControl> {
        match self {
            Self::Uninstalled => None,
            Self::Prepared(control) | Self::Admitted(control) => Some(control),
        }
    }

    pub(crate) fn mark_admitted(&mut self) {
        *self = match std::mem::take(self) {
            Self::Prepared(control) | Self::Admitted(control) => Self::Admitted(control),
            Self::Uninstalled => Self::Uninstalled,
        };
    }

    #[cfg(test)]
    pub(crate) fn is_admitted(&self) -> bool {
        matches!(self, Self::Admitted(_))
    }
}

#[derive(Default)]
pub(crate) struct TrafficListeners {
    pub(crate) http: Option<Server>,
    // template:begin grpc:shutdown-serving-grpc-field
    pub(crate) grpc: Option<Server>,
    // template:end grpc:shutdown-serving-grpc-field
}

#[derive(Default)]
pub(crate) struct Serving {
    pub(crate) readiness: ReadinessState,
    pub(crate) traffic: TrafficListeners,
    pub(crate) diagnostics: Option<Server>,
}

pub(crate) struct Plan<'a> {
    pub(crate) http_config: &'a HttpConfig,
    pub(crate) signals: &'a mut Signals,
    pub(crate) deadline: Instant,
    pub(crate) serving: Serving,
    pub(crate) background: Background,
    pub(crate) dependencies: Dependencies,
    pub(crate) telemetry: Telemetry,
}

async fn stage(name: &'static str, future: impl Future<Output = Outcome>) -> Outcome {
    if let Ok(outcome) = AssertUnwindSafe(future).catch_unwind().await {
        outcome
    } else {
        tracing::error!(
            stage = name,
            outcome = "unconfirmed",
            "shutdown_stage_panicked"
        );
        Outcome::Degraded
    }
}

pub(crate) async fn run(plan: Plan<'_>) -> Outcome {
    let Plan {
        http_config,
        signals,
        deadline,
        serving:
            Serving {
                readiness,
                traffic,
                diagnostics,
            },
        mut background,
        dependencies,
        telemetry,
    } = plan;
    let budget = Budget::until(deadline);
    let mut outcome = stage("listeners", async {
        tracing::info!(grace = ?http_config.grace_period, "shutdown_started");
        stop_serving(&readiness, traffic, http_config, signals, &budget).await
    })
    .await;
    if let Some(diagnostics) = diagnostics {
        outcome = outcome.max(stage("diagnostics", stop_diagnostics(diagnostics, &budget)).await);
    }
    drop(readiness);

    background.cancel();
    outcome = outcome.max(stage("background", join_background(&mut background, &budget)).await);
    let dependency_deadline = budget.stage_deadline(DEPENDENCY_CLOSE);
    outcome = outcome.max(
        stage(
            "background_forced",
            confirm_background_abort(&mut background, dependency_deadline),
        )
        .await,
    );
    drop(background);
    outcome = outcome.max(stage("dependencies", dependencies.close(dependency_deadline)).await);
    finish_telemetry(telemetry, signals, &budget, outcome).await
}

async fn join_background(background: &mut Background, budget: &Budget) -> Outcome {
    match background
        .join_until(budget.stage_deadline(BACKGROUND_JOIN))
        .await
    {
        JoinOutcome::Complete => {
            tracing::info!("background_joined");
            Outcome::Graceful
        }
        JoinOutcome::Failed => {
            tracing::warn!("background_failed");
            Outcome::Degraded
        }
        JoinOutcome::Unconfirmed => {
            background.abort_all();
            tracing::warn!("background_abort_requested");
            Outcome::Degraded
        }
    }
}

async fn confirm_background_abort(
    background: &mut Background,
    dependency_deadline: Instant,
) -> Outcome {
    if background.is_empty() {
        return Outcome::Graceful;
    }
    background.abort_all();
    match background.join_until(dependency_deadline).await {
        JoinOutcome::Unconfirmed => tracing::warn!("background_unconfirmed"),
        JoinOutcome::Complete | JoinOutcome::Failed => tracing::warn!("background_forced"),
    }
    Outcome::Degraded
}

async fn finish_telemetry(
    telemetry: Telemetry,
    signals: &mut Signals,
    budget: &Budget,
    mut outcome: Outcome,
) -> Outcome {
    let Telemetry {
        tracer_provider,
        logger,
    } = telemetry;
    let telemetry_deadline =
        budget.stage_deadline(TELEMETRY_FLUSH.saturating_add(SHUTDOWN_JOIN_SLACK));
    let log_reserve = if let Some(logger) = &logger {
        logger.begin_shutdown();
        LOGGER_FLUSH_RESERVE.min(telemetry_deadline.saturating_duration_since(Instant::now()))
    } else {
        Duration::ZERO
    };
    let trace_deadline = telemetry_deadline - log_reserve;
    if let Some(provider) = tracer_provider {
        outcome = outcome.max(stage("telemetry", shutdown_tracer(provider, trace_deadline)).await);
    }
    if let Err(error) = signals.take_pending() {
        outcome = outcome.max(signal_owner_failed(error));
    }
    tracing::info!(?outcome, logger_pending = true, "shutdown_finishing");
    if let Some(logger) = logger {
        outcome = outcome.max(stage("logger", shutdown_logger(logger, telemetry_deadline)).await);
    }
    outcome
}

async fn shutdown_tracer(provider: TracerProviderHandle, deadline: Instant) -> Outcome {
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
}

async fn shutdown_logger(logger: LoggerGuard, deadline: Instant) -> Outcome {
    match logger.shutdown_async(deadline).await {
        LoggerShutdown::Completed => Outcome::Graceful,
        LoggerShutdown::Incomplete(_) => Outcome::Degraded,
    }
}

fn signal_owner_failed(error: super::signals::SignalError) -> Outcome {
    tracing::error!(%error, "signal_owner_failed");
    Outcome::Degraded
}

async fn stop_diagnostics(server: Server, budget: &Budget) -> Outcome {
    match server
        .drain(budget.stage_budget(DIAGNOSTICS_SHUTDOWN))
        .await
    {
        Ok(Drained::Complete) => tracing::info!("diagnostics_stopped"),
        Ok(Drained::TimedOut { .. }) => tracing::warn!(
            reason = "scrape_outlived_shutdown_budget",
            "diagnostics_forced"
        ),
        Err(error) => {
            tracing::warn!(%error, "diagnostics_shutdown_failed");
            return Outcome::Degraded;
        }
    }
    Outcome::Graceful
}

async fn wait_readiness_propagation(delay: Duration, signals: &mut Signals) -> Outcome {
    if delay.is_zero() {
        return Outcome::Graceful;
    }
    tracing::info!(?delay, "readiness_propagation_wait");
    tokio::select! {
        () = tokio::time::sleep(delay) => Outcome::Graceful,
        signal = signals.wait() => match signal {
            Ok(()) => {
                tracing::warn!("second stop signal: skipping propagation delay");
                Outcome::Graceful
            }
            Err(error) => signal_owner_failed(error),
        },
    }
}

async fn stop_serving(
    readiness: &ReadinessState,
    traffic: TrafficListeners,
    http_config: &HttpConfig,
    signals: &mut Signals,
    budget: &Budget,
) -> Outcome {
    if let Some(control) = readiness.control() {
        control.start_drain();
        tracing::info!("readiness_disabled");
    }
    let overall_drain_deadline = budget.stage_deadline(http_config.drain_timeout);
    let (propagation_outcome, requested_drain) = match readiness {
        ReadinessState::Admitted(_) => (
            wait_readiness_propagation(
                budget.stage_budget(http_config.readiness_propagation_delay),
                signals,
            )
            .await,
            http_config.effective_drain_budget(),
        ),
        ReadinessState::Uninstalled | ReadinessState::Prepared(_) => {
            (Outcome::Graceful, http_config.drain_timeout)
        }
    };
    let deadlines = budget.drain_deadlines(requested_drain, overall_drain_deadline);
    tracing::info!(budget = ?deadlines.remaining(), "drain_started");
    let drain = drain_server(traffic.http, deadlines, &DrainEvents::HTTP);
    // template:begin grpc:shutdown-concurrent-grpc-drain
    let grpc_drain = drain_server(traffic.grpc, deadlines, &DrainEvents::GRPC);
    let drain = async {
        let (http_outcome, grpc_outcome) = tokio::join!(drain, grpc_drain);
        http_outcome.max(grpc_outcome)
    };
    // template:end grpc:shutdown-concurrent-grpc-drain
    propagation_outcome.max(drain.await)
}

struct DrainEvents {
    completed: &'static str,
    forced: &'static str,
    failed: &'static str,
}

impl DrainEvents {
    const HTTP: Self = Self {
        completed: "drain_completed",
        forced: "shutdown_forced",
        failed: "drain_failed",
    };
    // template:begin grpc:shutdown-grpc-drain-events
    const GRPC: Self = Self {
        completed: "grpc_drain_completed",
        forced: "grpc_drain_forced",
        failed: "grpc_drain_failed",
    };
    // template:end grpc:shutdown-grpc-drain-events
}

async fn drain_server(
    server: Option<Server>,
    deadlines: DrainDeadlines,
    events: &DrainEvents,
) -> Outcome {
    let Some(server) = server else {
        return Outcome::Graceful;
    };
    match server
        .drain_until(deadlines.graceful, deadlines.confirmation)
        .await
    {
        Ok(Drained::Complete) => {
            tracing::info!("{}", events.completed);
            Outcome::Graceful
        }
        Ok(Drained::TimedOut {
            remaining_at_deadline,
            residue,
        }) => {
            tracing::warn!(
                remaining = remaining_at_deadline,
                accept_unconfirmed = residue.accept_unconfirmed,
                remaining_connections = residue.remaining_connections,
                remaining_delegated_tasks = residue.remaining_delegated_tasks,
                reason = "in_flight_requests_outlived_drain_budget",
                "{}",
                events.forced
            );
            Outcome::Degraded
        }
        Err(error) => {
            let residue = error.residue();
            tracing::error!(
                %error,
                accept_unconfirmed = residue.accept_unconfirmed,
                remaining_connections = residue.remaining_connections,
                remaining_delegated_tasks = residue.remaining_delegated_tasks,
                "{}", events.failed
            );
            Outcome::Degraded
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[test]
    fn grace_covers_the_whole_tail_including_equality() {
        super::super::test_config(&[]);
        super::super::test_config(&[("APP__HTTP__GRACE_PERIOD", "43.5s")]);
        let refused = service_config::test_support::service(
            &[("APP__HTTP__GRACE_PERIOD", "43.499999999s")],
            super::super::BUILD_INFO,
            TAIL,
        );
        assert!(
            refused
                .unwrap_err()
                .validation_errors()
                .is_some_and(|errors| errors.names("http.grace_period"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stages_reserve_runtime_and_never_renew_the_deadline() {
        let budget = Budget::until(Instant::now() + Duration::from_secs(10));
        assert_eq!(
            budget.stage_budget(Duration::from_secs(20)),
            Duration::from_secs(9)
        );
        tokio::time::advance(Duration::from_secs(8)).await;
        assert_eq!(
            budget.stage_budget(Duration::from_secs(4)),
            Duration::from_secs(1)
        );
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(budget.stage_budget(Duration::from_secs(4)), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn ignored_cancellation_is_aborted_and_acknowledged_before_shutdown_returns() {
        let cancel = CancellationToken::new();
        let mut background = Background::new(cancel.clone());
        let (started, running) = tokio::sync::oneshot::channel();
        let (dropped, mut completed) = tokio::sync::oneshot::channel();
        background.spawn("ignores_cancel", async move {
            let _guard = std::mem::DropGuard::new(dropped, |dropped| {
                let _ = dropped.send(());
            });
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        running.await.unwrap();
        let config = super::super::test_config(&[]);
        let http_config = config.http();
        let mut signals = Signals::install().unwrap();
        let outcome = run(Plan {
            http_config,
            signals: &mut signals,
            deadline: Instant::now() + http_config.grace_period,
            serving: Serving::default(),
            background,
            dependencies: Dependencies::default(),
            telemetry: Telemetry::default(),
        })
        .await;
        assert_eq!(outcome, Outcome::Degraded);
        assert_eq!(completed.try_recv(), Ok(()));
    }

    #[tokio::test]
    async fn a_task_panicking_after_shutdown_cancellation_degrades_the_stop() {
        let cancel = CancellationToken::new();
        let mut background = Background::new(cancel.clone());
        background.spawn("cleanup_panic", async move {
            cancel.cancelled().await;
            panic!("cleanup defect")
        });
        let config = super::super::test_config(&[]);
        let http_config = config.http();
        let mut signals = Signals::install().unwrap();
        assert_eq!(
            run(Plan {
                http_config,
                signals: &mut signals,
                deadline: Instant::now() + http_config.grace_period,
                serving: Serving::default(),
                background,
                dependencies: Dependencies::default(),
                telemetry: Telemetry::default(),
            })
            .await,
            Outcome::Degraded
        );
    }

    // template:begin postgres:shutdown-test-listener-before-pool-close
    async fn hold_request_until_shutdown(
        entered: CancellationToken,
        dropping: CancellationToken,
        held: std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
        pool: infra_postgres::Pool,
        released_before_close: CancellationToken,
    ) -> &'static str {
        let _guard = std::mem::DropGuard::new(held, move |held| {
            dropping.cancel();
            tokio::task::block_in_place(|| {
                let _ = held.lock().unwrap().recv_timeout(Duration::from_secs(3));
            });
            if !pool.is_closed() {
                released_before_close.cancel();
            }
        });
        entered.cancel();
        std::future::pending().await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forced_handler_releases_before_postgres_close_and_shutdown_stays_degraded() {
        use std::sync::{Arc, Mutex, mpsc};

        use infra_http_transport::{ConnectionLimits, Http1Options};
        use tokio::io::AsyncWriteExt;

        let entered = CancellationToken::new();
        let dropping = CancellationToken::new();
        let released_before_close = CancellationToken::new();
        let (release, held) = mpsc::channel();
        let held = Arc::new(Mutex::new(held));
        let mut dependencies = Dependencies::default();
        let pool =
            infra_postgres::test_support::connect_lazy("postgres://localhost/unused").unwrap();
        dependencies.postgres = Some(infra_postgres::test_support::startup_from_pool(
            pool.clone(),
        ));
        let handler = {
            let entered = entered.clone();
            let dropping = dropping.clone();
            let pool = pool.clone();
            let released_before_close = released_before_close.clone();
            move || {
                hold_request_until_shutdown(
                    entered.clone(),
                    dropping.clone(),
                    held.clone(),
                    pool.clone(),
                    released_before_close.clone(),
                )
            }
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "shutdown fixture registers a raw route"
        )]
        let app = axum::Router::new().route("/", axum::routing::get(handler));
        let server = Server::bind(
            "127.0.0.1:0".parse().unwrap(),
            app,
            Http1Options {
                header_read_timeout: Duration::from_secs(2),
                max_read_buffer_bytes: 8 * 1024,
                connections: ConnectionLimits {
                    max_connections: None,
                    max_connection_age: None,
                },
            },
        )
        .unwrap();
        let mut peer = tokio::net::TcpStream::connect(server.local_addr())
            .await
            .unwrap();
        peer.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), entered.cancelled())
            .await
            .unwrap();
        let mut http_config = super::super::test_config(&[]).http().clone();
        http_config.drain_timeout = Duration::from_millis(30);
        let mut signals = Signals::install().unwrap();
        let mut serving = Serving::default();
        serving.traffic.http = Some(server);
        let shutdown = run(Plan {
            http_config: &http_config,
            signals: &mut signals,
            deadline: Instant::now() + http_config.grace_period,
            serving,
            background: Background::new(CancellationToken::new()),
            dependencies,
            telemetry: Telemetry::default(),
        });
        let check_order = async {
            tokio::time::timeout(Duration::from_secs(2), dropping.cancelled())
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(30),
                    infra_postgres::test_support::as_sqlx(&pool).close_event(),
                )
                .await
                .is_err()
            );
            release.send(()).unwrap();
        };
        let (outcome, ()) = tokio::join!(shutdown, check_order);
        assert_eq!(outcome, Outcome::Degraded);
        assert!(
            released_before_close.is_cancelled(),
            "dependencies closed before handler release"
        );
        assert!(pool.is_closed());
    }
    // template:end postgres:shutdown-test-listener-before-pool-close
}
