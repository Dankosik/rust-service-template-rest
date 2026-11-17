// template:begin jobs:worker-bootstrap-jobs-module
mod jobs;
// template:end jobs:worker-bootstrap-jobs-module
mod listeners;
// template:begin messaging:worker-bootstrap-messaging-module
mod messaging;
// template:end messaging:worker-bootstrap-messaging-module
// template:begin outbox:worker-bootstrap-outbox-module
mod outbox;
// template:end outbox:worker-bootstrap-outbox-module
// template:begin jobs:worker-bootstrap-postgres-module
pub(crate) mod postgres;
// template:end jobs:worker-bootstrap-postgres-module
mod telemetry;

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt;
use tokio::time::Instant;

use health::{PreparedReadiness, Readiness};
use infra_telemetry::install_panic_hook;
use service_config::WorkerConfig;

use crate::background::Background;
use crate::resources::{Listeners, Resources};
use crate::shutdown;
use crate::signals::{SignalError, Signals};
use crate::{BuildError, Register, Registration};

const METRICS_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkerError {
    #[error(
        "no job kind or typed message handler is registered: register this service's retained capabilities in crates/jobs-worker/src/main.rs"
    )]
    NoRegistrations,
    #[error(transparent)]
    Load(#[from] service_config::Error),
    #[error("configuration is invalid: {0}")]
    Config(#[from] service_config::ValidationError),
    #[error("build tokio runtime: {0}")]
    Runtime(#[source] std::io::Error),
    #[error("install stop signal handlers: {0}")]
    Signals(#[source] std::io::Error),
    #[error(transparent)]
    SignalClosed(#[from] SignalError),
    #[error("worker lifecycle panicked; payload withheld")]
    Panicked,
    #[error("listener {0} stopped without a stop signal")]
    ListenerStopped(&'static str),
    #[error(transparent)]
    Tracing(#[from] infra_telemetry::TracingError),
    #[error(transparent)]
    Logging(#[from] infra_telemetry::LoggingError),
    #[error(transparent)]
    Metrics(#[from] infra_telemetry::MetricsError),
    #[error("worker registration failed: {0}")]
    Registration(#[source] BuildError),
    #[error("background task {0} stopped without a stop signal")]
    BackgroundStopped(&'static str),
    // template:begin jobs:worker-bootstrap-job-errors
    #[error("postgres.enabled must be true to run the jobs worker")]
    PostgresDisabled,
    #[error("job kinds are invalid: {0}")]
    Kinds(#[from] infra_jobs::KindError),
    #[error("the job engine stopped without a stop signal")]
    EngineStopped,
    #[error("configuration is invalid: postgres.dsn: {0}")]
    PostgresDsn(#[from] infra_postgres::DsnError),
    #[error(transparent)]
    Postgres(#[from] infra_postgres::ConnectError),
    #[error("postgres migration history: {0}")]
    PostgresHistory(#[from] migrate::HistoryError),
    #[error("jobs startup check: {0}")]
    JobsStartup(#[from] infra_jobs::StartupError),
    // template:end jobs:worker-bootstrap-job-errors
    // template:begin outbox:worker-bootstrap-outbox-kind-error
    #[error("ordinary jobs cannot register the reserved outbox publication kind")]
    PublisherKindConflict,
    // template:end outbox:worker-bootstrap-outbox-kind-error
    // template:begin messaging:worker-bootstrap-messaging-errors
    #[error("typed message handlers are invalid: {0}")]
    MessagingRegistry(#[from] infra_messaging::RegistryError),
    #[error("messaging startup: {0}")]
    Messaging(#[from] infra_messaging::MessagingError),
    #[error("messaging.consumer_concurrency cannot fit this platform")]
    MessagingConcurrency,
    #[error("messaging.max_payload_bytes cannot fit this platform")]
    MessagingPayloadBound,
    #[error("the messaging consumer stopped without a stop signal: {0}")]
    ConsumerStopped(#[source] infra_messaging::ConsumerError),
    // template:end messaging:worker-bootstrap-messaging-errors
    #[error(transparent)]
    Server(#[from] infra_http_transport::ServerError),
    #[error(transparent)]
    HttpContract(#[from] infra_http::FinalizeError),
    #[error("startup admission: {0}")]
    Admission(health::NotReady),
}

#[derive(Debug)]
enum Ended {
    Signal,
    Failure(WorkerError),
}

struct Prepared {
    readiness: PreparedReadiness,
    // template:begin jobs:worker-bootstrap-prepared-jobs
    engine: Option<infra_jobs::Engine>,
    // template:end jobs:worker-bootstrap-prepared-jobs
    // template:begin messaging:worker-bootstrap-prepared-consumer
    consumer: Option<infra_messaging::Consumer>,
    // template:end messaging:worker-bootstrap-prepared-consumer
}

pub(crate) async fn serve(
    config: WorkerConfig,
    register: Register<'_>,
    signals: &mut Signals,
    deadline: &mut Option<Instant>,
) -> Result<shutdown::Outcome, WorkerError> {
    let background = Background::new();
    let mut resources = Resources::default();
    let ended = Box::pin(
        AssertUnwindSafe(run_worker(
            &config,
            register,
            signals,
            &background,
            &mut resources,
        ))
        .catch_unwind(),
    )
    .await
    .unwrap_or(Ended::Failure(WorkerError::Panicked));
    let ended = match ended {
        Ended::Failure(_) => ended,
        Ended::Signal => pending_failure(&resources, &background).map_or(ended, Ended::Failure),
    };
    let deadline = *deadline.insert(shutdown::stop_deadline(
        signals.first_stop(),
        config.http.grace_period,
    ));
    if let Ended::Failure(error) = &ended {
        if resources.logger.is_some() {
            #[allow(
                unused_variables,
                reason = "retained messaging supplies the failure kind"
            )]
            let failure_kind: Option<&str> = None;
            // template:begin messaging:worker-bootstrap-log-consumer-failure
            let failure_kind =
                matches!(error, WorkerError::ConsumerStopped(_)).then_some("consumer_stopped");
            // template:end messaging:worker-bootstrap-log-consumer-failure
            tracing::error!(failure.kind = failure_kind, error = %error, "jobs worker failed");
        } else {
            crate::report_failure(error);
        }
    }
    let outcome = shutdown::run(shutdown::Plan {
        http: &config.http,
        resources,
        background,
        signals,
        deadline,
    })
    .await;
    match ended {
        Ended::Signal => Ok(outcome),
        Ended::Failure(error) => Err(error),
    }
}

async fn run_worker(
    config: &WorkerConfig,
    register: Register<'_>,
    signals: &mut Signals,
    background: &Background,
    resources: &mut Resources,
) -> Ended {
    let prepared = {
        let admission = prepare(config, register, background, resources);
        tokio::pin!(admission);
        tokio::select! {
            biased;
            result = signals.wait() => Err(signal_ended(result, background)),
            task = background.wait_for_failure() => Err(Ended::Failure(WorkerError::BackgroundStopped(task))),
            result = &mut admission => result.map_err(Ended::Failure),
        }
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(ended) => return pending_failure(resources, background).map_or(ended, Ended::Failure),
    };
    if let Some(ended) = pending_end(resources, background, signals) {
        return ended;
    }
    // template:begin jobs:worker-bootstrap-start-admitted-jobs
    if let Some(ended) = jobs::start_engine(prepared.engine, resources, background, signals) {
        return ended;
    }
    // template:end jobs:worker-bootstrap-start-admitted-jobs
    // template:begin messaging:worker-bootstrap-start-admitted-consumer
    if let Some(ended) =
        messaging::start_consumer(prepared.consumer, resources, background, signals)
    {
        return ended;
    }
    // template:end messaging:worker-bootstrap-start-admitted-consumer
    if let Err(ended) = activate_readiness(prepared.readiness, resources, background, signals).await
    {
        return ended;
    }
    tracing::info!("jobs_worker_ready");
    wait_for_stop(resources, background, signals).await
}

async fn activate_readiness(
    mut readiness: PreparedReadiness,
    resources: &Resources,
    background: &Background,
    signals: &mut Signals,
) -> Result<(), Ended> {
    let fresh = tokio::select! {
        biased;
        ended = wait_for_stop(resources, background, signals) => return Err(ended),
        result = readiness.ensure_fresh() => result,
    };
    if let Some(ended) = pending_end(resources, background, signals) {
        return Err(ended);
    }
    let refresher = fresh
        .and_then(|()| readiness.activate())
        .map_err(|error| Ended::Failure(WorkerError::Admission(error)))?;
    background.spawn("readiness_refresher", |cancel| {
        refresher.refresh_until(cancel)
    });
    Ok(())
}

fn signal_ended(result: Result<(), SignalError>, background: &Background) -> Ended {
    if let Some(task) = background.failure() {
        return Ended::Failure(WorkerError::BackgroundStopped(task));
    }
    match result {
        Ok(()) => Ended::Signal,
        Err(error) => Ended::Failure(error.into()),
    }
}

fn pending_end(
    resources: &Resources,
    background: &Background,
    signals: &mut Signals,
) -> Option<Ended> {
    if let Some(error) = pending_failure(resources, background) {
        return Some(Ended::Failure(error));
    }
    match signals.take_pending() {
        Ok(false) => None,
        Ok(true) => Some(Ended::Signal),
        Err(error) => Some(Ended::Failure(error.into())),
    }
}

fn pending_failure(resources: &Resources, background: &Background) -> Option<WorkerError> {
    if let Some(task) = background.failure() {
        return Some(WorkerError::BackgroundStopped(task));
    }
    let Listeners {
        health,
        diagnostics,
    } = &resources.listeners;
    for (name, server) in [
        (Listeners::HEALTH, health.as_ref()),
        (Listeners::DIAGNOSTICS, diagnostics.as_ref()),
    ] {
        if let Some(server) = server
            && server.accept_failure().now_or_never().is_some()
        {
            return Some(WorkerError::ListenerStopped(name));
        }
    }
    // template:begin jobs:worker-bootstrap-pending-jobs
    if let Some(error) = jobs::engine_failure(resources) {
        return Some(error);
    }
    // template:end jobs:worker-bootstrap-pending-jobs
    // template:begin messaging:worker-bootstrap-pending-consumer
    if let Some(error) = messaging::consumer_failure(resources) {
        return Some(error);
    }
    // template:end messaging:worker-bootstrap-pending-consumer
    None
}

async fn prepare(
    config: &WorkerConfig,
    register: Register<'_>,
    background: &Background,
    resources: &mut Resources,
) -> Result<Prepared, WorkerError> {
    let identity = worker_identity(&config.observability.otel.service_name);
    let (metrics, exporter_state) = telemetry::install_observability(config, &identity, resources)?;
    let connection_caps = config.finite_listener_connection_caps();
    match rlimit::increase_nofile_limit(u64::MAX) {
        Err(error) => tracing::warn!(error = %error, "open_file_limit_unchanged"),
        Ok(open_file_limit) if open_file_limit < connection_caps => {
            tracing::warn!(
                open_file_limit,
                connection_caps,
                "open_file_limit_below_connection_caps"
            );
        }
        Ok(_) => {}
    }
    install_panic_hook();
    let mut capabilities = register_capabilities(config, register, background)?;
    telemetry::log_startup_record(
        config,
        &identity,
        // template:begin jobs:worker-bootstrap-log-jobs-argument
        capabilities.jobs.as_ref(),
        // template:end jobs:worker-bootstrap-log-jobs-argument
        &exporter_state,
    );
    telemetry::spawn_metrics_tasks(&metrics, background);
    // template:begin jobs:worker-bootstrap-pool-config
    let postgres = postgres::pool_config(config, capabilities.jobs.is_some())?;
    // template:end jobs:worker-bootstrap-pool-config
    // template:begin outbox:worker-bootstrap-outbox-pool-inputs
    if capabilities.messages.is_none() {
        capabilities.messages = Some(messaging::MessagingPlan::producer(config)?);
    }
    // template:end outbox:worker-bootstrap-outbox-pool-inputs
    // template:begin jobs:worker-bootstrap-admit-pool-call
    if let Some(postgres) = postgres {
        postgres::admit_pool(config, postgres, background, resources).await?;
    }
    // template:end jobs:worker-bootstrap-admit-pool-call
    // template:begin messaging:worker-bootstrap-messaging-connect
    let consumer = match capabilities.messages.take() {
        Some(messages) => {
            Box::pin(messaging::connect_messaging(
                messages,
                &background.cancel,
                resources,
            ))
            .await?
        }
        None => None,
    };
    // template:end messaging:worker-bootstrap-messaging-connect
    // template:begin jobs:worker-bootstrap-build-engines
    let engine = jobs::build_engine(config, capabilities.jobs.take(), resources).await?;
    // template:end jobs:worker-bootstrap-build-engines
    #[allow(unused_mut, reason = "the messaging profile adds the durable probe")]
    let mut probes = listeners::readiness_probes(resources);
    // template:begin messaging:worker-bootstrap-durable-readiness
    if let Some(consumer) = &consumer {
        probes.push(Box::new(consumer.probe()));
    }
    // template:end messaging:worker-bootstrap-durable-readiness
    Ok(Prepared {
        readiness: prepare_readiness(config, &metrics, background, resources, probes).await?,
        // template:begin jobs:worker-bootstrap-prepared-jobs-value
        engine,
        // template:end jobs:worker-bootstrap-prepared-jobs-value
        // template:begin messaging:worker-bootstrap-prepared-consumer-value
        consumer,
        // template:end messaging:worker-bootstrap-prepared-consumer-value
    })
}

async fn prepare_readiness(
    config: &WorkerConfig,
    metrics: &infra_telemetry::Metrics,
    background: &Background,
    resources: &mut Resources,
    probes: Vec<Box<dyn health::Probe>>,
) -> Result<PreparedReadiness, WorkerError> {
    let readiness = Readiness::new(probes, listeners::refresh_policy(config)?);
    let reader = resources.readiness.insert(readiness.control()).reader();
    listeners::bind_listeners(config, metrics, reader, background, resources)?;
    readiness.prepare().await.map_err(WorkerError::Admission)
}

struct RetainedCapabilities {
    // template:begin jobs:worker-bootstrap-registrations-jobs
    jobs: Option<infra_jobs::Kinds>,
    // template:end jobs:worker-bootstrap-registrations-jobs
    // template:begin messaging:worker-bootstrap-registrations-messaging
    messages: Option<messaging::MessagingPlan>,
    // template:end messaging:worker-bootstrap-registrations-messaging
}

fn register_capabilities(
    config: &WorkerConfig,
    register: Register<'_>,
    background: &Background,
) -> Result<RetainedCapabilities, WorkerError> {
    let mut registration = Registration::new(config, background);
    register(&mut registration).map_err(WorkerError::Registration)?;
    #[allow(
        unused_variables,
        reason = "retained jobs shadow the inert registration"
    )]
    let has_jobs = false;
    #[allow(
        unused_variables,
        reason = "retained messaging shadows the inert registration"
    )]
    let has_messages = false;
    // template:begin jobs:worker-bootstrap-validate-jobs
    let jobs = jobs::validate(std::mem::take(&mut registration.jobs))?;
    #[allow(
        unused_variables,
        reason = "retained outbox adds its own publication kind"
    )]
    let has_jobs = jobs.is_some();
    // template:end jobs:worker-bootstrap-validate-jobs
    // template:begin messaging:worker-bootstrap-retain-messaging
    let messages = messaging::MessagingPlan::retain(config, registration)?;
    let has_messages = messages.is_some();
    // template:end messaging:worker-bootstrap-retain-messaging
    // template:begin outbox:worker-bootstrap-outbox-registration
    let has_jobs = true;
    // template:end outbox:worker-bootstrap-outbox-registration
    if !has_jobs && !has_messages {
        return Err(WorkerError::NoRegistrations);
    }
    Ok(RetainedCapabilities {
        // template:begin jobs:worker-bootstrap-registrations-jobs-value
        jobs,
        // template:end jobs:worker-bootstrap-registrations-jobs-value
        // template:begin messaging:worker-bootstrap-registrations-messaging-value
        messages,
        // template:end messaging:worker-bootstrap-registrations-messaging-value
    })
}

async fn wait_for_stop(
    resources: &Resources,
    background: &Background,
    signals: &mut Signals,
) -> Ended {
    tokio::select! {
        biased;
        result = signals.wait() => {
            pending_failure(resources, background)
                .map_or_else(|| signal_ended(result, background), Ended::Failure)
        },
        task = background.wait_for_failure() => Ended::Failure(WorkerError::BackgroundStopped(task)),
        // template:begin jobs:worker-bootstrap-wait-jobs-failure
        () = jobs::engine_stopped(resources) => Ended::Failure(WorkerError::EngineStopped),
        // template:end jobs:worker-bootstrap-wait-jobs-failure
        // template:begin messaging:worker-bootstrap-wait-messaging-failure
        error = messaging::consumer_stopped(resources) => Ended::Failure(WorkerError::ConsumerStopped(error)),
        // template:end messaging:worker-bootstrap-wait-messaging-failure
    }
}

const IDENTITY_SUFFIX: &str = "-jobs-worker";

fn worker_identity(service_name: &str) -> String {
    format!("{service_name}{IDENTITY_SUFFIX}")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::WorkerError;

    #[cfg(unix)]
    async fn prepare_worker_and_check_http(
        background: &super::Background,
        resources: &mut super::Resources,
    ) -> super::PreparedReadiness {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

        let config = service_config::fixtures::worker(
            &[
                ("APP__HTTP__ADDR", "127.0.0.1:0"),
                ("APP__OBSERVABILITY__METRICS__ADDR", ""),
            ],
            crate::BUILD_INFO,
            crate::shutdown::TAIL,
        )
        .unwrap();
        let metrics = infra_telemetry::Metrics::install(&[]).unwrap();
        let prepared =
            super::prepare_readiness(&config, &metrics, background, resources, Vec::new())
                .await
                .unwrap();
        let address = resources.listeners.health.as_ref().unwrap().local_addr();
        let response = tokio::time::timeout(Duration::from_secs(2), async {
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            socket
                .write_all(
                    b"GET /health/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            BufReader::new(socket)
                .lines()
                .next_line()
                .await
                .unwrap()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(response.starts_with("HTTP/1.1 503 "), "{response}");
        prepared
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        clippy::disallowed_types,
        reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
    )]
    fn first_stop_preserves_a_queued_second_stop_for_drain() {
        const CHILD: &str = "WORKER_LIFECYCLE_SIGNAL_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let thread = std::thread::current();
            let mut child = std::mem::DropGuard::new(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", thread.name().unwrap()])
                    .env(CHILD, "1")
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
                |mut child| {
                    let _ = child.kill();
                    let _ = child.wait();
                },
            );
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while child.try_wait().unwrap().is_none() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "isolated signal case did not finish"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = std::mem::DropGuard::dismiss(child)
                .wait_with_output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                use futures_util::FutureExt;
                use tokio::signal::unix::{SignalKind, signal};

                let mut signals = super::Signals::install().unwrap();
                let mut terminate_seen = signal(SignalKind::terminate()).unwrap();
                let mut interrupt_seen = signal(SignalKind::interrupt()).unwrap();
                let background = super::Background::new();
                let mut resources = super::Resources::default();
                let prepared = prepare_worker_and_check_http(&background, &mut resources).await;
                let process = std::process::id().to_string();
                for signal in ["-TERM", "-INT"] {
                    assert!(
                        std::process::Command::new("kill")
                            .args([signal, &process])
                            .status()
                            .unwrap()
                            .success()
                    );
                }
                tokio::time::timeout(Duration::from_secs(2), async {
                    assert_eq!(terminate_seen.recv().await, Some(()));
                    assert_eq!(interrupt_seen.recv().await, Some(()));
                })
                .await
                .unwrap();
                let ended = tokio::time::timeout(
                    Duration::from_secs(2),
                    super::activate_readiness(prepared, &resources, &background, &mut signals),
                )
                .await
                .unwrap();
                std::assert_matches!(ended, Err(super::Ended::Signal));
                assert!(
                    !resources.readiness.as_ref().unwrap().reader().is_ready(),
                    "a pending stop must withhold worker readiness"
                );
                let first_stop = signals.first_stop();
                assert!(first_stop.is_some());
                std::assert_matches!(
                    signals.wait().now_or_never(),
                    Some(Ok(())),
                    "the queued second stop must remain available to expedite drain"
                );
                assert_eq!(
                    signals.first_stop(),
                    first_stop,
                    "the second stop must not restart the deadline"
                );
                background.cancel.cancel();
                background.tracker.close();
                background.tracker.wait().await;
                resources
                    .listeners
                    .health
                    .unwrap()
                    .drain(Duration::from_secs(2))
                    .await
                    .unwrap();
            });
    }

    #[tokio::test]
    async fn registration_unwind_cancels_and_joins_already_registered_work() {
        use futures_util::FutureExt;

        let config =
            service_config::fixtures::worker(&[], crate::BUILD_INFO, crate::shutdown::TAIL)
                .unwrap();
        let mut signals = super::Signals::install().unwrap();
        let mut deadline = None;
        let (finished, observed_finish) = tokio::sync::oneshot::channel();
        let registration: crate::Register<'_> = Box::new(|registration| {
            registration.spawn("registered_before_unwind", |cancel| async move {
                cancel.cancelled().await;
                let _ = finished.send(());
            });
            panic!("registration payload must stay withheld");
        });
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            Box::pin(super::serve(
                config,
                registration,
                &mut signals,
                &mut deadline,
            )),
        )
        .await
        .unwrap();
        std::assert_matches!(result, Err(WorkerError::Panicked), "{result:?}");
        assert_eq!(observed_finish.now_or_never(), Some(Ok(())));
        assert!(
            deadline.is_some(),
            "unwind must establish the bounded cleanup deadline"
        );
    }

    #[test]
    fn refusal_texts_name_the_failed_check() {
        assert_eq!(
            WorkerError::NoRegistrations.to_string(),
            "no job kind or typed message handler is registered: register this service's retained capabilities in crates/jobs-worker/src/main.rs"
        );

        let refused = service_config::fixtures::worker(
            &[("APP__HTTP__GRACE_PERIOD", "30s")],
            crate::BUILD_INFO,
            crate::shutdown::TAIL,
        )
        .unwrap_err();
        assert_eq!(
            WorkerError::from(refused).to_string(),
            "configuration is invalid: http.grace_period: (30s) must be >= http.drain_timeout (25s) plus the 18.5s jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush, SDK join slack, runtime shutdown)"
        );
    }
}
