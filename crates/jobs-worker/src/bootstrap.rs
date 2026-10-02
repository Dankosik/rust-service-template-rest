//! Asynchronous startup after the runtime exists: stop signals, observability,
//! registration, dependency admission, listeners, and readiness.
//!
//! A failed signal install returns before anything is open. Every later refusal
//! goes through `shutdown::abort_startup`.

use std::time::Duration;

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
    ExporterState, LoggingFormat, LoggingOptions, Metrics, TracerProviderHandle, TracingOptions,
    diagnostics_router, install_subscriber, install_tracer_provider, runtime_metrics,
};
use secrecy::ExposeSecret;
use service_config::{AppConfig, Config, LogFormat, TracesSampler};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::shutdown::{self, Resources, Signals};
use crate::{BuildError, Register, Registration};

const METRICS_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(10);
// template:begin messaging:worker-bootstrap-messaging-startup-budget
const MESSAGING_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
// template:end messaging:worker-bootstrap-messaging-startup-budget

/// Every startup refusal, plus an engine or consumer stopping without a stop signal.
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
    Tracing(#[from] infra_telemetry::TracingError),
    #[error(transparent)]
    Logging(#[from] infra_telemetry::LoggingError),
    #[error(transparent)]
    Metrics(#[from] infra_telemetry::MetricsError),
    #[error("job kind registration failed: {0}")]
    Registration(#[source] BuildError),
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

/// Observability and readiness handed back when startup was not refused.
struct Prepared {
    tracer_provider: TracerProviderHandle,
    readiness: Readiness,
    admitted: bool,
}

/// Install stop signals, admit dependencies, then wait until a stop signal or
/// a terminal engine or consumer failure. A failed signal install returns
/// before anything is open. Every later refusal goes through
/// `shutdown::abort_startup` exactly once. A stop signal or an engine failure
/// runs the staged shutdown plan.
pub(crate) async fn serve(
    config: Config,
    register: Register<'_>,
) -> Result<shutdown::Outcome, WorkerError> {
    let mut signals = Signals::install().map_err(WorkerError::Signals)?;
    let cancel = CancellationToken::new();
    let tracker = TaskTracker::new();
    let mut resources = Resources::default();
    let prepared = match Box::pin(prepare(
        &config,
        register,
        &mut signals,
        &cancel,
        &tracker,
        &mut resources,
    ))
    .await
    {
        Ok(prepared) => prepared,
        Err(err) => {
            shutdown::abort_startup(resources, &cancel, &tracker).await;
            return Err(err);
        }
    };
    let ended = if prepared.admitted {
        spawn_refresher(&prepared.readiness, &cancel, &tracker);
        tracing::info!("jobs_worker_ready");
        wait_for_stop(&resources, &mut signals).await
    } else {
        Ended::Signal
    };
    let outcome = shutdown::run(shutdown::Plan {
        http: &config.http,
        readiness: &prepared.readiness,
        resources,
        cancel,
        tracker,
        tracer_provider: prepared.tracer_provider,
        signals: &mut signals,
    })
    .await;
    match ended {
        Ended::Signal => Ok(outcome),
        Ended::Failure(error) => Err(error),
    }
}

async fn prepare(
    config: &Config,
    register: Register<'_>,
    signals: &mut Signals,
    cancel: &CancellationToken,
    tracker: &TaskTracker,
    resources: &mut Resources,
) -> Result<Prepared, WorkerError> {
    let identity = worker_identity(&config.observability.otel.service_name);
    let (tracer_provider, metrics) = install_observability(config, &identity)?;
    install_sanitized_panic_hook();
    let mut registrations = register_capabilities(config, register, cancel, tracker)?;
    log_startup_record(
        config,
        &identity,
        // template:begin jobs:worker-bootstrap-log-jobs-argument
        registrations.jobs.as_ref(),
        // template:end jobs:worker-bootstrap-log-jobs-argument
        &tracer_provider.exporter_state,
    );
    spawn_metrics_tasks(&metrics, cancel, tracker);
    admit_pool(config, &registrations, cancel, tracker, resources).await?;
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
    let connected = Box::pin(connect_messaging(
        config,
        &mut registrations.messages,
        signals,
        cancel,
        resources,
        needs_messaging,
    ))
    .await?;
    let std::ops::ControlFlow::Continue(consumer) = connected else {
        // A stop signal ended broker admission: nothing is bound or admitted.
        return Ok(Prepared {
            tracer_provider,
            readiness: Readiness::new(Vec::new(), refresh_policy(config)),
            admitted: false,
        });
    };
    // template:end messaging:worker-bootstrap-messaging-connect
    // template:begin jobs:worker-bootstrap-build-engines
    let mut engines = build_engines(config, &mut registrations, resources).await?;
    // template:end jobs:worker-bootstrap-build-engines
    // template:begin outbox:worker-bootstrap-outbox-engine
    if let Some(publisher) = outbox_publisher(resources, engines.first()).await? {
        engines.push(publisher);
    }
    // template:end outbox:worker-bootstrap-outbox-engine
    let readiness = bind_listeners(config, &metrics, resources).await?;
    let admitted = !signals.pending() && admit(signals, &readiness).await?;
    // template:begin jobs:worker-bootstrap-start-admitted-jobs
    if admitted {
        resources.started = engines
            .iter()
            .map(|engine| engine.start(tracker, cancel))
            .collect();
        tracing::info!(engines = resources.started.len(), "jobs_claiming_started");
    }
    // template:end jobs:worker-bootstrap-start-admitted-jobs
    // template:begin messaging:worker-bootstrap-start-admitted-consumer
    if admitted && let Some(consumer) = consumer {
        resources.consumer = Some(consumer.start(cancel));
        tracing::info!("messaging_consuming_started");
    }
    // template:end messaging:worker-bootstrap-start-admitted-consumer
    Ok(Prepared {
        tracer_provider,
        readiness,
        admitted,
    })
}

#[allow(
    unused_variables,
    reason = "retained jobs and outbox consume pool admission inputs"
)]
async fn admit_pool(
    config: &Config,
    registrations: &Registrations,
    cancel: &CancellationToken,
    tracker: &TaskTracker,
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
        let pool = open_pool(config, cancel, tracker, resources).await?;
        migrate::verify_history(&pool).await?;
    }
    // template:end jobs:worker-bootstrap-pool-open
    Ok(())
}

// template:begin messaging:worker-bootstrap-messaging-admit
async fn connect_messaging(
    config: &Config,
    messages: &mut Option<MessagingRegistry>,
    signals: &mut Signals,
    cancel: &CancellationToken,
    resources: &mut Resources,
    needs_messaging: bool,
) -> Result<std::ops::ControlFlow<(), Option<Consumer>>, WorkerError> {
    use std::ops::ControlFlow;

    if !needs_messaging {
        return Ok(ControlFlow::Continue(None));
    }
    let options = messaging_options(config, messages.is_some())?;
    let deadline = tokio::time::Instant::now() + MESSAGING_STARTUP_TIMEOUT;
    let startup_cancel = cancel.child_token();
    let connect = Messaging::connect(options, deadline, startup_cancel.clone());
    tokio::pin!(connect);
    let (connected, stopped) = tokio::select! {
        biased;
        () = signals.wait() => {
            startup_cancel.cancel();
            (connect.await, true)
        }
        connected = &mut connect => (connected, false),
    };
    if stopped {
        resources.messaging = connected.ok();
        return Ok(ControlFlow::Break(()));
    }
    let messaging = resources.messaging.insert(connected?);
    if let Some(registry) = messages.take() {
        let admit_consumer = messaging.consumer(registry);
        tokio::pin!(admit_consumer);
        tokio::select! {
            biased;
            () = signals.wait() => {
                startup_cancel.cancel();
                let _ = admit_consumer.await;
                Ok(ControlFlow::Break(()))
            }
            consumer = &mut admit_consumer => Ok(ControlFlow::Continue(Some(consumer?))),
        }
    } else {
        Ok(ControlFlow::Continue(None))
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

/// A panic in a job or message handler is a recorded outcome of that attempt.
/// Replace Rust's default hook so panic text, which can quote the payload a
/// caller supplied, never reaches logs: the record names only where the panic
/// was raised, inside the attempt's span.
fn install_sanitized_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let location = info.location();
        tracing::error!(
            panic.file = location.map_or("<unknown>", std::panic::Location::file),
            panic.line = location.map(std::panic::Location::line),
            panic.column = location.map(std::panic::Location::column),
            "background task panicked"
        );
    }));
}

fn install_observability(
    config: &Config,
    identity: &str,
) -> Result<(TracerProviderHandle, Metrics), WorkerError> {
    let tracer_provider = install_tracer_provider(&tracing_options(
        config,
        identity,
        replica_instance_id(&config.app),
    ))?;
    install_subscriber(&LoggingOptions {
        level: &config.log.level,
        format: match config.log.format {
            LogFormat::Json => LoggingFormat::Json,
            LogFormat::Text => LoggingFormat::Text,
        },
        tracer_provider: Some(&tracer_provider),
    })?;
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
    metrics.record_trace_exporter_initialized(matches!(
        tracer_provider.exporter_state,
        ExporterState::Initialized { .. }
    ));
    Ok((tracer_provider, metrics))
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
    cancel: &CancellationToken,
    tracker: &TaskTracker,
) -> Result<Registrations, WorkerError> {
    let mut registration = Registration {
        // template:begin jobs:worker-bootstrap-register-jobs
        jobs: Kinds::new(),
        // template:end jobs:worker-bootstrap-register-jobs
        // template:begin messaging:worker-bootstrap-register-messaging
        messages: MessagingRegistry::new([])?,
        // template:end messaging:worker-bootstrap-register-messaging
        config,
        tracker,
        cancel,
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
    let messages = if registration.messages.is_empty() {
        None
    } else {
        config.messaging.validate_consumer(&config.app.env)?;
        registration.messages.validate_consumer()?;
        Some(registration.messages)
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

fn spawn_metrics_tasks(metrics: &Metrics, cancel: &CancellationToken, tracker: &TaskTracker) {
    tracker.spawn(metrics.clone().upkeep(cancel.child_token()));
    tracker.spawn(runtime_metrics(
        METRICS_MAINTENANCE_INTERVAL,
        cancel.child_token(),
    ));
}

// template:begin jobs:worker-bootstrap-open-pool
async fn open_pool(
    config: &Config,
    cancel: &CancellationToken,
    tracker: &TaskTracker,
    resources: &mut Resources,
) -> Result<PgPool, WorkerError> {
    let dsn = Dsn::admit_with(
        config.postgres.required_dsn()?.expose_secret(),
        config.postgres.password_file.as_deref(),
    )?;
    let application_name = application_name(&config.observability.otel.service_name);
    let pool = infra_postgres::connect(
        &dsn,
        &PoolOptions {
            max_connections: config.postgres.max_connections,
            application_name: &application_name,
            default_isolation: infra_postgres::Isolation::ReadCommitted,
            session_budgets: match config.postgres.session_budgets {
                service_config::PostgresSessionBudgets::Startup => SessionBudgets::Startup,
                service_config::PostgresSessionBudgets::Server => SessionBudgets::Server,
            },
        },
    )
    .await?;
    resources.pool = Some(pool.clone());
    tracing::info!(
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        postgres.max_connections = config.postgres.max_connections.get(),
        "postgres_pool_opened"
    );
    tracker.spawn(infra_postgres::record_metrics_periodically(
        pool.clone(),
        METRICS_MAINTENANCE_INTERVAL,
        cancel.child_token(),
    ));
    // Ends at once unless `postgres.password_file` is set.
    tracker.spawn(infra_postgres::refresh_password_periodically(
        pool.clone(),
        dsn,
        cancel.child_token(),
    ));
    Ok(pool)
}
// template:end jobs:worker-bootstrap-open-pool

async fn bind_listeners(
    config: &Config,
    metrics: &Metrics,
    resources: &mut Resources,
) -> Result<Readiness, WorkerError> {
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
    let readiness = Readiness::new(probes, refresh_policy(config));
    let options = server_options(config);
    let routes = infra_http::finalize_public(infra_http::router())?.with_state(readiness.reader());
    let app = infra_http::harden(routes, &harden_options(config));
    let health = Server::bind(config.http.addr, app, options).await?;
    tracing::info!(addr = %health.local_addr(), "http listener bound");
    resources.listeners.health = Some(health);
    if let Some(addr) = config.observability.metrics.addr {
        let diagnostics = Server::bind(addr, diagnostics_router(metrics.clone()), options).await?;
        tracing::info!(addr = %diagnostics.local_addr(), "diagnostics listener bound");
        resources.listeners.diagnostics = Some(diagnostics);
    }
    Ok(readiness)
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
        root_ca_path: messaging.root_ca_path.clone(),
        allow_plaintext: messaging.allow_plaintext,
        source_stream,
        dlq_stream: None,
        max_payload_bytes: usize::try_from(messaging.max_payload_bytes.as_u64())
            .map_err(|_| WorkerError::MessagingPayloadBound)?,
        consumer,
    })
}
// template:end messaging:worker-bootstrap-messaging-options

async fn admit(signals: &mut Signals, readiness: &Readiness) -> Result<bool, WorkerError> {
    let verdict = tokio::select! {
        biased;
        () = signals.wait() => None,
        () = readiness.refresh() => Some(readiness.reader().verdict()),
    };
    match verdict {
        None => Ok(false),
        Some(Ok(())) => Ok(true),
        Some(Err(reason)) => Err(WorkerError::Admission(reason)),
    }
}

fn spawn_refresher(readiness: &Readiness, cancel: &CancellationToken, tracker: &TaskTracker) {
    let readiness = readiness.clone();
    let cancel = cancel.child_token();
    tracker.spawn(async move { readiness.refresh_until(cancel).await });
}

/// `Ended::Signal` when a stop signal ended the wait. A terminal jobs or
/// messaging failure names its owner and takes the error exit after ordered
/// cleanup.
async fn wait_for_stop(resources: &Resources, signals: &mut Signals) -> Ended {
    tokio::select! {
        biased;
        () = signals.wait() => Ended::Signal,
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

    // template:begin jobs:worker-bootstrap-test-jobs-imports
    use super::application_name;
    use infra_jobs::{KindError, StartupError};
    // template:end jobs:worker-bootstrap-test-jobs-imports

    use super::{Config, WorkerError, check_preconditions};

    #[test]
    fn preconditions_only_reserve_the_process_grace_budget() {
        let mut config = Config::default();
        config.http.grace_period = Duration::from_secs(30);
        assert!(matches!(
            check_preconditions(&config),
            Err(WorkerError::GraceBudget(_))
        ));

        config.http.grace_period = Duration::from_secs(42);
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
            "http.grace_period (30s) must be >= http.drain_timeout (25s) plus the 17s jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush)"
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
