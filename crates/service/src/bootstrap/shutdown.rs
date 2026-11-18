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
use infra_postgres::{CloseOutcome, PgPool};
// template:end postgres:shutdown-imports
use infra_telemetry::{
    LoggerGuard, LoggerShutdown, ProviderShutdown, SHUTDOWN_JOIN_SLACK, TracerProviderHandle,
};
use service_config::{HttpConfig, ShutdownTail};
use tokio::time::Instant;

use super::background::{Background, JoinOutcome};
use super::signals::Signals;

const DIAGNOSTICS_SHUTDOWN: Duration = Duration::from_secs(2);
const FORCED_LISTENER_JOIN: Duration = Duration::from_millis(500);
const BACKGROUND_JOIN: Duration = Duration::from_secs(5);
pub(crate) const DEPENDENCY_CLOSE: Duration = Duration::from_secs(5);
const TELEMETRY_FLUSH: Duration = Duration::from_secs(5);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Graceful,
    Degraded,
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

    fn remaining(&self, want: Duration) -> Duration {
        want.min(
            self.async_deadline
                .saturating_duration_since(Instant::now()),
        )
    }

    fn stage_deadline(&self, want: Duration) -> Instant {
        self.async_deadline.min(Instant::now() + want)
    }
}

#[derive(Default)]
pub(crate) struct Dependencies {
    // template:begin postgres:shutdown-dependencies-postgres-field
    pub(crate) postgres: Option<PgPool>,
    // template:end postgres:shutdown-dependencies-postgres-field
    // template:begin cache:service-shutdown-dependencies-cache-field
    pub(crate) cache: Option<Cache>,
    // template:end cache:service-shutdown-dependencies-cache-field
    // template:begin object-storage:service-shutdown-dependencies-object-storage-field
    pub(crate) object_storage: Option<ObjectStorage>,
    // template:end object-storage:service-shutdown-dependencies-object-storage-field
}

impl Dependencies {
    fn close(
        self,
        #[allow(unused_variables, reason = "dependency-free profiles perform no close")]
        deadline: Instant,
    ) -> impl std::future::Future<Output = bool> {
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
        } = self;
        // template:begin object-storage:service-shutdown-dependencies-object-storage-close
        drop(object_storage);
        // template:end object-storage:service-shutdown-dependencies-object-storage-close
        async move {
            let cache_close = async {
                // template:begin cache:service-shutdown-dependencies-cache-close
                if let Some(cache) = cache {
                    return match cache.close(deadline).await {
                        infra_cache::CloseOutcome::Complete => {
                            tracing::info!("cache_closed");
                            false
                        }
                        infra_cache::CloseOutcome::TimedOut => {
                            tracing::warn!("cache outlived its close budget");
                            true
                        }
                    };
                }
                // template:end cache:service-shutdown-dependencies-cache-close
                false
            };
            let postgres_close = async {
                // template:begin postgres:shutdown-dependencies-postgres-close
                if let Some(pool) = postgres {
                    return match infra_postgres::close(
                        &pool,
                        deadline.saturating_duration_since(Instant::now()),
                    )
                    .await
                    {
                        CloseOutcome::Complete => {
                            tracing::info!("postgres_pool_closed");
                            false
                        }
                        CloseOutcome::TimedOut => {
                            tracing::warn!("postgres pool outlived its close budget");
                            true
                        }
                    };
                }
                // template:end postgres:shutdown-dependencies-postgres-close
                false
            };
            let (cache_degraded, postgres_degraded) = tokio::join!(cache_close, postgres_close);
            cache_degraded || postgres_degraded
        }
    }
}

#[derive(Default)]
pub(crate) struct Serving {
    pub(crate) readiness: Option<ReadinessControl>,
    pub(crate) app_listener: Option<Server>,
    pub(crate) diagnostics: Option<Server>,
    // template:begin grpc:shutdown-serving-grpc-field
    pub(crate) grpc_listener: Option<Server>,
    // template:end grpc:shutdown-serving-grpc-field
    pub(crate) admitted: bool,
}

pub(crate) struct Plan<'a> {
    pub(crate) http_config: &'a HttpConfig,
    pub(crate) signals: &'a mut Signals,
    pub(crate) deadline: Instant,
    pub(crate) serving: Serving,
    pub(crate) background: Background,
    pub(crate) dependencies: Dependencies,
    pub(crate) tracer_provider: Option<TracerProviderHandle>,
    pub(crate) logger: Option<LoggerGuard>,
}

async fn stage(name: &'static str, future: impl Future<Output = bool>) -> bool {
    if let Ok(degraded) = AssertUnwindSafe(future).catch_unwind().await {
        degraded
    } else {
        tracing::error!(
            stage = name,
            outcome = "unconfirmed",
            "shutdown_stage_panicked"
        );
        true
    }
}

pub(crate) async fn run(plan: Plan<'_>) -> Outcome {
    let Plan {
        http_config,
        signals,
        deadline,
        mut serving,
        mut background,
        dependencies,
        tracer_provider,
        logger,
    } = plan;
    let budget = Budget::until(deadline);
    let mut degraded = stage("listeners", async {
        tracing::info!(grace = ?http_config.grace_period, "shutdown_started");
        stop_serving(&mut serving, http_config, signals, &budget).await
    })
    .await;
    degraded |= stage(
        "diagnostics",
        stop_diagnostics(serving.diagnostics.take(), &budget),
    )
    .await;
    drop(serving);

    background.cancel();
    degraded |= stage("background", async {
        match background
            .join_until(budget.stage_deadline(BACKGROUND_JOIN))
            .await
        {
            JoinOutcome::Complete => {
                tracing::info!("background_joined");
                false
            }
            JoinOutcome::Failed => {
                tracing::warn!("background_failed");
                true
            }
            JoinOutcome::Unconfirmed => {
                background.abort_all();
                tracing::warn!("background_abort_requested");
                true
            }
        }
    })
    .await;
    let dependency_deadline = budget.stage_deadline(DEPENDENCY_CLOSE);
    degraded |= stage("background_forced", async {
        if background.is_empty() {
            return false;
        }
        background.abort_all();
        match background.join_until(dependency_deadline).await {
            JoinOutcome::Unconfirmed => tracing::warn!("background_unconfirmed"),
            JoinOutcome::Complete | JoinOutcome::Failed => tracing::warn!("background_forced"),
        }
        true
    })
    .await;
    degraded |= stage("dependencies", async {
        dependencies.close(dependency_deadline).await
    })
    .await;
    finish_telemetry(tracer_provider, logger, signals, &budget, degraded).await
}

async fn finish_telemetry(
    tracer_provider: Option<TracerProviderHandle>,
    logger: Option<LoggerGuard>,
    signals: &mut Signals,
    budget: &Budget,
    mut degraded: bool,
) -> Outcome {
    let telemetry_deadline =
        budget.stage_deadline(TELEMETRY_FLUSH.saturating_add(SHUTDOWN_JOIN_SLACK));
    if let Some(logger) = &logger {
        logger.begin_shutdown();
    }
    let log_reserve = if logger.is_some() {
        Duration::from_secs(1).min(telemetry_deadline.saturating_duration_since(Instant::now()))
    } else {
        Duration::ZERO
    };
    let trace_deadline = telemetry_deadline - log_reserve;
    degraded |= stage("telemetry", async {
        let Some(provider) = tracer_provider else {
            return false;
        };
        match provider.shutdown(trace_deadline).await {
            ProviderShutdown::Completed => {
                tracing::info!(delivery_confirmed = false, "trace_shutdown_completed");
                false
            }
            ProviderShutdown::Incomplete(reasons) => {
                tracing::warn!(
                    reasons = reasons.bits(),
                    reason_flags = ?reasons,
                    "trace_shutdown_incomplete"
                );
                true
            }
        }
    })
    .await;
    if let Err(error) = signals.pending() {
        degraded = true;
        tracing::error!(%error, "signal_owner_failed");
    }
    let mut outcome = if degraded {
        Outcome::Degraded
    } else {
        Outcome::Graceful
    };
    tracing::info!(?outcome, logger_pending = true, "shutdown_finishing");
    if let Some(logger) = logger {
        let incomplete = stage("logger", async {
            let close =
                tokio::task::spawn_blocking(move || logger.shutdown(telemetry_deadline.into_std()));
            !matches!(
                tokio::time::timeout_at(telemetry_deadline, close).await,
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

async fn stop_diagnostics(listener: Option<Server>, budget: &Budget) -> bool {
    let Some(listener) = listener else {
        return false;
    };
    match listener.drain(budget.remaining(DIAGNOSTICS_SHUTDOWN)).await {
        Ok(Drained::Complete) => tracing::info!("diagnostics_stopped"),
        Ok(Drained::TimedOut { .. }) => tracing::warn!(
            reason = "scrape_outlived_shutdown_budget",
            "diagnostics_forced"
        ),
        Err(error) => {
            tracing::warn!(%error, "diagnostics_shutdown_failed");
            return true;
        }
    }
    false
}

async fn stop_serving(
    serving: &mut Serving,
    http_config: &HttpConfig,
    signals: &mut Signals,
    budget: &Budget,
) -> bool {
    if let Some(readiness) = &serving.readiness {
        readiness.start_drain();
        tracing::info!("readiness_disabled");
    }
    let drain_deadline = budget.stage_deadline(http_config.drain_timeout);
    let mut degraded = false;
    if serving.admitted {
        let delay = budget.remaining(http_config.readiness_propagation_delay);
        if !delay.is_zero() {
            tracing::info!(?delay, "readiness_propagation_wait");
            tokio::select! {
                () = tokio::time::sleep(delay) => {},
                signal = signals.wait() => match signal {
                    Ok(()) => tracing::warn!("second stop signal: skipping propagation delay"),
                    Err(error) => { degraded = true; tracing::error!(%error, "signal_owner_failed"); }
                },
            }
        }
    }
    let wanted = if serving.admitted {
        http_config.effective_drain_budget()
    } else {
        http_config.drain_timeout
    };
    let graceful_deadline = budget.stage_deadline(wanted).min(drain_deadline);
    let confirmation_deadline = budget
        .async_deadline
        .min(graceful_deadline + FORCED_LISTENER_JOIN);
    let drain_budget = graceful_deadline.saturating_duration_since(Instant::now());
    tracing::info!(budget = ?drain_budget, "drain_started");
    let drain_failed = drain_listener(
        serving.app_listener.take(),
        graceful_deadline,
        confirmation_deadline,
        ListenerKind::Http,
    );
    // template:begin grpc:shutdown-concurrent-grpc-drain
    let drain_failed = async {
        let (http_failed, grpc_failed) = tokio::join!(
            drain_failed,
            drain_listener(
                serving.grpc_listener.take(),
                graceful_deadline,
                confirmation_deadline,
                ListenerKind::Grpc
            )
        );
        http_failed || grpc_failed
    };
    // template:end grpc:shutdown-concurrent-grpc-drain
    degraded | drain_failed.await
}

#[derive(Clone, Copy)]
enum ListenerKind {
    Http,
    // template:begin grpc:shutdown-listener-kind-grpc
    Grpc,
    // template:end grpc:shutdown-listener-kind-grpc
}

struct DrainEvents {
    completed: &'static str,
    forced: &'static str,
    failed: &'static str,
}

impl ListenerKind {
    const fn drain_events(self) -> DrainEvents {
        match self {
            Self::Http => DrainEvents {
                completed: "drain_completed",
                forced: "shutdown_forced",
                failed: "drain_failed",
            },
            // template:begin grpc:shutdown-listener-kind-grpc-events
            Self::Grpc => DrainEvents {
                completed: "grpc_drain_completed",
                forced: "grpc_drain_forced",
                failed: "grpc_drain_failed",
            },
            // template:end grpc:shutdown-listener-kind-grpc-events
        }
    }
}

async fn drain_listener(
    listener: Option<Server>,
    graceful_deadline: Instant,
    confirmation_deadline: Instant,
    kind: ListenerKind,
) -> bool {
    let Some(listener) = listener else {
        return false;
    };
    let events = kind.drain_events();
    let (drained, residue) = listener
        .drain_until(graceful_deadline, confirmation_deadline)
        .await;
    match drained {
        Ok(Drained::Complete) => {
            tracing::info!("{}", events.completed);
            false
        }
        Ok(Drained::TimedOut {
            remaining_connections,
        }) => {
            tracing::warn!(
                remaining = remaining_connections,
                accept_unconfirmed = residue.accept_unconfirmed,
                remaining_connections = residue.remaining_connections,
                remaining_delegated_tasks = residue.remaining_delegated_tasks,
                reason = "in_flight_requests_outlived_drain_budget",
                "{}",
                events.forced
            );
            true
        }
        Err(error) => {
            tracing::error!(
                %error,
                accept_unconfirmed = residue.accept_unconfirmed,
                remaining_connections = residue.remaining_connections,
                remaining_delegated_tasks = residue.remaining_delegated_tasks,
                "{}", events.failed
            );
            true
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
        let refused = service_config::fixtures::service(
            &[("APP__HTTP__GRACE_PERIOD", "43.499999999s")],
            super::super::BUILD_INFO,
            TAIL,
        );
        std::assert_matches!(
            refused,
            Err(service_config::Error::Validate(errors)) if errors.names("http.grace_period")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stages_reserve_runtime_and_never_renew_the_deadline() {
        let budget = Budget::until(Instant::now() + Duration::from_secs(10));
        assert_eq!(
            budget.remaining(Duration::from_secs(20)),
            Duration::from_secs(9)
        );
        tokio::time::advance(Duration::from_secs(8)).await;
        assert_eq!(
            budget.remaining(Duration::from_secs(4)),
            Duration::from_secs(1)
        );
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(budget.remaining(Duration::from_secs(4)), Duration::ZERO);
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
        let http_config = &config.http;
        let mut signals = Signals::install().unwrap();
        let outcome = run(Plan {
            http_config,
            signals: &mut signals,
            deadline: Instant::now() + http_config.grace_period,
            serving: Serving::default(),
            background,
            dependencies: Dependencies::default(),
            tracer_provider: None,
            logger: None,
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
        let http_config = &config.http;
        let mut signals = Signals::install().unwrap();
        assert_eq!(
            run(Plan {
                http_config,
                signals: &mut signals,
                deadline: Instant::now() + http_config.grace_period,
                serving: Serving::default(),
                background,
                dependencies: Dependencies::default(),
                tracer_provider: None,
                logger: None,
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
        pool: PgPool,
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
        let pool = PgPool::connect_lazy("postgres://localhost/unused").unwrap();
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
        let mut http_config = super::super::test_config(&[]).http.clone();
        http_config.drain_timeout = Duration::from_millis(30);
        let mut signals = Signals::install().unwrap();
        let shutdown = run(Plan {
            http_config: &http_config,
            signals: &mut signals,
            deadline: Instant::now() + http_config.grace_period,
            serving: Serving {
                app_listener: Some(server),
                ..Serving::default()
            },
            background: Background::new(CancellationToken::new()),
            dependencies: Dependencies {
                postgres: Some(pool.clone()),
                ..Dependencies::default()
            },
            tracer_provider: None,
            logger: None,
        });
        let check_order = async {
            tokio::time::timeout(Duration::from_secs(2), dropping.cancelled())
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(30), pool.close_event())
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
