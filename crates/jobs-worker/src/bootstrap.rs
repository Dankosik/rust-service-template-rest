//! The worker's asynchronous startup (steps 8-19 of the startup order in
//! docs/architecture/runtime-lifecycle.md), its refusals, the health
//! listener, and readiness.
//!
//! Steps 1-10 do no database I/O. Every refusal after the runtime started
//! goes through `shutdown::abort_startup`.

use std::time::Duration;

use health::{Probe, Readiness, RefreshPolicy};
use infra_http::{HTTP_REQUESTS_DURATION_SECONDS, HardenOptions, Server, ServerOptions};
use infra_jobs::{
    ATTEMPT_DURATION_BUCKETS, ATTEMPT_DURATION_METRIC, Engine, Kinds, Registry, Started,
};
use infra_postgres::{Dsn, PgPool, PoolOptions, PostgresProbe};
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

/// Every startup refusal, in order, plus the engine stopping without a stop signal.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkerError {
    #[error(
        "no job kind is registered: register this service's job kinds in crates/jobs-worker/src/main.rs"
    )]
    NoKinds,
    #[error(transparent)]
    Load(#[from] service_config::Error),
    #[error("postgres.enabled must be true to run the jobs worker")]
    PostgresDisabled,
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
    #[error("job kinds are invalid: {0}")]
    Kinds(#[from] infra_jobs::KindError),
    #[error("configuration is invalid: postgres.dsn: {0}")]
    PostgresDsn(#[from] infra_postgres::DsnError),
    #[error(transparent)]
    Postgres(#[from] infra_postgres::ConnectError),
    #[error("postgres migration history: {0}")]
    PostgresHistory(#[from] migrate::HistoryError),
    #[error("jobs startup check: {0}")]
    JobsStartup(#[from] infra_jobs::StartupError),
    #[error(transparent)]
    Server(#[from] infra_http::ServerError),
    #[error(transparent)]
    HttpContract(#[from] infra_http::FinalizeError),
    #[error("startup admission: {0}")]
    Admission(health::NotReady),
    #[error("the job engine stopped without a stop signal")]
    EngineStopped,
}

/// Steps 4-6: PostgreSQL enabled, the pool bound, and the grace budget.
pub(crate) fn check_preconditions(config: &Config) -> Result<(), WorkerError> {
    if !config.postgres.enabled {
        return Err(WorkerError::PostgresDisabled);
    }
    config.jobs.required_connections(&config.postgres)?;
    shutdown::validate_grace_budget(&config.http)?;
    Ok(())
}

/// What startup has opened so far, which `abort_startup` tears down.
#[derive(Default)]
struct Opened {
    pool: Option<PgPool>,
    listeners: Listeners,
    started: Option<Started>,
}

/// What steps 9-17 hand back when startup was not refused. `admitted` is
/// false when a stop signal ended startup before readiness admission passed.
struct Prepared {
    tracer_provider: TracerProviderHandle,
    readiness: Readiness,
    policy: RefreshPolicy,
    pool: PgPool,
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
            shutdown::abort_startup(None, Listeners::default(), &cancel, &tracker, None).await;
            return Err(WorkerError::Signals(err));
        }
    };
    let prepared = match prepare(
        &config,
        register,
        &mut signals,
        &cancel,
        &tracker,
        &mut opened,
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(err) => {
            let started = opened.started.take();
            let listeners = std::mem::take(&mut opened.listeners);
            shutdown::abort_startup(
                started.as_ref(),
                listeners,
                &cancel,
                &tracker,
                opened.pool.as_ref(),
            )
            .await;
            return Err(err);
        }
    };
    let stop_signal = if prepared.admitted {
        spawn_refresher(&prepared.readiness, prepared.policy, &cancel, &tracker);
        tracing::info!("jobs_worker_ready");
        wait_for_stop(opened.started.as_ref(), &mut signals).await
    } else {
        true
    };
    let started = opened.started.take();
    let listeners = std::mem::take(&mut opened.listeners);
    let tracer_provider = prepared.tracer_provider;
    let pool = prepared.pool;
    let outcome = shutdown::run(shutdown::Plan {
        http: &config.http,
        readiness: &prepared.readiness,
        started: started.as_ref(),
        listeners,
        cancel,
        tracker,
        pool,
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
    let registry = register_kinds(config, register, cancel, tracker)?;
    log_startup_record(
        config,
        &identity,
        &registry,
        &tracer_provider.exporter_state,
    );
    spawn_metrics_tasks(&metrics, cancel, tracker);
    let pool = open_pool(config, cancel, tracker, opened).await?;
    migrate::verify_history(&pool).await?;
    let engine = Engine::new(pool.clone(), registry, config.jobs.max_workers()?);
    engine.check_startup().await?;
    let (readiness, policy) = bind_listeners(config, &pool, &metrics, opened).await?;
    let admitted = if signals.pending() {
        false
    } else {
        opened.started = Some(engine.start(tracker, cancel));
        tracing::info!("jobs_claiming_started");
        admit(signals, &readiness, policy).await?
    };
    Ok(Prepared {
        tracer_provider,
        readiness,
        policy,
        pool,
        admitted,
    })
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
        service_name: identity,
    })?;
    let metrics = Metrics::install_with_histograms(
        HTTP_REQUESTS_DURATION_SECONDS,
        &[
            (ATTEMPT_DURATION_METRIC, ATTEMPT_DURATION_BUCKETS),
            (
                infra_jobs::CLAIM_DURATION_METRIC,
                infra_jobs::CLAIM_DURATION_BUCKETS,
            ),
            (
                infra_jobs::QUEUE_WAIT_METRIC,
                infra_jobs::QUEUE_WAIT_BUCKETS,
            ),
        ],
    )?;
    metrics.record_trace_exporter_initialized(matches!(
        tracer_provider.exporter_state,
        ExporterState::Initialized { .. }
    ));
    Ok((tracer_provider, metrics))
}

fn register_kinds(
    config: &Config,
    register: crate::Register,
    cancel: &CancellationToken,
    tracker: &TaskTracker,
) -> Result<Registry, WorkerError> {
    let mut kinds = Kinds::new();
    register(
        &mut kinds,
        &Support {
            config,
            tracker,
            cancel,
        },
    )
    .map_err(WorkerError::Registration)?;
    kinds.validate().map_err(WorkerError::from)
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

async fn bind_listeners(
    config: &Config,
    pool: &PgPool,
    metrics: &Metrics,
    opened: &mut Opened,
) -> Result<(Readiness, RefreshPolicy), WorkerError> {
    let probes: Vec<Box<dyn Probe>> = vec![Box::new(PostgresProbe::new(pool.clone()))];
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

/// `true` when a stop signal ended the wait. With no engine, only a stop signal ends it.
async fn wait_for_stop(started: Option<&Started>, signals: &mut Signals) -> bool {
    let Some(started) = started else {
        signals.wait().await;
        return true;
    };
    tokio::select! {
        biased;
        () = signals.wait() => true,
        () = started.failed() => false,
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

fn application_name(service_name: &str) -> String {
    let end = service_name.floor_char_boundary(51);
    let prefix = service_name.get(..end).unwrap_or("");
    format!("{prefix}-jobs-worker")
}

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
    registry: &Registry,
    exporter: &ExporterState,
) {
    log_exporter(exporter);
    let kinds = registry.names().collect::<Vec<_>>().join(",");
    tracing::info!(
        service.name = %identity,
        app.env = %config.app.env,
        app.version = %config.app.version,
        app.commit = %config.app.commit,
        http.addr = %config.http.addr,
        http.drain_timeout = ?config.http.drain_timeout,
        http.grace_period = ?config.http.grace_period,
        observability.metrics.addr = %config.observability.metrics.addr,
        postgres.max_connections = config.postgres.max_connections,
        jobs.max_workers = config.jobs.max_workers,
        jobs.kinds = %kinds,
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

    use infra_jobs::{KindError, StartupError};

    use super::{Config, WorkerError, application_name, check_preconditions, worker_identity};

    #[test]
    fn preconditions_refuse_in_order() {
        let mut config = Config::default();
        config.postgres.enabled = false;
        config.postgres.max_connections = 2;
        config.http.grace_period = Duration::from_secs(30);
        assert!(matches!(
            check_preconditions(&config),
            Err(WorkerError::PostgresDisabled)
        ));

        config.postgres.enabled = true;
        let err = check_preconditions(&config).unwrap_err();
        assert!(
            matches!(err, WorkerError::Config(ref invalid) if invalid.key == "postgres.max_connections"),
            "{err}"
        );

        config.postgres.max_connections = 3;
        assert!(matches!(
            check_preconditions(&config),
            Err(WorkerError::GraceBudget(_))
        ));

        config.http.grace_period = Duration::from_secs(42);
        check_preconditions(&config).unwrap();

        config.jobs.max_workers = 8;
        config.postgres.max_connections = 9;
        let err = check_preconditions(&config).unwrap_err();
        assert!(err.to_string().contains("(10)"), "{err}");
    }

    #[test]
    fn refusal_texts_name_the_failed_check() {
        assert_eq!(
            WorkerError::NoKinds.to_string(),
            "no job kind is registered: register this service's job kinds in crates/jobs-worker/src/main.rs"
        );
        assert_eq!(
            WorkerError::PostgresDisabled.to_string(),
            "postgres.enabled must be true to run the jobs worker"
        );

        let mut config = Config::default();
        config.postgres.enabled = true;
        config.postgres.max_connections = 2;
        assert_eq!(
            check_preconditions(&config).unwrap_err().to_string(),
            "configuration is invalid: postgres.max_connections: must be at least jobs.max_workers + 2 (3) for the jobs worker"
        );

        config.postgres.max_connections = 3;
        config.http.grace_period = Duration::from_secs(30);
        assert_eq!(
            check_preconditions(&config).unwrap_err().to_string(),
            "http.grace_period (30s) must be >= http.drain_timeout (25s) plus the 17s jobs worker teardown tail (cleanup, listeners, background join, dependency close, telemetry flush)"
        );
        assert_eq!(
            WorkerError::Kinds(KindError::NoKinds).to_string(),
            "job kinds are invalid: no job kind is registered"
        );
        assert_eq!(
            WorkerError::JobsStartup(StartupError::Unavailable).to_string(),
            "jobs startup check: the jobs store is unavailable"
        );
    }

    #[test]
    fn identity_keeps_the_suffix_inside_the_postgres_limit() {
        assert_eq!(worker_identity("service"), "service-jobs-worker");
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
}
