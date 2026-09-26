//! The worker's asynchronous startup (steps 8-19 of the startup order in
//! docs/architecture/runtime-lifecycle.md), its refusals, the health
//! listener, and readiness.
//!
//! Steps 1-10 do no database I/O. Every refusal after the runtime started
//! goes through `shutdown::abort_startup`.

use std::time::Duration;

use health::{Probe, Readiness, RefreshPolicy};
use infra_http::{HTTP_REQUESTS_DURATION_SECONDS, HardenOptions, Server, ServerOptions};
// template:begin jobs:worker-bootstrap-jobs-imports
use infra_jobs::{
    ATTEMPT_DURATION_BUCKETS, ATTEMPT_DURATION_METRIC, Engine, Kinds, Registry, Started,
};
// template:end jobs:worker-bootstrap-jobs-imports
// template:begin messaging:worker-bootstrap-messaging-imports
use infra_messaging::{
    ConsumerHandle, ConsumerOptions, Messaging, MessagingError, MessagingOptions,
    Registry as MessagingRegistry,
};
// template:end messaging:worker-bootstrap-messaging-imports
// template:begin jobs:worker-bootstrap-postgres-imports
use infra_postgres::{Dsn, PgPool, PoolOptions, PostgresProbe};
// template:end jobs:worker-bootstrap-postgres-imports
use infra_telemetry::{
    ExporterState, LoggingFormat, LoggingOptions, Metrics, TracerProviderHandle, TracingOptions,
    diagnostics_router, install_subscriber, install_tracer_provider,
};
use secrecy::ExposeSecret;
use service_config::{AppConfig, Config, LogFormat, TracesSampler};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::shutdown::{self, Listeners, Signals};
use crate::{BuildError, Support};

const METRICS_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(10);
// template:begin messaging:worker-bootstrap-messaging-startup-budget
const MESSAGING_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
// template:end messaging:worker-bootstrap-messaging-startup-budget

/// Every startup refusal, in order, plus the engine stopping without a stop signal.
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
    #[error("the job engine stopped without a stop signal")]
    EngineStopped,
}

/// Validate process grace before building the runtime. Capability-specific
/// pool and broker admission follows registration and precedes dependency I/O.
pub(crate) fn check_preconditions(config: &Config) -> Result<(), WorkerError> {
    shutdown::validate_grace_budget(&config.http)?;
    Ok(())
}

/// What startup has opened so far, which `abort_startup` tears down.
#[derive(Default)]
struct Opened {
    // template:begin jobs:worker-bootstrap-opened-jobs
    pool: Option<PgPool>,
    // template:end jobs:worker-bootstrap-opened-jobs
    // template:begin messaging:worker-bootstrap-opened-messaging
    messaging: Option<Messaging>,
    consumer: Option<ConsumerHandle>,
    // template:end messaging:worker-bootstrap-opened-messaging
    listeners: Listeners,
    // template:begin jobs:worker-bootstrap-opened-started
    started: Vec<Started>,
    // template:end jobs:worker-bootstrap-opened-started
}

/// What steps 9-17 hand back when startup was not refused. `admitted` is
/// false when a stop signal ended startup before readiness admission passed.
struct Prepared {
    tracer_provider: TracerProviderHandle,
    readiness: Readiness,
    policy: RefreshPolicy,
    // template:begin jobs:worker-bootstrap-prepared-jobs
    pool: Option<PgPool>,
    // template:end jobs:worker-bootstrap-prepared-jobs
    admitted: bool,
}

/// Steps 8-19. Every refusal after the runtime started, including a failed
/// signal-handler install, goes through `shutdown::abort_startup` exactly once
/// before it is returned. A stop signal or an engine failure runs the staged
/// shutdown plan.
pub(crate) async fn serve(
    config: Config,
    register: crate::Register,
) -> Result<shutdown::Outcome, WorkerError> {
    let cancel = CancellationToken::new();
    let tracker = TaskTracker::new();
    let mut opened = Opened::default();
    let mut signals = match Signals::install() {
        Ok(signals) => signals,
        Err(err) => {
            shutdown::abort_startup(
                // template:begin jobs:worker-bootstrap-signal-abort-started
                &[],
                // template:end jobs:worker-bootstrap-signal-abort-started
                // template:begin messaging:worker-bootstrap-signal-abort-consumer
                None,
                // template:end messaging:worker-bootstrap-signal-abort-consumer
                Listeners::default(),
                &cancel,
                &tracker,
                // template:begin jobs:worker-bootstrap-signal-abort-pool
                None,
                // template:end jobs:worker-bootstrap-signal-abort-pool
                // template:begin messaging:worker-bootstrap-signal-abort-messaging
                None,
                // template:end messaging:worker-bootstrap-signal-abort-messaging
            )
            .await;
            return Err(WorkerError::Signals(err));
        }
    };
    let prepared = match Box::pin(prepare(
        &config,
        register,
        &mut signals,
        &cancel,
        &tracker,
        &mut opened,
    ))
    .await
    {
        Ok(prepared) => prepared,
        Err(err) => {
            // template:begin jobs:worker-bootstrap-error-started
            let started = std::mem::take(&mut opened.started);
            // template:end jobs:worker-bootstrap-error-started
            let listeners = std::mem::take(&mut opened.listeners);
            shutdown::abort_startup(
                // template:begin jobs:worker-bootstrap-error-abort-started
                &started,
                // template:end jobs:worker-bootstrap-error-abort-started
                // template:begin messaging:worker-bootstrap-error-abort-consumer
                opened.consumer.take(),
                // template:end messaging:worker-bootstrap-error-abort-consumer
                listeners,
                &cancel,
                &tracker,
                // template:begin jobs:worker-bootstrap-error-abort-pool
                opened.pool.as_ref(),
                // template:end jobs:worker-bootstrap-error-abort-pool
                // template:begin messaging:worker-bootstrap-error-abort-messaging
                opened.messaging.take(),
                // template:end messaging:worker-bootstrap-error-abort-messaging
            )
            .await;
            return Err(err);
        }
    };
    let stop_signal = if prepared.admitted {
        spawn_refresher(&prepared.readiness, prepared.policy, &cancel, &tracker);
        tracing::info!("jobs_worker_ready");
        wait_for_stop(
            // template:begin jobs:worker-bootstrap-wait-started-argument
            &opened.started,
            // template:end jobs:worker-bootstrap-wait-started-argument
            // template:begin messaging:worker-bootstrap-wait-consumer-argument
            opened.consumer.as_ref(),
            // template:end messaging:worker-bootstrap-wait-consumer-argument
            &mut signals,
        )
        .await
    } else {
        true
    };
    // template:begin jobs:worker-bootstrap-shutdown-started
    let started = std::mem::take(&mut opened.started);
    // template:end jobs:worker-bootstrap-shutdown-started
    let listeners = std::mem::take(&mut opened.listeners);
    let tracer_provider = prepared.tracer_provider;
    // template:begin jobs:worker-bootstrap-shutdown-pool
    let pool = prepared.pool;
    // template:end jobs:worker-bootstrap-shutdown-pool
    let outcome = shutdown::run(shutdown::Plan {
        http: &config.http,
        readiness: &prepared.readiness,
        // template:begin jobs:worker-bootstrap-shutdown-plan-started
        started: &started,
        // template:end jobs:worker-bootstrap-shutdown-plan-started
        listeners,
        cancel,
        tracker,
        // template:begin jobs:worker-bootstrap-shutdown-plan-pool
        pool,
        // template:end jobs:worker-bootstrap-shutdown-plan-pool
        // template:begin messaging:worker-bootstrap-shutdown-plan-messaging
        consumer: opened.consumer.take(),
        messaging: opened.messaging.take(),
        // template:end messaging:worker-bootstrap-shutdown-plan-messaging
        tracer_provider,
        signals: &mut signals,
    })
    .await;
    if stop_signal {
        Ok(outcome)
    } else {
        Err(WorkerError::EngineStopped)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "keep pool and broker admission with startup resource ownership in one ordered sequence"
)]
async fn prepare(
    config: &Config,
    register: crate::Register,
    signals: &mut Signals,
    cancel: &CancellationToken,
    tracker: &TaskTracker,
    opened: &mut Opened,
) -> Result<Prepared, WorkerError> {
    let identity = worker_identity(&config.observability.otel.service_name);
    let (tracer_provider, metrics) = install_observability(config, &identity)?;
    // template:begin messaging:worker-bootstrap-sanitized-panic-hook-call
    install_sanitized_panic_hook();
    // template:end messaging:worker-bootstrap-sanitized-panic-hook-call
    let registrations = register_capabilities(config, register, cancel, tracker)?;
    log_startup_record(
        config,
        &identity,
        // template:begin jobs:worker-bootstrap-log-jobs-argument
        registrations.jobs.as_ref(),
        // template:end jobs:worker-bootstrap-log-jobs-argument
        &tracer_provider.exporter_state,
    );
    spawn_metrics_tasks(&metrics, cancel, tracker);
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
    let mut engines = Vec::new();
    let pool = if needs_pool {
        if !config.postgres.enabled {
            return Err(WorkerError::PostgresDisabled);
        }
        #[allow(
            unused_variables,
            reason = "retained outbox supplies the combined admission"
        )]
        let capacity = || config.jobs.required_connections(&config.postgres);
        // template:end jobs:worker-bootstrap-pool-admission
        // template:begin outbox:worker-bootstrap-outbox-capacity
        let capacity = || {
            config
                .jobs
                .required_connections_with_outbox(&config.postgres, registrations.jobs.is_some())
        };
        // template:end outbox:worker-bootstrap-outbox-capacity
        // template:begin jobs:worker-bootstrap-pool-capacity
        capacity()?;
        // template:end jobs:worker-bootstrap-pool-capacity
        // template:begin outbox:worker-bootstrap-outbox-config
        config.messaging.validate_producer(&config.app.env)?;
        // template:end outbox:worker-bootstrap-outbox-config
        // template:begin jobs:worker-bootstrap-pool-open
        let pool = open_pool(config, cancel, tracker, opened).await?;
        migrate::verify_history(&pool).await?;
        Some(pool)
    } else {
        None
    };
    // template:end jobs:worker-bootstrap-pool-open
    #[allow(
        unused_variables,
        reason = "retained messaging shadows the signal result"
    )]
    let startup_stopped = false;
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
    let (consumer, startup_stopped) = if needs_messaging {
        let options = messaging_options(config, registrations.messages.is_some())?;
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
            opened.messaging = connected.ok();
            (None, true)
        } else {
            opened.messaging = Some(connected?);
            let messaging = opened
                .messaging
                .as_ref()
                .ok_or(MessagingError::Connection)?;
            if let Some(registry) = registrations.messages {
                let admit_consumer = messaging.consumer(registry);
                tokio::pin!(admit_consumer);
                tokio::select! {
                    biased;
                    () = signals.wait() => {
                        startup_cancel.cancel();
                        let _ = admit_consumer.await;
                        (None, true)
                    }
                    consumer = &mut admit_consumer => (Some(consumer?), false),
                }
            } else {
                (None, false)
            }
        }
    } else {
        (None, false)
    };
    // template:end messaging:worker-bootstrap-messaging-connect
    // template:begin outbox:worker-bootstrap-outbox-engine
    let publisher = if startup_stopped {
        None
    } else {
        let messaging = opened
            .messaging
            .as_ref()
            .ok_or(MessagingError::Connection)?;
        let registry = infra_messaging::outbox::registry(messaging.producer())?;
        if registrations.jobs.as_ref().is_some_and(|ordinary| {
            registry
                .names()
                .any(|reserved| ordinary.names().any(|name| name == reserved))
        }) {
            return Err(WorkerError::PublisherKindConflict);
        }
        let publisher = Engine::new(
            pool.as_ref().ok_or(WorkerError::PostgresDisabled)?.clone(),
            registry,
            std::num::NonZeroU32::MIN,
        );
        publisher.check_startup().await?;
        Some(publisher)
    };
    // template:end outbox:worker-bootstrap-outbox-engine
    // template:begin jobs:worker-bootstrap-ordinary-engine
    if !startup_stopped && let Some(registry) = registrations.jobs {
        let engine = Engine::new(
            pool.as_ref().ok_or(WorkerError::PostgresDisabled)?.clone(),
            registry,
            config.jobs.max_workers()?,
        );
        engine.check_startup().await?;
        engines.push(engine);
    }
    // template:end jobs:worker-bootstrap-ordinary-engine
    // template:begin outbox:worker-bootstrap-append-publisher
    if let Some(publisher) = publisher {
        engines.push(publisher);
    }
    // template:end outbox:worker-bootstrap-append-publisher
    // template:begin messaging:worker-bootstrap-messaging-probe-value
    let messaging_probe = opened.messaging.as_ref().map(Messaging::probe);
    // template:end messaging:worker-bootstrap-messaging-probe-value
    let (readiness, policy) = if startup_stopped {
        (
            Readiness::new(Vec::new()),
            RefreshPolicy {
                interval: config.health.refresh_interval,
                probe_budget: config.health.probe_budget,
                failure_threshold: config.health.failure_threshold,
            },
        )
    } else {
        bind_listeners(
            config,
            &metrics,
            opened,
            // template:begin jobs:worker-bootstrap-listener-jobs-probe-argument
            pool.as_ref(),
            // template:end jobs:worker-bootstrap-listener-jobs-probe-argument
            // template:begin messaging:worker-bootstrap-listener-messaging-probe-argument
            messaging_probe,
            // template:end messaging:worker-bootstrap-listener-messaging-probe-argument
        )
        .await?
    };
    let admitted = if startup_stopped || signals.pending() {
        false
    } else {
        admit(signals, &readiness, policy).await?
    };
    // template:begin jobs:worker-bootstrap-start-admitted-jobs
    if admitted {
        opened.started = engines
            .iter()
            .map(|engine| engine.start(tracker, cancel))
            .collect();
        tracing::info!(engines = opened.started.len(), "jobs_claiming_started");
    }
    // template:end jobs:worker-bootstrap-start-admitted-jobs
    // template:begin messaging:worker-bootstrap-start-admitted-consumer
    if admitted && let Some(consumer) = consumer {
        opened.consumer = Some(consumer.start(cancel));
        tracing::info!("messaging_consuming_started");
    }
    // template:end messaging:worker-bootstrap-start-admitted-consumer
    Ok(Prepared {
        tracer_provider,
        readiness,
        policy,
        // template:begin jobs:worker-bootstrap-prepared-jobs-value
        pool,
        // template:end jobs:worker-bootstrap-prepared-jobs-value
        admitted,
    })
}

// template:begin messaging:worker-bootstrap-sanitized-panic-hook
/// The consumer treats a handler panic as a terminal worker fault. Replace
/// Rust's default hook so caller-controlled panic text never reaches logs
/// before that typed failure reaches the lifecycle owner.
fn install_sanitized_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map_or("<unknown>", |location| location.file());
        tracing::error!(panic.location = location, "background task panicked");
    }));
}
// template:end messaging:worker-bootstrap-sanitized-panic-hook

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
        service_name: identity,
    })?;
    let metrics = Metrics::install_with_histograms(
        HTTP_REQUESTS_DURATION_SECONDS,
        &[
            // template:begin jobs:worker-bootstrap-jobs-histograms
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
        ],
    )?;
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
    register: crate::Register,
    cancel: &CancellationToken,
    tracker: &TaskTracker,
) -> Result<Registrations, WorkerError> {
    // template:begin jobs:worker-bootstrap-register-jobs
    let mut kinds = Kinds::new();
    // template:end jobs:worker-bootstrap-register-jobs
    // template:begin messaging:worker-bootstrap-register-messaging
    let mut messages = MessagingRegistry::new([])?;
    // template:end messaging:worker-bootstrap-register-messaging
    register(
        // template:begin jobs:worker-bootstrap-register-jobs-argument
        &mut kinds,
        // template:end jobs:worker-bootstrap-register-jobs-argument
        // template:begin messaging:worker-bootstrap-register-messaging-argument
        &mut messages,
        // template:end messaging:worker-bootstrap-register-messaging-argument
        &Support {
            config,
            tracker,
            cancel,
        },
    )
    .map_err(WorkerError::Registration)?;
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
    let jobs = match kinds.validate() {
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
    let messages = if messages.is_empty() {
        None
    } else {
        config.messaging.validate_consumer(&config.app.env)?;
        messages.validate_consumer()?;
        Some(messages)
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
    tracker.spawn(
        metrics
            .clone()
            .upkeep(METRICS_MAINTENANCE_INTERVAL, cancel.child_token()),
    );
    tracker.spawn(Metrics::runtime_metrics(
        METRICS_MAINTENANCE_INTERVAL,
        cancel.child_token(),
    ));
}

// template:begin jobs:worker-bootstrap-open-pool
async fn open_pool(
    config: &Config,
    cancel: &CancellationToken,
    tracker: &TaskTracker,
    opened: &mut Opened,
) -> Result<PgPool, WorkerError> {
    let dsn = Dsn::admit(config.postgres.required_dsn()?.expose_secret())?;
    let application_name = application_name(&config.observability.otel.service_name);
    let pool = infra_postgres::connect(
        &dsn,
        &PoolOptions {
            max_connections: config.postgres.pool_max_connections()?,
            application_name: &application_name,
            default_isolation: infra_postgres::Isolation::ReadCommitted,
        },
    )
    .await?;
    opened.pool = Some(pool.clone());
    tracing::info!(
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        postgres.max_connections = config.postgres.max_connections,
        "postgres_pool_opened"
    );
    tracker.spawn(infra_postgres::record_metrics_periodically(
        pool.clone(),
        METRICS_MAINTENANCE_INTERVAL,
        cancel.child_token(),
    ));
    Ok(pool)
}
// template:end jobs:worker-bootstrap-open-pool

async fn bind_listeners(
    config: &Config,
    metrics: &Metrics,
    opened: &mut Opened,
    // template:begin jobs:worker-bootstrap-listener-jobs-parameter
    pool: Option<&PgPool>,
    // template:end jobs:worker-bootstrap-listener-jobs-parameter
    // template:begin messaging:worker-bootstrap-listener-messaging-parameter
    messaging_probe: Option<infra_messaging::MessagingProbe>,
    // template:end messaging:worker-bootstrap-listener-messaging-parameter
) -> Result<(Readiness, RefreshPolicy), WorkerError> {
    let mut probes: Vec<Box<dyn Probe>> = Vec::new();
    // template:begin jobs:worker-bootstrap-listener-jobs-probe
    if let Some(pool) = pool {
        probes.push(Box::new(PostgresProbe::new(pool.clone())));
    }
    // template:end jobs:worker-bootstrap-listener-jobs-probe
    // template:begin messaging:worker-bootstrap-listener-messaging-probe
    if let Some(probe) = messaging_probe {
        probes.push(Box::new(probe));
    }
    // template:end messaging:worker-bootstrap-listener-messaging-probe
    let readiness = Readiness::new(probes);
    let policy = RefreshPolicy {
        interval: config.health.refresh_interval,
        probe_budget: config.health.probe_budget,
        failure_threshold: config.health.failure_threshold,
    };
    let options = server_options(config);
    let routes = infra_http::finalize_public(infra_http::router())?.with_state(readiness.reader());
    let app = infra_http::harden(routes, &harden_options(config));
    let health = Server::bind(config.http.listen_addr()?, app, options).await?;
    tracing::info!(addr = %health.local_addr(), "http listener bound");
    opened.listeners.health = Some(health);
    if let Some(addr) = config.observability.metrics.listen_addr()? {
        let diagnostics = Server::bind(addr, diagnostics_router(metrics.clone()), options).await?;
        tracing::info!(addr = %diagnostics.local_addr(), "diagnostics listener bound");
        opened.listeners.diagnostics = Some(diagnostics);
    }
    Ok((readiness, policy))
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
            concurrency: usize::try_from(messaging.consumer_concurrency)
                .map_err(|_| WorkerError::MessagingConcurrency)?,
        })
    } else {
        None
    };
    Ok(MessagingOptions {
        servers: messaging.urls.clone(),
        credentials: messaging
            .credentials
            .as_ref()
            .map(|value| value.expose_secret().to_owned()),
        root_ca_path: messaging.root_ca_path.clone(),
        allow_plaintext: messaging.allow_plaintext,
        allow_unauthenticated: messaging.allow_unauthenticated,
        source_stream,
        dlq_stream: None,
        max_payload_bytes: usize::try_from(messaging.max_payload_bytes.as_u64())
            .map_err(|_| WorkerError::MessagingPayloadBound)?,
        consumer,
    })
}
// template:end messaging:worker-bootstrap-messaging-options

async fn admit(
    signals: &mut Signals,
    readiness: &Readiness,
    policy: RefreshPolicy,
) -> Result<bool, WorkerError> {
    let verdict = tokio::select! {
        biased;
        () = signals.wait() => None,
        () = readiness.refresh(policy) => Some(readiness.reader().verdict()),
    };
    match verdict {
        None => Ok(false),
        Some(Ok(())) => Ok(true),
        Some(Err(reason)) => Err(WorkerError::Admission(reason)),
    }
}

fn spawn_refresher(
    readiness: &Readiness,
    policy: RefreshPolicy,
    cancel: &CancellationToken,
    tracker: &TaskTracker,
) {
    let readiness = readiness.clone();
    let cancel = cancel.child_token();
    tracker.spawn(async move { readiness.refresh_until(policy, cancel).await });
}

/// `true` when a stop signal ended the wait. A terminal jobs or messaging
/// failure takes the existing error exit after ordered cleanup.
async fn wait_for_stop(
    // template:begin jobs:worker-bootstrap-wait-started-parameter
    started: &[Started],
    // template:end jobs:worker-bootstrap-wait-started-parameter
    // template:begin messaging:worker-bootstrap-wait-consumer-parameter
    consumer: Option<&ConsumerHandle>,
    // template:end messaging:worker-bootstrap-wait-consumer-parameter
    signals: &mut Signals,
) -> bool {
    tokio::select! {
        biased;
        () = signals.wait() => true,
        // template:begin jobs:worker-bootstrap-wait-jobs-failure
        () = async {
            if started.is_empty() {
                std::future::pending::<()>().await;
            } else {
                futures_util::future::select_all(
                    started.iter().map(|engine| Box::pin(engine.failed())),
                ).await;
            }
        } => false,
        // template:end jobs:worker-bootstrap-wait-jobs-failure
        // template:begin messaging:worker-bootstrap-wait-messaging-failure
        error = async {
            if let Some(consumer) = consumer {
                consumer.failed().await
            } else {
                std::future::pending::<infra_messaging::ConsumerError>().await
            }
        } => {
            tracing::error!(error = %error, "messaging consumer stopped");
            false
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
fn application_name(service_name: &str) -> String {
    let end = service_name.floor_char_boundary(51);
    let prefix = service_name.get(..end).unwrap_or("");
    format!("{prefix}-jobs-worker")
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
    log_exporter(exporter);
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
        observability.metrics.addr = %config.observability.metrics.addr,
        // template:begin jobs:worker-bootstrap-log-jobs-fields
        postgres.max_connections = config.postgres.max_connections,
        jobs.max_workers = config.jobs.max_workers,
        jobs.kinds = %kinds,
        // template:end jobs:worker-bootstrap-log-jobs-fields
        log.level = %config.log.level,
        tracing.exporter = exporter.as_str(),
        "jobs_worker_starting"
    );
}

fn log_exporter(exporter: &ExporterState) {
    match exporter {
        ExporterState::Degraded { reason } => tracing::warn!(
            reason = %reason,
            "trace exporter degraded; spans are recorded but not exported"
        ),
        ExporterState::Initialized { endpoint_source } => {
            tracing::info!(
                endpoint_source = endpoint_source.as_str(),
                "trace exporter initialized"
            );
        }
        ExporterState::Disabled => {}
    }
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
            WorkerError::Kinds(KindError::NoKinds).to_string(),
            "job kinds are invalid: no job kind is registered"
        );
        assert_eq!(
            WorkerError::JobsStartup(StartupError::Unavailable).to_string(),
            "jobs startup check: the jobs store is unavailable"
        );
        // template:end jobs:worker-bootstrap-test-jobs-refusals
    }

    // template:begin jobs:worker-bootstrap-test-application-name
    #[test]
    fn identity_keeps_the_suffix_inside_the_postgres_limit() {
        assert_eq!(super::worker_identity("service"), "service-jobs-worker");
        assert_eq!(application_name("service"), "service-jobs-worker");

        let ascii_64 = "a".repeat(64);
        let cut = application_name(&ascii_64);
        assert_eq!(cut, format!("{}-jobs-worker", "a".repeat(51)));
        assert_eq!(cut.len(), 63);

        let ascii_51 = "a".repeat(51);
        let whole = application_name(&ascii_51);
        assert_eq!(whole, format!("{ascii_51}-jobs-worker"));
        assert_eq!(whole.len(), 63);

        let inside = format!("{}é", "a".repeat(50));
        assert_eq!(inside.len(), 52);
        let cut_char = application_name(&inside);
        assert_eq!(cut_char, format!("{}-jobs-worker", "a".repeat(50)));
        assert_eq!(cut_char.len(), 62);

        let exact = format!("{}é", "a".repeat(49));
        assert_eq!(exact.len(), 51);
        let kept = application_name(&exact);
        assert_eq!(kept, format!("{exact}-jobs-worker"));
        assert_eq!(kept.len(), 63);

        let wide = "あ".repeat(20);
        assert_eq!(wide.len(), 60);
        let wide_name = application_name(&wide);
        assert_eq!(wide_name.len(), 63);
        assert!(wide_name.starts_with(&wide[..51]));

        for name in [
            application_name("service"),
            cut,
            whole,
            cut_char,
            kept,
            wide_name,
        ] {
            assert!(name.ends_with("-jobs-worker"), "{name}");
            assert!(name.len() <= 63, "{}", name.len());
        }
    }
    // template:end jobs:worker-bootstrap-test-application-name
}
