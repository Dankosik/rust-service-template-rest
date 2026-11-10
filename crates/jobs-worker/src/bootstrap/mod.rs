//! Asynchronous startup after the runtime exists: stop signals, observability,
//! registration, dependency admission, listeners, and readiness.
//!
//! A failed signal install returns before anything is open. Every later refusal
//! goes through the same retained-resource shutdown plan.

// template:begin jobs:worker-bootstrap-jobs-module
mod jobs;
// template:end jobs:worker-bootstrap-jobs-module
// template:begin messaging:worker-bootstrap-messaging-module
mod messaging;
// template:end messaging:worker-bootstrap-messaging-module
// template:begin outbox:worker-bootstrap-outbox-module
mod outbox;
// template:end outbox:worker-bootstrap-outbox-module
// template:begin jobs:worker-bootstrap-postgres-module
pub(crate) mod postgres;
// template:end jobs:worker-bootstrap-postgres-module

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt;
use tokio::time::Instant;

use health::{Probe, Readiness, RefreshPolicy};
use infra_http::{HardenOptions, Server, ServerOptions};
use infra_telemetry::{
    ExporterState, LoggingFormat, LoggingOptions, Metrics, TracingOptions, diagnostics_router,
    install_panic_hook, install_subscriber, install_tracer_provider, runtime_metrics,
};
use service_config::{Config, LogFormat, TracesSampler};

use crate::shutdown::{self, Background, Resources, Signals};
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
    SignalClosed(#[from] shutdown::SignalError),
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
    #[error("job kind registration failed: {0}")]
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
    let ended = stop_progress(&resources, &background, ended);
    let deadline = *deadline.get_or_insert(shutdown::stop_deadline(
        signals.first_stop(),
        config.http.grace_period,
    ));
    if let Ended::Failure(error) = &ended {
        if resources.logger.is_some() {
            tracing::error!(error = %error, "jobs worker failed");
        } else {
            let _ = service_config::process_failure(&error.to_string());
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
            task = background.stopped() => Err(Ended::Failure(WorkerError::BackgroundStopped(task))),
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

fn signal_ended(result: Result<(), shutdown::SignalError>, background: &Background) -> Ended {
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
    match signals.pending() {
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
        ("health_listener", resources.listeners.health.as_ref()),
        (
            "diagnostics_listener",
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

fn stop_progress(resources: &Resources, background: &Background, ended: Ended) -> Ended {
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
    // Docker, containerd and systemd start processes with a soft limit of
    // 1024 open files under a much higher hard limit, keeping the low value
    // for `select(2)` callers. Nothing here calls `select`, so raise the
    // soft limit to the hard one, as the Go runtime does since 1.19;
    // otherwise every connection past about a thousand fails to accept.
    if let Err(error) = rlimit::increase_nofile_limit(u64::MAX) {
        tracing::warn!(error = %error, "open_file_limit_unchanged");
    }
    let identity = worker_identity(&config.observability.otel.service_name);
    let metrics = install_observability(config, &identity, resources)?;
    // A handler may include its payload in a panic. Install before registration
    // and keep catch diagnostics free of the payload as well. The hook writes
    // through the subscriber, so it follows it.
    install_panic_hook();
    let mut capabilities = register_capabilities(config, register, background)?;
    if let Some(provider) = resources.tracer_provider.as_ref() {
        log_startup_record(
            config,
            &identity,
            // template:begin jobs:worker-bootstrap-log-jobs-argument
            capabilities.jobs.as_ref(),
            // template:end jobs:worker-bootstrap-log-jobs-argument
            &provider.exporter_state,
        );
    }
    spawn_metrics_tasks(&metrics, background);
    // template:begin jobs:worker-bootstrap-admit-pool-call
    postgres::admit_pool(config, &capabilities, background, resources).await?;
    // template:end jobs:worker-bootstrap-admit-pool-call
    // template:begin outbox:worker-bootstrap-activate-postgres-messages
    capabilities.activate_postgres_messages(resources)?;
    // template:end outbox:worker-bootstrap-activate-postgres-messages
    // template:begin messaging:worker-bootstrap-messaging-connect
    let consumer = messaging::connect_messaging(
        config,
        &mut capabilities.messages,
        &background.cancel,
        resources,
    )
    .await?;
    // template:end messaging:worker-bootstrap-messaging-connect
    // template:begin jobs:worker-bootstrap-build-engines
    let engines = jobs::build_engines(config, &mut capabilities, resources).await?;
    // template:end jobs:worker-bootstrap-build-engines
    bind_listeners(config, &metrics, background, resources).await?;
    if let Some(readiness) = resources.readiness.as_ref() {
        readiness.refresh().await;
        readiness
            .reader()
            .verdict()
            .map_err(WorkerError::Admission)?;
        start_progress(readiness, background)?;
    }
    Ok(Prepared {
        // template:begin jobs:worker-bootstrap-prepared-jobs-value
        engines,
        // template:end jobs:worker-bootstrap-prepared-jobs-value
        // template:begin messaging:worker-bootstrap-prepared-consumer-value
        consumer,
        // template:end messaging:worker-bootstrap-prepared-consumer-value
    })
}

fn refresh_policy(config: &Config) -> RefreshPolicy {
    RefreshPolicy {
        interval: config.health.refresh_interval,
        probe_budget: config.health.probe_budget,
        failure_threshold: config.health.failure_threshold,
    }
}

fn install_observability(
    config: &Config,
    identity: &str,
    resources: &mut Resources,
) -> Result<Metrics, WorkerError> {
    let tracer_provider = resources
        .tracer_provider
        .insert(install_tracer_provider(&tracing_options(config, identity))?);
    resources.logger = Some(install_subscriber(&LoggingOptions {
        level: &config.log.level,
        format: match config.log.format {
            LogFormat::Json => LoggingFormat::Json,
            LogFormat::Text => LoggingFormat::Text,
        },
        tracer_provider: Some(tracer_provider),
    })?);
    let metrics = Metrics::install(
        &[
            infra_http::HISTOGRAMS,
            // template:begin jobs:worker-bootstrap-jobs-histograms
            infra_postgres::HISTOGRAMS,
            infra_jobs::HISTOGRAMS,
            // template:end jobs:worker-bootstrap-jobs-histograms
            // template:begin outbound-http:worker-bootstrap-outbound-histogram
            infra_outbound_http::HISTOGRAMS,
            // template:end outbound-http:worker-bootstrap-outbound-histogram
        ]
        .concat(),
    )?;
    metrics.record_trace_exporter(
        resources
            .tracer_provider
            .as_ref()
            .map(|provider| &provider.exporter_state),
    );
    Ok(metrics)
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

fn spawn_metrics_tasks(metrics: &Metrics, background: &Background) {
    background.spawn("metrics_upkeep", |cancel| metrics.clone().upkeep(cancel));
    background.spawn("runtime_progress", |cancel| {
        metrics.clone().runtime_progress(cancel)
    });
    background.spawn("runtime_metrics", |cancel| {
        runtime_metrics(METRICS_MAINTENANCE_INTERVAL, cancel)
    });
}

async fn bind_listeners(
    config: &Config,
    metrics: &Metrics,
    background: &Background,
    resources: &mut Resources,
) -> Result<(), WorkerError> {
    let mut probes: Vec<Box<dyn Probe>> = Vec::new();
    // template:begin jobs:worker-bootstrap-listener-jobs-probe
    probes.extend(postgres::probe(resources));
    // template:end jobs:worker-bootstrap-listener-jobs-probe
    // template:begin messaging:worker-bootstrap-listener-messaging-probe
    probes.extend(messaging::probe(resources));
    // template:end messaging:worker-bootstrap-listener-messaging-probe
    let readiness = resources
        .readiness
        .insert(Readiness::new(probes, refresh_policy(config)));
    let options = server_options(config);
    let routes = infra_http::finalize_public(infra_http::router())?.with_state(readiness.reader());
    let app = infra_http::harden(routes, &harden_options(config));
    let health = resources
        .listeners
        .health
        .insert(Server::bind(config.http.addr, app, options).await?);
    watch_listener("health_listener", health, background);
    tracing::info!(addr = %health.local_addr(), "http listener bound");
    if let Some(addr) = config.observability.metrics.addr {
        // Liveness is served here as well, as in the service, so one probe
        // target fits both processes.
        let routes = diagnostics_router(metrics.clone()).merge(infra_http::liveness_router());
        let diagnostics = resources
            .listeners
            .diagnostics
            .insert(Server::bind(addr, routes, options).await?);
        watch_listener("diagnostics_listener", diagnostics, background);
        tracing::info!(addr = %diagnostics.local_addr(), "diagnostics listener bound");
    }
    Ok(())
}

fn watch_listener(name: &'static str, server: &Server, background: &Background) {
    let failure = server.accept_failure();
    background.spawn(name, |cancel| async move {
        tokio::select! {
            biased;
            failure = failure => tracing::error!(listener = name, ?failure, "listener_failed"),
            () = cancel.cancelled() => {}
        }
    });
}

fn start_progress(readiness: &Readiness, background: &Background) -> Result<(), WorkerError> {
    readiness.arm_progress().map_err(|error| {
        if readiness.progress_loss().is_some() {
            WorkerError::BackgroundStopped(READINESS_PROGRESS)
        } else {
            WorkerError::Admission(error)
        }
    })?;
    let observed = readiness.clone();
    let reporter = background.clone();
    background.spawn(READINESS_PROGRESS, |cancel| async move {
        if observed
            .wait_for_progress_loss(cancel.clone())
            .await
            .is_some()
        {
            reporter.record_failure(READINESS_PROGRESS);
            cancel.cancelled().await;
        }
    });
    let readiness = readiness.clone();
    background.spawn("readiness_refresher", |cancel| async move {
        readiness.refresh_until(cancel).await;
    });
    Ok(())
}

/// `Ended::Signal` when a stop signal ended the wait. A terminal jobs,
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
        task = background.stopped() => Ended::Failure(WorkerError::BackgroundStopped(task)),
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

fn server_options(config: &Config) -> ServerOptions {
    ServerOptions {
        header_read_timeout: config.http.header_read_timeout,
        max_header_bytes: usize::try_from(config.http.max_header_bytes.as_u64())
            .unwrap_or(usize::MAX),
        max_connections: config.http.connection_cap(),
        max_connection_age: config.http.connection_age(),
    }
}

fn harden_options(config: &Config) -> HardenOptions {
    HardenOptions {
        max_body_bytes: usize::try_from(config.http.max_body_bytes.as_u64()).unwrap_or(usize::MAX),
        request_timeout: config.http.request_timeout,
        max_in_flight: config.http.in_flight_cap(),
        log_health_probes: config.http.access_log_health_probes,
    }
}

const IDENTITY_SUFFIX: &str = "-jobs-worker";

fn worker_identity(service_name: &str) -> String {
    format!("{service_name}{IDENTITY_SUFFIX}")
}

fn tracing_options(config: &Config, identity: &str) -> TracingOptions {
    let otel = &config.observability.otel;
    let sampler = match otel.traces_sampler {
        TracesSampler::AlwaysOn => infra_telemetry::ResolvedSampler::AlwaysOn,
        TracesSampler::AlwaysOff => infra_telemetry::ResolvedSampler::AlwaysOff,
        TracesSampler::TraceIdRatio => {
            infra_telemetry::ResolvedSampler::TraceIdRatio(otel.traces_sampler_arg)
        }
        TracesSampler::ParentBasedTraceIdRatio => {
            infra_telemetry::ResolvedSampler::ParentBasedTraceIdRatio(otel.traces_sampler_arg)
        }
    };
    TracingOptions {
        service_name: identity.to_owned(),
        service_version: config.app.version.clone(),
        vcs_revision: config.app.commit.clone(),
        instance_id: config.app.instance_id.clone(),
        deployment_environment: config.app.env.clone(),
        sampler,
        otlp_endpoint: otel.exporter.otlp_endpoint.clone(),
        otlp_headers: otel.exporter.otlp_headers.clone(),
    }
}

fn log_startup_record(
    config: &Config,
    identity: &str,
    // template:begin jobs:worker-bootstrap-log-jobs-parameter
    registry: Option<&infra_jobs::Kinds>,
    // template:end jobs:worker-bootstrap-log-jobs-parameter
    exporter: &ExporterState,
) {
    exporter.log();
    tracing::info!(
        service.name = %identity,
        app.env = %config.app.env,
        app.version = %config.app.version,
        app.commit = %config.app.commit,
        runtime.worker_threads = tokio::runtime::Handle::current().metrics().num_workers(),
        http.addr = %config.http.addr,
        http.drain_timeout = ?config.http.drain_timeout,
        http.grace_period = ?config.http.grace_period,
        observability.metrics.addr = ?config.observability.metrics.addr,
        // template:begin jobs:worker-bootstrap-log-jobs-fields
        postgres.max_connections = config.postgres.max_connections.get(),
        jobs.max_workers = config.jobs.max_workers.get(),
        jobs.kinds = %jobs::kind_names(registry),
        // template:end jobs:worker-bootstrap-log-jobs-fields
        log.level = %config.log.level,
        tracing.exporter = exporter.as_str(),
        "jobs_worker_starting"
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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
        super::start_progress(&readiness, &background).unwrap();
        let resources = super::Resources {
            readiness: Some(readiness),
            ..super::Resources::default()
        };
        tokio::time::advance(policy.stale_after() + Duration::from_secs(1)).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), background.stopped())
                .await
                .unwrap(),
            "readiness_progress"
        );
        assert!(matches!(
            super::pending_failure(&resources, &background),
            Some(super::WorkerError::BackgroundStopped("readiness_progress"))
        ));
        assert_eq!(
            background.tracker.len(),
            2,
            "the report retains both task lifetimes"
        );
        let ended = super::stop_progress(&resources, &background, super::Ended::Signal);
        assert!(matches!(
            ended,
            super::Ended::Failure(super::WorkerError::BackgroundStopped("readiness_progress"))
        ));
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
            let ended = super::stop_progress(
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

    use super::{Config, WorkerError};
    use crate::shutdown::validate_grace_budget;

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
                assert!(matches!(ended, super::Ended::Signal));
                let first_stop = signals.first_stop();
                assert!(first_stop.is_some());
                assert!(
                    matches!(signals.wait().now_or_never(), Some(Ok(()))),
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
            super::serve(config, registration, &mut signals, &mut deadline),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(WorkerError::Panicked)), "{result:?}");
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
