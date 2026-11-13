//! Asynchronous startup after the runtime exists: stop signals, observability,
//! registration, dependency admission, listeners, and readiness.
//!
//! A failed signal install returns before anything is open. Every later refusal
//! goes through the same retained-resource shutdown plan.

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

use health::Readiness;
use infra_telemetry::install_panic_hook;
use service_config::Config;

use crate::background::Background;
use crate::resources::{Listeners, Resources};
use crate::shutdown;
use crate::signals::{SignalError, Signals};
use crate::{BuildError, Register, Registration};

const METRICS_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(10);
/// The background task that watches readiness progress, as named in failures.
const READINESS_PROGRESS: &str = "readiness_progress";

/// Every startup refusal, plus an engine, a consumer, or a background task
/// stopping without a stop signal.
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
    #[error(transparent)]
    GraceBudget(#[from] shutdown::GraceBudgetError),
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
    Server(#[from] infra_http::ServerError),
    #[error(transparent)]
    HttpContract(#[from] infra_http::FinalizeError),
    #[error("startup admission: {0}")]
    Admission(health::NotReady),
}

/// How the running worker ended. A stop during startup counts as [`Self::Signal`].
#[derive(Debug)]
enum Ended {
    Signal,
    Failure(WorkerError),
}

/// Native work that has been admitted but has not started claiming or pulling.
struct Prepared {
    // template:begin jobs:worker-bootstrap-prepared-jobs
    engines: Vec<infra_jobs::Engine>,
    // template:end jobs:worker-bootstrap-prepared-jobs
    // template:begin messaging:worker-bootstrap-prepared-consumer
    consumer: Option<infra_messaging::Consumer>,
    // template:end messaging:worker-bootstrap-prepared-consumer
}

/// Retain cleanup ownership outside every operation that can stop or unwind.
///
/// `deadline` is written once, before teardown starts, so the caller can still
/// read the process deadline after this future unwinds.
pub(crate) async fn serve(
    config: Config,
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
    // Arbitration happens before teardown can cancel an incomplete round.
    let ended = settle_end(&resources, &background, ended);
    let deadline = *deadline.insert(shutdown::stop_deadline(
        signals.first_stop(),
        config.http.grace_period,
    ));
    if let Ended::Failure(error) = &ended {
        if resources.logger.is_some() {
            tracing::error!(error = %error, "jobs worker failed");
        } else {
            let _ = service_config::process_failure(error);
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
    config: &Config,
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
        // A retained failure outranks the stop or refusal that ended admission.
        Err(ended) => return pending_failure(resources, background).map_or(ended, Ended::Failure),
    };
    if let Some(ended) = pending_end(resources, background, signals) {
        return ended;
    }
    // template:begin jobs:worker-bootstrap-start-admitted-jobs
    if let Some(ended) = jobs::start_engines(prepared.engines, resources, background, signals) {
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
    if let Some(ended) = pending_end(resources, background, signals) {
        return ended;
    }
    tracing::info!("jobs_worker_ready");
    wait_for_stop(resources, background, signals).await
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

/// Check retained faults before a stop and before every claiming/ready transition.
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
    for (name, server) in [
        (Listeners::HEALTH, resources.listeners.health.as_ref()),
        (
            Listeners::DIAGNOSTICS,
            resources.listeners.diagnostics.as_ref(),
        ),
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
    resources
        .readiness
        .as_ref()
        .and_then(Readiness::progress_loss)
        .map(|_| WorkerError::BackgroundStopped(READINESS_PROGRESS))
}

/// Stop readiness progress first, so a retained failure found next outranks
/// a stop signal.
fn settle_end(resources: &Resources, background: &Background, ended: Ended) -> Ended {
    if let Some(readiness) = &resources.readiness {
        let _ = readiness.stop_progress();
    }
    match ended {
        Ended::Failure(_) => ended,
        Ended::Signal => pending_failure(resources, background).map_or(ended, Ended::Failure),
    }
}

async fn prepare(
    config: &Config,
    register: Register<'_>,
    background: &Background,
    resources: &mut Resources,
) -> Result<Prepared, WorkerError> {
    let identity = worker_identity(&config.observability.otel.service_name);
    let (metrics, exporter_state) = telemetry::install_observability(config, &identity, resources)?;
    // Docker, containerd and systemd start processes with a soft limit of
    // 1024 open files under a much higher hard limit, keeping the low value
    // for `select(2)` callers. Nothing here calls `select`, so raise the
    // soft limit to the hard one, as the Go runtime does since 1.19;
    // otherwise every connection past about a thousand fails to accept.
    // After the subscriber, so a failure's warning is not lost.
    if let Err(error) = rlimit::increase_nofile_limit(u64::MAX) {
        tracing::warn!(error = %error, "open_file_limit_unchanged");
    }
    // A handler may include its payload in a panic. Install before registration
    // and keep catch diagnostics free of the payload as well. The hook writes
    // through the subscriber, so it follows it.
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
    // template:begin jobs:worker-bootstrap-admit-pool-call
    postgres::admit_pool(config, &capabilities, background, resources).await?;
    // template:end jobs:worker-bootstrap-admit-pool-call
    // template:begin outbox:worker-bootstrap-activate-postgres-messages
    capabilities.activate_postgres_messages(resources)?;
    // template:end outbox:worker-bootstrap-activate-postgres-messages
    // template:begin messaging:worker-bootstrap-messaging-connect
    let consumer = messaging::connect_messaging(
        config,
        capabilities.messages.take(),
        &background.cancel,
        resources,
    )
    .await?;
    // template:end messaging:worker-bootstrap-messaging-connect
    // template:begin jobs:worker-bootstrap-build-engines
    let engines = jobs::build_engines(config, capabilities.jobs.take(), resources).await?;
    // template:end jobs:worker-bootstrap-build-engines
    let probes = listeners::readiness_probes(resources);
    let readiness = resources
        .readiness
        .insert(Readiness::new(probes, listeners::refresh_policy(config)))
        .clone();
    // A refused verdict leaves the bound listeners to teardown.
    listeners::bind_listeners(config, &metrics, &readiness, background, resources).await?;
    readiness.refresh().await;
    readiness
        .reader()
        .verdict()
        .map_err(WorkerError::Admission)?;
    listeners::start_progress(&readiness, background)?;
    Ok(Prepared {
        // template:begin jobs:worker-bootstrap-prepared-jobs-value
        engines,
        // template:end jobs:worker-bootstrap-prepared-jobs-value
        // template:begin messaging:worker-bootstrap-prepared-consumer-value
        consumer,
        // template:end messaging:worker-bootstrap-prepared-consumer-value
    })
}

struct RetainedCapabilities {
    // template:begin outbox:worker-bootstrap-postgres-messages-field
    postgres_messages: Option<crate::PostgresMessages>,
    // template:end outbox:worker-bootstrap-postgres-messages-field
    // template:begin jobs:worker-bootstrap-registrations-jobs
    jobs: Option<infra_jobs::Kinds>,
    // template:end jobs:worker-bootstrap-registrations-jobs
    // template:begin messaging:worker-bootstrap-registrations-messaging
    messages: Option<infra_messaging::Registry>,
    // template:end messaging:worker-bootstrap-registrations-messaging
}

fn register_capabilities(
    config: &Config,
    register: Register<'_>,
    background: &Background,
) -> Result<RetainedCapabilities, WorkerError> {
    let mut registration = Registration {
        // template:begin jobs:worker-bootstrap-register-jobs
        jobs: infra_jobs::KindsBuilder::new(),
        // template:end jobs:worker-bootstrap-register-jobs
        // template:begin messaging:worker-bootstrap-register-messaging
        messages: infra_messaging::Registry::try_new([])?,
        // template:end messaging:worker-bootstrap-register-messaging
        // template:begin outbox:worker-bootstrap-postgres-messages-initial
        postgres_messages: None,
        // template:end outbox:worker-bootstrap-postgres-messages-initial
        config,
        background,
    };
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
    // Taken, not moved, so the messaging check can still borrow the registration.
    let jobs = jobs::validate(std::mem::take(&mut registration.jobs))?;
    #[allow(
        unused_variables,
        reason = "retained outbox adds its own publication kind"
    )]
    let has_jobs = jobs.is_some();
    // template:end jobs:worker-bootstrap-validate-jobs
    // template:begin messaging:worker-bootstrap-retain-messaging
    let messages =
        messaging::consumes_messages(config, &registration)?.then_some(registration.messages);
    let has_messages = messages.is_some();
    // template:end messaging:worker-bootstrap-retain-messaging
    // template:begin outbox:worker-bootstrap-outbox-registration
    let has_jobs = true;
    // template:end outbox:worker-bootstrap-outbox-registration
    if !has_jobs && !has_messages {
        return Err(WorkerError::NoRegistrations);
    }
    Ok(RetainedCapabilities {
        // template:begin outbox:worker-bootstrap-postgres-messages-value
        postgres_messages: registration.postgres_messages,
        // template:end outbox:worker-bootstrap-postgres-messages-value
        // template:begin jobs:worker-bootstrap-registrations-jobs-value
        jobs,
        // template:end jobs:worker-bootstrap-registrations-jobs-value
        // template:begin messaging:worker-bootstrap-registrations-messaging-value
        messages,
        // template:end messaging:worker-bootstrap-registrations-messaging-value
    })
}

/// [`Ended::Signal`] when a stop signal ended the wait. A terminal jobs,
/// messaging, or background-task failure names its owner and takes the error
/// exit after ordered cleanup.
async fn wait_for_stop(
    resources: &Resources,
    background: &Background,
    signals: &mut Signals,
) -> Ended {
    tokio::select! {
        biased;
        result = signals.wait() => {
            // The next notification belongs to shutdown's expedite wait.
            pending_failure(resources, background)
                .map_or_else(|| signal_ended(result, background), Ended::Failure)
        },
        task = background.wait_for_failure() => Ended::Failure(WorkerError::BackgroundStopped(task)),
        // template:begin jobs:worker-bootstrap-wait-jobs-failure
        () = jobs::engine_stopped(resources) => Ended::Failure(WorkerError::EngineStopped),
        // template:end jobs:worker-bootstrap-wait-jobs-failure
        // template:begin messaging:worker-bootstrap-wait-messaging-failure
        error = messaging::consumer_stopped(resources) => {
            tracing::error!(error = %error, "messaging consumer stopped");
            Ended::Failure(WorkerError::ConsumerStopped(error))
        },
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

    use super::{Config, WorkerError};
    use crate::shutdown::validate_grace_budget;

    #[tokio::test(start_paused = true)]
    async fn progress_failure_prevents_admission_and_retains_tracker_custody() {
        let policy = health::RefreshPolicy {
            interval: Duration::from_secs(1),
            probe_budget: Duration::from_secs(1),
            failure_threshold: 3,
        };
        let readiness = health::Readiness::new(Vec::new(), policy);
        readiness.refresh().await;
        let background = super::Background::new();
        super::listeners::start_progress(&readiness, &background).unwrap();
        let resources = super::Resources {
            readiness: Some(readiness),
            ..super::Resources::default()
        };
        tokio::time::advance(policy.stale_after() + Duration::from_secs(1)).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), background.wait_for_failure())
                .await
                .unwrap(),
            "readiness_progress"
        );
        std::assert_matches!(
            super::pending_failure(&resources, &background),
            Some(super::WorkerError::BackgroundStopped("readiness_progress"))
        );
        assert_eq!(
            background.tracker.len(),
            2,
            "the report retains both task lifetimes"
        );
        let ended = super::settle_end(&resources, &background, super::Ended::Signal);
        std::assert_matches!(
            ended,
            super::Ended::Failure(super::WorkerError::BackgroundStopped("readiness_progress"))
        );
        background.cancel.cancel();
        background.tracker.close();
        tokio::time::timeout(Duration::from_secs(1), background.tracker.wait())
            .await
            .unwrap();
        assert!(background.tracker.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn stop_arbitrates_before_observer_reporting_and_preserves_primary_failure() {
        for (late, prior_failure) in [(false, false), (true, false), (true, true)] {
            let readiness = health::Readiness::new(
                Vec::new(),
                health::RefreshPolicy {
                    interval: Duration::from_secs(1),
                    probe_budget: Duration::from_secs(1),
                    failure_threshold: 3,
                },
            );
            readiness.refresh().await;
            readiness.arm_progress().unwrap();
            let resources = super::Resources {
                readiness: Some(readiness.clone()),
                ..super::Resources::default()
            };
            let background = super::Background::new();
            if late {
                tokio::time::advance(Duration::from_secs(5)).await;
            }
            let ended = super::settle_end(
                &resources,
                &background,
                if prior_failure {
                    super::Ended::Failure(super::WorkerError::Panicked)
                } else {
                    super::Ended::Signal
                },
            );
            match (late, prior_failure, ended) {
                (_, true, super::Ended::Failure(super::WorkerError::Panicked))
                | (
                    true,
                    false,
                    super::Ended::Failure(super::WorkerError::BackgroundStopped(
                        "readiness_progress",
                    )),
                )
                | (false, false, super::Ended::Signal) => {}
                _ => panic!("incorrect primary failure"),
            }
            tokio::time::advance(Duration::from_secs(5)).await;
            readiness.refresh().await;
            assert_eq!(readiness.progress_loss().is_some(), late);
            assert!(background.tracker.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
    )]
    fn first_stop_preserves_a_queued_second_stop_for_drain() {
        // Native signals are process-wide. Reuse this test binary with only
        // this case selected so sibling lifecycle tests cannot receive them.
        const CHILD: &str = "WORKER_LIFECYCLE_SIGNAL_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let thread = std::thread::current();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", thread.name().unwrap()])
                .env(CHILD, "1")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while child.try_wait().unwrap().is_none() {
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("isolated signal case did not finish");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
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
                // Broadcast delivery to the independent observers establishes
                // that both streams owned by Signals have pending notifications.
                tokio::time::timeout(Duration::from_secs(2), async {
                    assert_eq!(terminate_seen.recv().await, Some(()));
                    assert_eq!(interrupt_seen.recv().await, Some(()));
                })
                .await
                .unwrap();
                let background = super::Background::new();
                let resources = super::Resources::default();
                let ended = tokio::time::timeout(
                    Duration::from_secs(2),
                    super::wait_for_stop(&resources, &background, &mut signals),
                )
                .await
                .unwrap();
                std::assert_matches!(ended, super::Ended::Signal);
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
            });
    }

    #[tokio::test]
    async fn registration_unwind_cancels_and_joins_already_registered_work() {
        use futures_util::FutureExt;

        let config = Config::default();
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

        let mut config = Config::default();
        config.http.grace_period = Duration::from_secs(30);
        assert_eq!(
            WorkerError::from(validate_grace_budget(&config.http).unwrap_err()).to_string(),
            "http.grace_period (30s) must be >= http.drain_timeout (25s) plus the 18.5s jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush, SDK join slack, runtime shutdown)"
        );
    }
}
