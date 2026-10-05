//! Ordered teardown under one retained process deadline.

use std::{future::Future, panic::AssertUnwindSafe, time::Duration};

use futures_util::FutureExt;
use health::Readiness;
use infra_http::{Drained, Server};
// template:begin cache:service-shutdown-cache-imports
use infra_cache::Cache;
// template:end cache:service-shutdown-cache-imports
// template:begin object-storage:service-shutdown-object-storage-imports
use infra_object_storage::ObjectStorage;
// template:end object-storage:service-shutdown-object-storage-imports
// template:begin postgres:shutdown-imports
use infra_postgres::{Closed, PgPool};
// template:end postgres:shutdown-imports
use infra_telemetry::{ProviderShutdown, SHUTDOWN_JOIN_SLACK, TracerProviderHandle};
use service_config::HttpConfig;
use tokio::{sync::watch, task::JoinSet, time::Instant};
use tokio_util::sync::CancellationToken;

const DIAGNOSTICS_SHUTDOWN: Duration = Duration::from_secs(2);
const BACKGROUND_JOIN: Duration = Duration::from_secs(5);
pub(crate) const DEPENDENCY_CLOSE: Duration = Duration::from_secs(5);
const TELEMETRY_FLUSH: Duration = Duration::from_secs(5);
pub(crate) const SHUTDOWN_TAIL: Duration = DIAGNOSTICS_SHUTDOWN
    .saturating_add(BACKGROUND_JOIN)
    .saturating_add(DEPENDENCY_CLOSE)
    .saturating_add(TELEMETRY_FLUSH)
    .saturating_add(SHUTDOWN_JOIN_SLACK)
    .saturating_add(super::RUNTIME_SHUTDOWN_TIMEOUT);

#[derive(Debug, thiserror::Error)]
#[error(
    "http.grace_period ({grace:?}) must be >= http.drain_timeout ({drain_timeout:?}) plus the {tail:?} teardown tail (diagnostics, background join, dependency close, telemetry flush, SDK join, runtime)"
)]
pub(crate) struct GraceBudgetError {
    grace: Duration,
    drain_timeout: Duration,
    tail: Duration,
}

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

/// Native streams stay owned through the runtime's final shutdown.
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
                terminate,
                interrupt,
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

    pub(crate) fn first_stop(&self) -> Option<Instant> {
        self.first_stop
    }

    pub(crate) async fn wait(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        let (received, name) = tokio::select! {
            received = self.terminate.recv() => (received, "SIGTERM"),
            received = self.interrupt.recv() => (received, "SIGINT"),
        };
        #[cfg(windows)]
        let (received, name) = (self.ctrl_c.recv().await, "ctrl-c");
        if received.is_none() {
            return Err(std::io::Error::other("stop signal stream closed"));
        }
        self.first_stop.get_or_insert_with(Instant::now);
        tracing::info!(signal = name, "stop requested");
        Ok(())
    }

    pub(crate) fn pending(&mut self) -> std::io::Result<bool> {
        self.wait()
            .now_or_never()
            .transpose()
            .map(|value| value.is_some())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackgroundFailure {
    pub(crate) name: &'static str,
    pub(crate) panicked: bool,
}

struct Completion {
    name: &'static str,
    cancel: CancellationToken,
    failure: watch::Sender<Option<BackgroundFailure>>,
}

impl Drop for Completion {
    fn drop(&mut self) {
        let panicked = std::thread::panicking();
        if panicked || !self.cancel.is_cancelled() {
            tracing::error!(task = self.name, panicked, "background_task_stopped");
            self.failure.send_if_modified(|observed| {
                if observed.is_none() {
                    *observed = Some(BackgroundFailure {
                        name: self.name,
                        panicked,
                    });
                    true
                } else {
                    false
                }
            });
        }
    }
}

/// Private composition owner; dependencies must outlive every registered future.
pub(crate) struct Background {
    tasks: JoinSet<()>,
    cancel: CancellationToken,
    failure: watch::Sender<Option<BackgroundFailure>>,
}

impl Background {
    pub(crate) fn new(cancel: CancellationToken) -> Self {
        let (failure, _) = watch::channel(None);
        Self {
            tasks: JoinSet::new(),
            cancel,
            failure,
        }
    }

    pub(crate) fn spawn(
        &mut self,
        name: &'static str,
        future: impl Future<Output = ()> + Send + 'static,
    ) {
        let guard = Completion {
            name,
            cancel: self.cancel.clone(),
            failure: self.failure.clone(),
        };
        self.tasks.spawn(async move {
            let _guard = guard;
            future.await;
        });
    }

    pub(crate) fn observer(&self) -> watch::Receiver<Option<BackgroundFailure>> {
        self.failure.subscribe()
    }

    pub(crate) fn failed(&self) -> Option<BackgroundFailure> {
        *self.failure.borrow()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub(crate) fn watch_listener(&mut self, name: &'static str, listener: &Server) {
        let failure = listener.failure();
        let cancel = self.cancel.child_token();
        self.spawn(name, async move {
            tokio::select! {
                biased;
                failure = failure => tracing::error!(listener = name, ?failure, "listener failed"),
                () = cancel.cancelled() => {},
            }
        });
    }

    async fn join_until(&mut self, deadline: Instant) -> JoinOutcome {
        let mut failed = self.failed().is_some();
        loop {
            match tokio::time::timeout_at(deadline, self.tasks.join_next()).await {
                Ok(Some(Ok(()))) => {}
                Ok(Some(Err(error))) => {
                    failed = true;
                    tracing::warn!(
                        panicked = error.is_panic(),
                        cancelled = error.is_cancelled(),
                        "background_join_failed"
                    );
                }
                Ok(None) => {
                    return if failed || self.failed().is_some() {
                        JoinOutcome::Failed
                    } else {
                        JoinOutcome::Complete
                    };
                }
                Err(_) => return JoinOutcome::Unconfirmed,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JoinOutcome {
    Complete,
    Failed,
    Unconfirmed,
}

pub(crate) async fn background_failure(
    receiver: &mut watch::Receiver<Option<BackgroundFailure>>,
) -> BackgroundFailure {
    loop {
        if let Some(failure) = *receiver.borrow_and_update() {
            return failure;
        }
        if receiver.changed().await.is_err() {
            return BackgroundFailure {
                name: "background_owner",
                panicked: false,
            };
        }
    }
}
/// Dependencies startup opened. Teardown closes them after the background
/// tasks that use them have joined, on every exit path.
#[derive(Default)]
pub(crate) struct Dependencies {
    // template:begin postgres:shutdown-dependencies-postgres-field
    /// HTTP connection tasks are not in the background set; `close` waits for any
    /// pooled connections they still hold. Work that outlives close is
    /// dropped by `runtime.shutdown_timeout`.
    pub(crate) postgres: Option<PgPool>,
    // template:end postgres:shutdown-dependencies-postgres-field
    // template:begin cache:service-shutdown-dependencies-cache-field
    /// Not a readiness probe; the connection closes when it drops.
    pub(crate) cache: Option<Cache>,
    // template:end cache:service-shutdown-dependencies-cache-field
    // template:begin object-storage:service-shutdown-dependencies-object-storage-field
    /// Not a readiness probe; idle connections close when it drops.
    pub(crate) object_storage: Option<ObjectStorage>,
    // template:end object-storage:service-shutdown-dependencies-object-storage-field
}

impl Dependencies {
    /// Close every opened dependency. Returns whether one
    /// outlived `deadline`.
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
        // template:begin cache:service-shutdown-dependencies-cache-close
        drop(cache);
        // template:end cache:service-shutdown-dependencies-cache-close
        // template:begin object-storage:service-shutdown-dependencies-object-storage-close
        drop(object_storage);
        // template:end object-storage:service-shutdown-dependencies-object-storage-close
        async move {
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
        }
    }
}

/// Resources become visible to cleanup immediately, before the next await.
#[derive(Default)]
pub(crate) struct Serving {
    pub(crate) readiness: Option<Readiness>,
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
}

/// Catch one stage's unwind without spending a new budget or skipping later owners.
async fn stage(name: &'static str, future: impl Future<Output = bool>) -> bool {
    match AssertUnwindSafe(future).catch_unwind().await {
        Ok(degraded) => degraded,
        Err(_) => {
            tracing::error!(
                stage = name,
                outcome = "unconfirmed",
                "shutdown_stage_panicked"
            );
            true
        }
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
    } = plan;
    let budget = Budget::until(deadline);
    let mut degraded = stage("listeners", async {
        tracing::info!(grace = ?http_config.grace_period, "shutdown_started");
        stop_serving(&mut serving, http_config, signals, &budget).await
    })
    .await;
    degraded |= stage("diagnostics", async {
        if let Some(listener) = serving.diagnostics.take() {
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
        }
        false
    })
    .await;
    // Any listener not taken because its stage unwound still requests cleanup on drop.
    drop(serving);

    background.cancel.cancel();
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
                background.tasks.abort_all();
                tracing::warn!("background_abort_requested");
                true
            }
        }
    })
    .await;
    // Forced acknowledgement and dependency close share this allocation.
    let dependency_deadline = budget.stage_deadline(DEPENDENCY_CLOSE);
    degraded |= stage("background_forced", async {
        if background.tasks.is_empty() {
            return false;
        }
        background.tasks.abort_all();
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
    degraded |= stage("telemetry", async {
        let Some(provider) = tracer_provider else {
            return false;
        };
        match provider
            .shutdown_until(
                TELEMETRY_FLUSH,
                budget.stage_deadline(TELEMETRY_FLUSH.saturating_add(SHUTDOWN_JOIN_SLACK)),
            )
            .await
        {
            ProviderShutdown::Flushed => {
                tracing::info!("telemetry_flushed");
                false
            }
            ProviderShutdown::Incomplete => {
                tracing::warn!("telemetry_incomplete");
                true
            }
        }
    })
    .await;
    if let Err(error) = signals.pending() {
        degraded = true;
        tracing::error!(%error, "signal_owner_failed");
    }
    let outcome = if degraded {
        Outcome::Degraded
    } else {
        Outcome::Graceful
    };
    tracing::info!(?outcome, "shutdown_completed");
    outcome
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
    let drain_budget = wanted.min(drain_deadline.saturating_duration_since(Instant::now()));
    tracing::info!(budget = ?drain_budget, "drain_started");
    let http_drain = drain_listener(serving.app_listener.take(), drain_budget, "http");
    // template:begin grpc:shutdown-concurrent-grpc-drain
    let http_drain = async {
        let (http_failed, grpc_failed) = tokio::join!(
            http_drain,
            drain_listener(serving.grpc_listener.take(), drain_budget, "grpc")
        );
        http_failed || grpc_failed
    };
    // template:end grpc:shutdown-concurrent-grpc-drain
    degraded | http_drain.await
}

async fn drain_listener(listener: Option<Server>, budget: Duration, name: &'static str) -> bool {
    let Some(listener) = listener else {
        return false;
    };
    match listener.drain(budget).await {
        Ok(Drained::Complete) => {
            if name == "grpc" {
                tracing::info!("grpc_drain_completed");
            } else {
                tracing::info!("drain_completed");
            }
            false
        }
        Ok(Drained::TimedOut {
            remaining_connections,
        }) => {
            if name == "grpc" {
                tracing::warn!(
                    remaining = remaining_connections,
                    reason = "in_flight_requests_outlived_drain_budget",
                    "grpc_drain_forced"
                );
            } else {
                tracing::warn!(
                    remaining = remaining_connections,
                    reason = "in_flight_requests_outlived_drain_budget",
                    "shutdown_forced"
                );
            }
            true
        }
        Err(error) => {
            if name == "grpc" {
                tracing::error!(%error, "grpc_drain_failed");
            } else {
                tracing::error!(%error, "drain_failed");
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grace_covers_the_whole_tail_including_equality() {
        let mut http = HttpConfig::default();
        validate_grace_budget(&http).unwrap();
        http.grace_period = Duration::from_millis(43_500);
        validate_grace_budget(&http).unwrap();
        http.grace_period -= Duration::from_nanos(1);
        assert!(validate_grace_budget(&http).is_err());
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

    #[tokio::test]
    async fn task_failure_is_sticky_before_join() {
        let cancel = CancellationToken::new();
        let mut background = Background::new(cancel.clone());
        let mut observed = background.observer();
        background.spawn("early", async {});
        let failure =
            tokio::time::timeout(Duration::from_secs(1), background_failure(&mut observed))
                .await
                .unwrap();
        assert_eq!(
            failure,
            BackgroundFailure {
                name: "early",
                panicked: false
            }
        );
        cancel.cancel();
        assert_eq!(
            background
                .join_until(Instant::now() + Duration::from_secs(1))
                .await,
            JoinOutcome::Failed
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ignored_cancellation_is_aborted_and_acknowledged_before_shutdown_returns() {
        let cancel = CancellationToken::new();
        let mut background = Background::new(cancel.clone());
        let (started, running) = tokio::sync::oneshot::channel();
        let (dropped, mut completed) = tokio::sync::oneshot::channel();
        background.spawn("ignores_cancel", async move {
            struct OnDrop(Option<tokio::sync::oneshot::Sender<()>>);
            impl Drop for OnDrop {
                fn drop(&mut self) {
                    let _ = self.0.take().unwrap().send(());
                }
            }
            let _guard = OnDrop(Some(dropped));
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        running.await.unwrap();
        let http_config = HttpConfig::default();
        let mut signals = Signals::install().unwrap();
        let outcome = run(Plan {
            http_config: &http_config,
            signals: &mut signals,
            deadline: Instant::now() + http_config.grace_period,
            serving: Serving::default(),
            background,
            dependencies: Dependencies::default(),
            tracer_provider: None,
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
        let http_config = HttpConfig::default();
        let mut signals = Signals::install().unwrap();
        assert_eq!(
            run(Plan {
                http_config: &http_config,
                signals: &mut signals,
                deadline: Instant::now() + http_config.grace_period,
                serving: Serving::default(),
                background,
                dependencies: Dependencies::default(),
                tracer_provider: None,
            })
            .await,
            Outcome::Degraded
        );
    }
}
