//! Asynchronous startup after the runtime exists: stop signals, observability,
//! registration, dependency admission, listeners, and readiness.
//!
//! A failed signal install returns before anything is open. Every later refusal
//! goes through the same retained-resource shutdown plan.

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt;
use tokio::time::Instant;

use health::{Probe, Readiness, RefreshPolicy};
use infra_http::{
    HTTP_REQUESTS_DURATION_BUCKETS, HTTP_REQUESTS_DURATION_SECONDS, HardenOptions, Server,
    ServerOptions,
};
// template:begin jobs:worker-bootstrap-jobs-imports
use infra_jobs::{ATTEMPT_DURATION_BUCKETS, ATTEMPT_DURATION_METRIC, Engine, Kinds, Registry};
// template:end jobs:worker-bootstrap-jobs-imports
// template:begin messaging:worker-bootstrap-messaging-imports
use infra_messaging::{
    Consumer, ConsumerOptions, Messaging, MessagingError, MessagingOptions,
    Registry as MessagingRegistry,
};
// template:end messaging:worker-bootstrap-messaging-imports
// template:begin jobs:worker-bootstrap-postgres-imports
use infra_postgres::{Dsn, PgPool, PoolOptions, PostgresProbe, SessionBudgets};
// template:end jobs:worker-bootstrap-postgres-imports
use infra_telemetry::{
    ExporterState, LoggingFormat, LoggingOptions, Metrics, TracingOptions, diagnostics_router,
    install_panic_hook, install_subscriber, install_tracer_provider, runtime_metrics,
};
use secrecy::ExposeSecret;
use service_config::{AppConfig, Config, LogFormat, TracesSampler};

use crate::shutdown::{self, Background, Resources, Signals};
use crate::{BuildError, Register, Registration};

const METRICS_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(10);
// template:begin messaging:worker-bootstrap-messaging-startup-budget
const MESSAGING_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
// template:end messaging:worker-bootstrap-messaging-startup-budget

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
    // template:begin jobs:worker-bootstrap-postgres-disabled-error
    #[error("postgres.enabled must be true to run the jobs worker")]
    PostgresDisabled,
    // template:end jobs:worker-bootstrap-postgres-disabled-error
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
    #[error("job kinds are invalid: {0}")]
    Kinds(#[from] infra_jobs::KindError),
    #[error("the job engine stopped without a stop signal")]
    EngineStopped,
    // template:end jobs:worker-bootstrap-job-errors
    // template:begin outbox:worker-bootstrap-outbox-kind-error
    #[error("ordinary jobs cannot register the reserved outbox publication kind")]
    PublisherKindConflict,
    // template:end outbox:worker-bootstrap-outbox-kind-error
    // template:begin messaging:worker-bootstrap-messaging-errors
    #[error("typed message handlers are invalid: {0}")]
    MessagingRegistry(#[from] infra_messaging::RegistryError),
    #[error("messaging startup: {0}")]
    Messaging(#[from] MessagingError),
    #[error("messaging configuration requires {key}")]
    MessagingConfigRequired { key: &'static str },
    #[error("messaging.consumer_concurrency cannot fit this platform")]
    MessagingConcurrency,
    #[error("messaging.max_payload_bytes cannot fit this platform")]
    MessagingPayloadBound,
    #[error("the messaging consumer stopped without a stop signal: {0}")]
    ConsumerStopped(#[source] infra_messaging::ConsumerError),
    // template:end messaging:worker-bootstrap-messaging-errors
    // template:begin jobs:worker-bootstrap-postgres-errors
    #[error("configuration is invalid: postgres.dsn: {0}")]
    PostgresDsn(#[from] infra_postgres::DsnError),
    #[error(transparent)]
    Postgres(#[from] infra_postgres::ConnectError),
    #[error("postgres migration history: {0}")]
    PostgresHistory(#[from] migrate::HistoryError),
    #[error("jobs startup check: {0}")]
    JobsStartup(#[from] infra_jobs::StartupError),
    // template:end jobs:worker-bootstrap-postgres-errors
    #[error(transparent)]
    Server(#[from] infra_http::ServerError),
    #[error(transparent)]
    HttpContract(#[from] infra_http::FinalizeError),
    #[error("startup admission: {0}")]
    Admission(health::NotReady),
}

/// Validate process grace before building the runtime. Capability-specific
/// pool and broker admission follows registration and precedes dependency I/O.
pub(crate) fn check_preconditions(config: &Config) -> Result<(), WorkerError> {
    shutdown::validate_grace_budget(&config.http)?;
    Ok(())
}

/// How the running worker ended. A stop during startup counts as [`Self::Signal`].
enum Ended {
    Signal,
    Failure(WorkerError),
}

/// Native work that has been admitted but has not started claiming or pulling.
struct Prepared {
    // template:begin jobs:worker-bootstrap-prepared-jobs
    engines: Vec<Engine>,
    // template:end jobs:worker-bootstrap-prepared-jobs
    // template:begin messaging:worker-bootstrap-prepared-consumer
    consumer: Option<Consumer>,
    // template:end messaging:worker-bootstrap-prepared-consumer
}

/// Retain cleanup ownership outside every operation that can stop or unwind.
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
    let stop_at = signals.first_stop().unwrap_or_else(Instant::now);
    let deadline = *deadline.get_or_insert(stop_at + config.http.grace_period);
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
    if let Some(error) = pending_failure(resources, background) {
        return Ended::Failure(error);
    }
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(ended) => return ended,
    };
    if let Some(ended) = pending_end(resources, background, signals) {
        return ended;
    }
    // template:begin jobs:worker-bootstrap-start-admitted-jobs
    for engine in prepared.engines {
        if let Some(ended) = pending_end(resources, background, signals) {
            return ended;
        }
        resources
            .started
            .push(engine.start(&background.tracker, &background.cancel));
    }
    tracing::info!(engines = resources.started.len(), "jobs_claiming_started");
    // template:end jobs:worker-bootstrap-start-admitted-jobs
    // template:begin messaging:worker-bootstrap-start-admitted-consumer
    if let Some(consumer) = prepared.consumer {
        if let Some(ended) = pending_end(resources, background, signals) {
            return ended;
        }
        resources.consumer = Some(consumer.start(&background.cancel));
        tracing::info!("messaging_consuming_started");
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
            && server.failure().now_or_never().is_some()
        {
            return Some(WorkerError::ListenerStopped(name));
        }
    }
    // template:begin jobs:worker-bootstrap-pending-jobs
    if resources
        .started
        .iter()
        .any(|engine| engine.failed().now_or_never().is_some())
    {
        return Some(WorkerError::EngineStopped);
    }
    // template:end jobs:worker-bootstrap-pending-jobs
    // template:begin messaging:worker-bootstrap-pending-consumer
    if let Some(consumer) = resources.consumer.as_ref()
        && let Some(error) = consumer.failed().now_or_never()
    {
        return Some(WorkerError::ConsumerStopped(error));
    }
    // template:end messaging:worker-bootstrap-pending-consumer
    resources
        .readiness
        .as_ref()
        .and_then(Readiness::progress_loss)
        .map(|_| WorkerError::BackgroundStopped("readiness_progress"))
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
    // A handler may include its payload in a panic. Install before registration
    // and keep catch diagnostics free of the payload as well.
    install_panic_hook();
    let identity = worker_identity(&config.observability.otel.service_name);
    let metrics = install_observability(config, &identity, resources)?;
    let mut registrations = register_capabilities(config, register, background)?;
    if let Some(provider) = resources.tracer_provider.as_ref() {
        log_startup_record(
            config,
            &identity,
            // template:begin jobs:worker-bootstrap-log-jobs-argument
            registrations.jobs.as_ref(),
            // template:end jobs:worker-bootstrap-log-jobs-argument
            &provider.exporter_state,
        );
    }
    spawn_metrics_tasks(&metrics, background);
    admit_pool(config, &registrations, background, resources).await?;
    // template:begin messaging:worker-bootstrap-messaging-startup
    #[allow(
        unused_variables,
        reason = "retained outbox also requires broker admission"
    )]
    let needs_messaging = registrations.messages.is_some();
    // template:end messaging:worker-bootstrap-messaging-startup
    // template:begin outbox:worker-bootstrap-outbox-messaging
    let needs_messaging = true;
    // template:end outbox:worker-bootstrap-outbox-messaging
    // template:begin messaging:worker-bootstrap-messaging-connect
    let consumer = connect_messaging(
        config,
        &mut registrations.messages,
        &background.cancel,
        resources,
        needs_messaging,
    )
    .await?;
    // template:end messaging:worker-bootstrap-messaging-connect
    // template:begin jobs:worker-bootstrap-build-engines
    let engines = build_engines(config, &mut registrations, resources).await?;
    // template:end jobs:worker-bootstrap-build-engines
    // template:begin outbox:worker-bootstrap-outbox-engine
    let mut engines = engines;
    if let Some(publisher) = outbox_publisher(resources, engines.first()).await? {
        engines.push(publisher);
    }
    // template:end outbox:worker-bootstrap-outbox-engine
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

#[allow(
    unused_variables,
    reason = "retained jobs and outbox consume pool admission inputs"
)]
async fn admit_pool(
    config: &Config,
    registrations: &Registrations,
    background: &Background,
    resources: &mut Resources,
) -> Result<(), WorkerError> {
    // template:begin jobs:worker-bootstrap-jobs-startup
    #[allow(
        unused_variables,
        reason = "retained outbox also requires the shared pool"
    )]
    let needs_pool = registrations.jobs.is_some();
    // template:end jobs:worker-bootstrap-jobs-startup
    // template:begin outbox:worker-bootstrap-outbox-pool
    let needs_pool = true;
    // template:end outbox:worker-bootstrap-outbox-pool
    // template:begin jobs:worker-bootstrap-pool-admission
    if needs_pool {
        if !config.postgres.enabled {
            return Err(WorkerError::PostgresDisabled);
        }
        #[allow(
            unused_variables,
            reason = "retained outbox supplies the combined admission"
        )]
        let capacity = || config.jobs.validate_pool_capacity(&config.postgres);
        // template:end jobs:worker-bootstrap-pool-admission
        // template:begin outbox:worker-bootstrap-outbox-capacity
        let capacity = || {
            config
                .jobs
                .validate_pool_capacity_with_outbox(&config.postgres, registrations.jobs.is_some())
        };
        // template:end outbox:worker-bootstrap-outbox-capacity
        // template:begin jobs:worker-bootstrap-pool-capacity
        capacity()?;
        // template:end jobs:worker-bootstrap-pool-capacity
        // template:begin outbox:worker-bootstrap-outbox-config
        config.messaging.validate_producer(&config.app.env)?;
        // template:end outbox:worker-bootstrap-outbox-config
        // template:begin jobs:worker-bootstrap-pool-open
        let pool = open_pool(config, background, resources).await?;
        migrate::verify_history(&pool).await?;
    }
    // template:end jobs:worker-bootstrap-pool-open
    Ok(())
}

// template:begin messaging:worker-bootstrap-messaging-admit
async fn connect_messaging(
    config: &Config,
    messages: &mut Option<MessagingRegistry>,
    cancel: &tokio_util::sync::CancellationToken,
    resources: &mut Resources,
    needs_messaging: bool,
) -> Result<Option<Consumer>, WorkerError> {
    if !needs_messaging {
        return Ok(None);
    }
    let options = messaging_options(config, messages.is_some())?;
    let deadline = Instant::now() + MESSAGING_STARTUP_TIMEOUT;
    let startup = resources.messaging_startup.insert(Messaging::prepare(
        options,
        deadline,
        cancel.child_token(),
    )?);
    let messaging = startup.admit().await?;
    let messaging = resources.messaging.insert(messaging);
    resources.messaging_startup = None;
    if let Some(registry) = messages.take() {
        Ok(Some(messaging.consumer(registry).await?))
    } else {
        Ok(None)
    }
}
// template:end messaging:worker-bootstrap-messaging-admit

// template:begin jobs:worker-bootstrap-ordinary-engine
async fn build_engines(
    config: &Config,
    registrations: &mut Registrations,
    resources: &Resources,
) -> Result<Vec<Engine>, WorkerError> {
    let mut engines = Vec::new();
    if let Some(registry) = registrations.jobs.take() {
        let engine = Engine::new(
            resources
                .pool
                .as_ref()
                .ok_or(WorkerError::PostgresDisabled)?
                .clone(),
            registry,
            config.jobs.max_workers,
        );
        engine.check_startup().await?;
        engines.push(engine);
    }
    Ok(engines)
}
// template:end jobs:worker-bootstrap-ordinary-engine

// template:begin outbox:worker-bootstrap-outbox-publisher
async fn outbox_publisher(
    resources: &Resources,
    ordinary: Option<&Engine>,
) -> Result<Option<Engine>, WorkerError> {
    let messaging = resources
        .messaging
        .as_ref()
        .ok_or(MessagingError::Connection)?;
    let registry = infra_messaging::outbox::registry(messaging.producer())?;
    if ordinary.is_some_and(|ordinary| {
        registry
            .names()
            .any(|reserved| ordinary.kinds().any(|name| name == reserved))
    }) {
        return Err(WorkerError::PublisherKindConflict);
    }
    // One listener and one retention loop serve both engines of this process.
    let publisher = match ordinary {
        Some(ordinary) => ordinary.beside(registry, std::num::NonZeroU32::MIN),
        None => Engine::new(
            resources
                .pool
                .as_ref()
                .ok_or(WorkerError::PostgresDisabled)?
                .clone(),
            registry,
            std::num::NonZeroU32::MIN,
        ),
    };
    publisher.check_startup().await?;
    Ok(Some(publisher))
}
// template:end outbox:worker-bootstrap-outbox-publisher

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
    let tracer_provider =
        resources
            .tracer_provider
            .insert(install_tracer_provider(&tracing_options(
                config,
                identity,
                replica_instance_id(&config.app),
            ))?);
    resources.logger = Some(install_subscriber(&LoggingOptions {
        level: &config.log.level,
        format: match config.log.format {
            LogFormat::Json => LoggingFormat::Json,
            LogFormat::Text => LoggingFormat::Text,
        },
        tracer_provider: Some(tracer_provider),
    })?);
    let metrics = Metrics::install(&[
        (
            HTTP_REQUESTS_DURATION_SECONDS,
            HTTP_REQUESTS_DURATION_BUCKETS,
        ),
        // template:begin jobs:worker-bootstrap-jobs-histograms
        (
            infra_postgres::CONNECTION_WAIT_METRIC,
            infra_postgres::CONNECTION_WAIT_BUCKETS,
        ),
        (
            infra_postgres::TRANSACTION_DURATION_METRIC,
            infra_postgres::TRANSACTION_DURATION_BUCKETS,
        ),
        (
            infra_postgres::OPERATION_DURATION_METRIC,
            infra_postgres::OPERATION_DURATION_BUCKETS,
        ),
        (ATTEMPT_DURATION_METRIC, ATTEMPT_DURATION_BUCKETS),
        (
            infra_jobs::CLAIM_DURATION_METRIC,
            infra_jobs::CLAIM_DURATION_BUCKETS,
        ),
        (
            infra_jobs::QUEUE_WAIT_METRIC,
            infra_jobs::QUEUE_WAIT_BUCKETS,
        ),
        // template:end jobs:worker-bootstrap-jobs-histograms
        // template:begin outbound-http:worker-bootstrap-outbound-histogram
        (
            infra_outbound_http::REQUEST_DURATION_METRIC,
            infra_outbound_http::REQUEST_DURATION_BUCKETS,
        ),
        // template:end outbound-http:worker-bootstrap-outbound-histogram
    ])?;
    metrics.record_trace_exporter_initialized(resources.tracer_provider.as_ref().is_some_and(
        |provider| matches!(provider.exporter_state, ExporterState::Initialized { .. }),
    ));
    Ok(metrics)
}

struct Registrations {
    // template:begin jobs:worker-bootstrap-registrations-jobs
    jobs: Option<Registry>,
    // template:end jobs:worker-bootstrap-registrations-jobs
    // template:begin messaging:worker-bootstrap-registrations-messaging
    messages: Option<MessagingRegistry>,
    // template:end messaging:worker-bootstrap-registrations-messaging
}

fn register_capabilities(
    config: &Config,
    register: Register<'_>,
    background: &Background,
) -> Result<Registrations, WorkerError> {
    let mut registration = Registration {
        // template:begin jobs:worker-bootstrap-register-jobs
        jobs: Kinds::new(),
        // template:end jobs:worker-bootstrap-register-jobs
        // template:begin messaging:worker-bootstrap-register-messaging
        messages: MessagingRegistry::new([])?,
        // template:end messaging:worker-bootstrap-register-messaging
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
    let jobs = match registration.jobs.validate() {
        Ok(registry) => Some(registry),
        Err(infra_jobs::KindError::NoKinds) => None,
        Err(error) => return Err(WorkerError::Kinds(error)),
    };
    #[allow(
        unused_variables,
        reason = "retained outbox adds its own publication kind"
    )]
    let has_jobs = jobs.is_some();
    // template:end jobs:worker-bootstrap-validate-jobs
    // template:begin messaging:worker-bootstrap-validate-messaging
    let messages = if registration.messages.has_handlers() {
        config.messaging.validate_consumer(&config.app.env)?;
        Some(registration.messages)
    } else {
        None
    };
    let has_messages = messages.is_some();
    // template:end messaging:worker-bootstrap-validate-messaging
    // template:begin outbox:worker-bootstrap-outbox-registration
    let has_jobs = true;
    // template:end outbox:worker-bootstrap-outbox-registration
    if !has_jobs && !has_messages {
        return Err(WorkerError::NoRegistrations);
    }
    Ok(Registrations {
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

// template:begin jobs:worker-bootstrap-open-pool
async fn open_pool(
    config: &Config,
    background: &Background,
    resources: &mut Resources,
) -> Result<PgPool, WorkerError> {
    let dsn = Dsn::admit_with(
        config.postgres.required_dsn()?.expose_secret(),
        config.postgres.password_file.as_deref(),
    )?;
    let application_name = application_name(&config.observability.otel.service_name);
    let options = PoolOptions {
        max_connections: config.postgres.max_connections,
        application_name: &application_name,
        default_isolation: infra_postgres::Isolation::ReadCommitted,
        session_budgets: match config.postgres.session_budgets {
            service_config::PostgresSessionBudgets::Startup => SessionBudgets::Startup,
            service_config::PostgresSessionBudgets::Server => SessionBudgets::Server,
        },
    };
    let pool = infra_postgres::prepare_pool(&dsn, &options);
    resources.pool = Some(pool.clone());
    infra_postgres::admit_pool(&pool, &options).await?;
    tracing::info!(
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        postgres.max_connections = config.postgres.max_connections.get(),
        "postgres_pool_opened"
    );
    background.spawn("postgres_pool_metrics", |cancel| {
        infra_postgres::record_metrics_periodically(
            pool.clone(),
            METRICS_MAINTENANCE_INTERVAL,
            cancel,
        )
    });
    // The rotation task ends at once without a password file, and an ended
    // background task is a worker failure.
    if dsn.password_file().is_some() {
        background.spawn("postgres_password_refresh", |cancel| {
            infra_postgres::refresh_password_periodically(pool.clone(), dsn, cancel)
        });
    }
    Ok(pool)
}
// template:end jobs:worker-bootstrap-open-pool

async fn bind_listeners(
    config: &Config,
    metrics: &Metrics,
    background: &Background,
    resources: &mut Resources,
) -> Result<(), WorkerError> {
    let mut probes: Vec<Box<dyn Probe>> = Vec::new();
    // template:begin jobs:worker-bootstrap-listener-jobs-probe
    if let Some(pool) = resources.pool.as_ref() {
        probes.push(Box::new(PostgresProbe::new(pool.clone())));
    }
    // template:end jobs:worker-bootstrap-listener-jobs-probe
    // template:begin messaging:worker-bootstrap-listener-messaging-probe
    if let Some(messaging) = resources.messaging.as_ref() {
        probes.push(Box::new(messaging.probe()));
    }
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
    let failure = server.failure();
    background.spawn(name, |cancel| async move {
        tokio::select! {
            biased;
            failure = failure => tracing::error!(listener = name, ?failure, "listener_failed"),
            () = cancel.cancelled() => {}
        }
    });
}

// template:begin messaging:worker-bootstrap-messaging-options
fn messaging_options(
    config: &Config,
    needs_subscription: bool,
) -> Result<MessagingOptions, WorkerError> {
    let messaging = &config.messaging;
    let source_stream =
        messaging
            .source_stream
            .clone()
            .ok_or(WorkerError::MessagingConfigRequired {
                key: "messaging.source_stream",
            })?;
    let consumer = if needs_subscription {
        Some(ConsumerOptions {
            durable_name: messaging.consumer_durable.clone().ok_or(
                WorkerError::MessagingConfigRequired {
                    key: "messaging.consumer_durable",
                },
            )?,
            filter_subject: messaging.consumer_filter_subject.clone().ok_or(
                WorkerError::MessagingConfigRequired {
                    key: "messaging.consumer_filter_subject",
                },
            )?,
            dlq_subject: messaging.dlq_subject.clone().ok_or(
                WorkerError::MessagingConfigRequired {
                    key: "messaging.dlq_subject",
                },
            )?,
            concurrency: usize::try_from(messaging.consumer_concurrency.get())
                .map_err(|_| WorkerError::MessagingConcurrency)?,
        })
    } else {
        None
    };
    Ok(MessagingOptions {
        connection_name: worker_identity(&config.observability.otel.service_name),
        servers: messaging.urls.clone(),
        credentials: messaging.credentials.clone(),
        credentials_file: messaging.credentials_file.clone(),
        root_ca_path: messaging.root_ca_path.clone(),
        allow_plaintext: messaging.plaintext_admitted(),
        source_stream,
        dlq_stream: None,
        max_payload_bytes: usize::try_from(messaging.max_payload_bytes.as_u64())
            .map_err(|_| WorkerError::MessagingPayloadBound)?,
        consumer,
    })
}
// template:end messaging:worker-bootstrap-messaging-options

fn start_progress(readiness: &Readiness, background: &Background) -> Result<(), WorkerError> {
    readiness.arm_progress().map_err(|error| {
        if readiness.progress_loss().is_some() {
            WorkerError::BackgroundStopped("readiness_progress")
        } else {
            WorkerError::Admission(error)
        }
    })?;
    let observed = readiness.clone();
    let reporter = background.clone();
    background.spawn("readiness_progress", |cancel| async move {
        if observed
            .wait_for_progress_loss(cancel.clone())
            .await
            .is_some()
        {
            reporter.record_failure("readiness_progress");
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
        () = async {
            if resources.started.is_empty() {
                std::future::pending::<()>().await;
            } else {
                futures_util::future::select_all(
                    resources.started.iter().map(|engine| Box::pin(engine.failed())),
                ).await;
            }
        } => Ended::Failure(WorkerError::EngineStopped),
        // template:end jobs:worker-bootstrap-wait-jobs-failure
        // template:begin messaging:worker-bootstrap-wait-messaging-failure
        error = async {
            if let Some(consumer) = resources.consumer.as_ref() {
                consumer.failed().await
            } else {
                std::future::pending::<infra_messaging::ConsumerError>().await
            }
        } => {
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

fn worker_identity(service_name: &str) -> String {
    format!("{service_name}-jobs-worker")
}

// template:begin jobs:worker-bootstrap-application-name
const APPLICATION_NAME_SUFFIX: &str = "-jobs-worker";
const POSTGRES_IDENTIFIER_LIMIT: usize = 63;

fn application_name(service_name: &str) -> String {
    let end =
        service_name.floor_char_boundary(POSTGRES_IDENTIFIER_LIMIT - APPLICATION_NAME_SUFFIX.len());
    format!("{}{APPLICATION_NAME_SUFFIX}", &service_name[..end])
}
// template:end jobs:worker-bootstrap-application-name

fn replica_instance_id(app: &AppConfig) -> String {
    app.instance_id
        .clone()
        .unwrap_or_else(|| gethostname::gethostname().to_string_lossy().into_owned())
}

fn tracing_options(config: &Config, identity: &str, instance_id: String) -> TracingOptions {
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
        instance_id,
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
    registry: Option<&Registry>,
    // template:end jobs:worker-bootstrap-log-jobs-parameter
    exporter: &ExporterState,
) {
    exporter.log();
    // template:begin jobs:worker-bootstrap-log-jobs-kinds
    let kinds = registry
        .map(|registry| registry.names().collect::<Vec<_>>().join(","))
        .unwrap_or_default();
    // template:end jobs:worker-bootstrap-log-jobs-kinds
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
        jobs.kinds = %kinds,
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

    // template:begin jobs:worker-bootstrap-test-jobs-imports
    use super::application_name;
    use infra_jobs::{KindError, StartupError};
    // template:end jobs:worker-bootstrap-test-jobs-imports

    use super::{Config, WorkerError, check_preconditions};

    #[cfg(unix)]
    #[test]
    #[allow(
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
    fn preconditions_only_reserve_the_process_grace_budget() {
        let mut config = Config::default();
        config.http.grace_period = Duration::from_secs(30);
        assert!(matches!(
            check_preconditions(&config),
            Err(WorkerError::GraceBudget(_))
        ));

        config.http.grace_period = Duration::from_millis(43_500);
        check_preconditions(&config).unwrap();
    }

    #[test]
    fn refusal_texts_name_the_failed_check() {
        assert_eq!(
            WorkerError::NoRegistrations.to_string(),
            "no job kind or typed message handler is registered: register this service's retained capabilities in crates/jobs-worker/src/main.rs"
        );
        // template:begin jobs:worker-bootstrap-test-postgres-refusal
        assert_eq!(
            WorkerError::PostgresDisabled.to_string(),
            "postgres.enabled must be true to run the jobs worker"
        );
        // template:end jobs:worker-bootstrap-test-postgres-refusal

        let mut config = Config::default();
        config.http.grace_period = Duration::from_secs(30);
        assert_eq!(
            check_preconditions(&config).unwrap_err().to_string(),
            "http.grace_period (30s) must be >= http.drain_timeout (25s) plus the 18.5s jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush, SDK join slack, runtime shutdown)"
        );
        // template:begin jobs:worker-bootstrap-test-jobs-refusals
        assert_eq!(
            WorkerError::Kinds(KindError::Duplicate("welcome")).to_string(),
            "job kinds are invalid: job kind \"welcome\" is registered twice"
        );
        assert_eq!(
            WorkerError::JobsStartup(StartupError::Unavailable).to_string(),
            "jobs startup check: the jobs store is unavailable"
        );
        assert_eq!(
            WorkerError::EngineStopped.to_string(),
            "the job engine stopped without a stop signal"
        );
        // template:end jobs:worker-bootstrap-test-jobs-refusals
        // template:begin messaging:worker-bootstrap-test-consumer-failure
        assert_eq!(
            WorkerError::ConsumerStopped(infra_messaging::ConsumerError::ConsumerLost).to_string(),
            "the messaging consumer stopped without a stop signal: messaging durable consumer was deleted or replaced"
        );
        // template:end messaging:worker-bootstrap-test-consumer-failure
    }

    // template:begin jobs:worker-bootstrap-test-application-name
    #[test]
    fn identity_keeps_the_suffix_inside_the_postgres_limit() {
        assert_eq!(super::worker_identity("service"), "service-jobs-worker");
        let cases = [
            ("service".to_owned(), "service-jobs-worker".to_owned()),
            ("a".repeat(64), format!("{}-jobs-worker", "a".repeat(51))),
            ("a".repeat(51), format!("{}-jobs-worker", "a".repeat(51))),
            (
                format!("{}é", "a".repeat(50)),
                format!("{}-jobs-worker", "a".repeat(50)),
            ),
            (
                format!("{}é", "a".repeat(49)),
                format!("{}é-jobs-worker", "a".repeat(49)),
            ),
            ("あ".repeat(20), format!("{}-jobs-worker", "あ".repeat(17))),
        ];
        for (input, expected) in cases {
            let name = application_name(&input);
            assert_eq!(name, expected, "{input}");
            assert!(name.ends_with("-jobs-worker"), "{name}");
            assert!(name.len() <= 63, "{}", name.len());
        }
    }
    // template:end jobs:worker-bootstrap-test-application-name
}
