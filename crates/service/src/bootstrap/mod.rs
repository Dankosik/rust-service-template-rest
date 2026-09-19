//! Composition root: configuration, telemetry, readiness, transports, and
//! process lifecycle.
//!
//! Startup order: flags → config → signal handlers → tracer provider →
//! subscriber → metrics recorder → background tasks → dependency pools →
//! readiness admission → HTTP listeners → ready. Shutdown order lives in
//! [`shutdown`]. Handlers and feature code never own this sequence.

mod shutdown;

use std::ffi::OsString;
use std::num::NonZeroU32;
use std::process::ExitCode;
use std::time::Duration;

use health::{Probe, Readiness, RefreshPolicy};
use infra_http::{HTTP_REQUESTS_DURATION_SECONDS, HardenOptions, Server, ServerOptions};
use infra_postgres::{Dsn, PgPool, PoolOptions, PostgresProbe};
use infra_telemetry::{
    ExporterState, LoggingOptions, Metrics, Sampler, TracingOptions, diagnostics_router,
    install_subscriber, install_tracer_provider,
};
use secrecy::ExposeSecret;
use service_config::{AppConfig, BuildInfo, Config, LoadOptions, LogFormat, ResolvedSampler};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use self::shutdown::{Outcome, Signals};

/// Version and revision stamped into this binary.
pub(crate) const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

/// Exit code when the process shut down but a teardown stage overran its
/// budget: the platform and the process test can tell it from a crash.
const EXIT_DEGRADED_SHUTDOWN: u8 = 3;

/// Bound for dropping whatever the runtime still owns after the ordered
/// teardown: HTTP connection tasks that outlived drain and `pool.close`,
/// and any blocking tracer-provider shutdown that outlived its budget.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// Interval for Prometheus histogram upkeep and Tokio runtime metrics.
const METRICS_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub(crate) enum BootstrapError {
    #[error("install stop signal handlers: {0}")]
    Signals(#[source] std::io::Error),
    #[error(transparent)]
    Tracing(#[from] infra_telemetry::TracingError),
    #[error(transparent)]
    Logging(#[from] infra_telemetry::LoggingError),
    #[error(transparent)]
    Metrics(#[from] infra_telemetry::MetricsError),
    #[error("startup admission: {0}")]
    Admission(health::NotReady),
    #[error("configuration is invalid: {0}")]
    Config(#[from] service_config::ValidationError),
    #[error("configuration is invalid: postgres.dsn: {0}")]
    PostgresDsn(#[from] infra_postgres::DsnError),
    #[error(transparent)]
    Postgres(#[from] infra_postgres::ConnectError),
    #[error(transparent)]
    Server(#[from] infra_http::ServerError),
}

/// Parse flags, load configuration, run the service, and map the result to
/// an exit code. Never calls `process::exit`, so destructors run. `--help`
/// exits 0; other clap errors exit 1. Version is not a loader flag.
pub(crate) fn run<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    let options = match LoadOptions::from_args(args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    let config = match service_config::load(&options, BUILD_INFO) {
        Ok(config) => config,
        Err(err) => return startup_failure(&err.to_string()),
    };
    if let Err(err) = shutdown::validate_grace_budget(&config.http) {
        return startup_failure(&err.to_string());
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return startup_failure(&format!("build tokio runtime: {err}")),
    };

    let outcome = runtime.block_on(serve(config));
    // Drops connection tasks that outlived the drain and any blocking work.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);

    match outcome {
        Ok(Outcome::Graceful) => ExitCode::SUCCESS,
        Ok(Outcome::Degraded) => ExitCode::from(EXIT_DEGRADED_SHUTDOWN),
        Err(err) => {
            // The subscriber may or may not be installed; report both ways.
            tracing::error!(error = %err, "startup failed");
            startup_failure(&err.to_string())
        }
    }
}

fn startup_failure(message: &str) -> ExitCode {
    #[allow(clippy::print_stderr)]
    {
        eprintln!("{message}");
    }
    ExitCode::FAILURE
}

async fn serve(config: Config) -> Result<Outcome, BootstrapError> {
    // Before this point SIGTERM has its default disposition and kills the
    // process; install the handlers first and keep them for the lifetime.
    let mut signals = Signals::install().map_err(BootstrapError::Signals)?;

    let tracer_provider =
        install_tracer_provider(&tracing_options(&config, replica_instance_id(&config.app)))?;
    install_subscriber(&LoggingOptions {
        level: config.log.level.clone(),
        format: match config.log.format {
            LogFormat::Json => infra_telemetry::LogFormat::Json,
            LogFormat::Text => infra_telemetry::LogFormat::Text,
        },
        tracer_provider: Some(&tracer_provider),
        service_name: &config.observability.otel.service_name,
    })?;
    let metrics = Metrics::install(HTTP_REQUESTS_DURATION_SECONDS)?;
    metrics.record_trace_exporter_initialized(matches!(
        tracer_provider.exporter_state,
        ExporterState::Initialized { .. }
    ));
    log_startup_summary(&config, &tracer_provider.exporter_state);

    let cancel = CancellationToken::new();
    let tracker = TaskTracker::new();
    tracker.spawn(
        metrics
            .clone()
            .upkeep(METRICS_MAINTENANCE_INTERVAL, cancel.child_token()),
    );
    tracker.spawn(Metrics::runtime_metrics(
        METRICS_MAINTENANCE_INTERVAL,
        cancel.child_token(),
    ));

    // Dependency pools open before admission so the first readiness
    // evaluation already includes them. The PostgreSQL profile is inert
    // unless selected; a selected profile whose database is unreachable
    // fails startup here rather than serving a readiness that never passes.
    let mut probes: Vec<Box<dyn Probe>> = Vec::new();
    let postgres_pool = if config.postgres.enabled {
        match open_postgres(&config).await {
            Ok(pool) => {
                probes.push(Box::new(PostgresProbe::new(pool.clone())));
                tracker.spawn(infra_postgres::record_metrics_periodically(
                    pool.clone(),
                    METRICS_MAINTENANCE_INTERVAL,
                    cancel.child_token(),
                ));
                Some(pool)
            }
            Err(err) => {
                shutdown::close_opened_dependencies(&cancel, &tracker, None).await;
                return Err(err);
            }
        }
    } else {
        None
    };

    // Admission runs even without probes so the first probe after bind
    // answers from an evaluation.
    let readiness = Readiness::new(probes);
    let policy = RefreshPolicy {
        interval: config.health.refresh_interval,
        probe_budget: config.health.probe_budget,
        failure_threshold: config.health.failure_threshold,
    };

    let outcome = admit_and_serve(Prepared {
        config: &config,
        signals: &mut signals,
        tracer_provider,
        metrics,
        cancel: cancel.clone(),
        tracker: tracker.clone(),
        readiness,
        policy,
        postgres_pool: postgres_pool.clone(),
    })
    .await;
    // Bind or admission failure: cancel and join tracked tasks, then close
    // the pool. `Server` only cancels accept. Dropping `TracerProviderHandle`
    // is not last-ref: the global SDK clone remains until process teardown.
    if outcome.is_err() {
        shutdown::close_opened_dependencies(&cancel, &tracker, postgres_pool.as_ref()).await;
    }
    outcome
}

async fn open_postgres(config: &Config) -> Result<PgPool, BootstrapError> {
    let dsn = Dsn::parse(config.postgres.dsn.expose_secret())?;
    let pool = infra_postgres::connect(
        &dsn,
        &PoolOptions {
            max_connections: config.postgres.pool_max_connections()?,
            application_name: &config.observability.otel.service_name,
        },
    )
    .await?;
    tracing::info!(
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        postgres.max_connections = config.postgres.max_connections,
        "postgres_pool_opened"
    );
    Ok(pool)
}

/// Runtime pieces built before listeners bind: admission, then serve.
struct Prepared<'a> {
    config: &'a Config,
    signals: &'a mut Signals,
    tracer_provider: infra_telemetry::TracerProviderHandle,
    metrics: Metrics,
    cancel: CancellationToken,
    tracker: TaskTracker,
    readiness: Readiness,
    policy: RefreshPolicy,
    postgres_pool: Option<PgPool>,
}

async fn admit_and_serve(prepared: Prepared<'_>) -> Result<Outcome, BootstrapError> {
    let Prepared {
        config,
        signals,
        tracer_provider,
        metrics,
        cancel,
        tracker,
        readiness,
        policy,
        postgres_pool,
    } = prepared;
    readiness.refresh(policy).await;
    readiness
        .reader()
        .verdict()
        .map_err(BootstrapError::Admission)?;
    tracker.spawn({
        let readiness = readiness.clone();
        let cancel = cancel.child_token();
        async move { readiness.refresh_until(policy, cancel).await }
    });

    let server_options = ServerOptions {
        header_read_timeout: config.http.header_read_timeout,
        max_header_bytes: usize::try_from(config.http.max_header_bytes.as_u64())
            .unwrap_or(usize::MAX),
        max_connections: NonZeroU32::new(config.http.max_connections),
    };
    // The routes and the committed OpenAPI document are the two halves of
    // one contract; only the routes are needed here.
    let (routes, _document) = service::api::contract().split_for_parts();
    let app = infra_http::harden(
        routes.with_state(readiness.reader()),
        &HardenOptions {
            max_body_bytes: usize::try_from(config.http.max_body_bytes.as_u64())
                .unwrap_or(usize::MAX),
            request_timeout: config.http.request_timeout,
            max_in_flight: NonZeroU32::new(config.http.max_in_flight),
            log_health_probes: config.http.access_log_health_probes,
        },
    );
    let api = Server::bind(config.http.listen_addr()?, app, server_options).await?;
    tracing::info!(addr = %api.local_addr(), "http listener bound");

    let diagnostics = match config.observability.metrics.listen_addr()? {
        None => None,
        Some(addr) => {
            let server =
                Server::bind(addr, diagnostics_router(metrics.clone()), server_options).await?;
            tracing::info!(addr = %server.local_addr(), "diagnostics listener bound");
            Some(server)
        }
    };

    tracing::info!("service_ready");
    signals.wait().await;

    Ok(shutdown::run(shutdown::Plan {
        http: &config.http,
        readiness: &readiness,
        api,
        diagnostics,
        cancel,
        tracker,
        postgres_pool,
        tracer_provider,
        signals,
    })
    .await)
}

fn replica_instance_id(app: &AppConfig) -> String {
    app.instance_id
        .clone()
        .unwrap_or_else(|| gethostname::gethostname().to_string_lossy().into_owned())
}

fn tracing_options(config: &Config, instance_id: String) -> TracingOptions {
    let otel = &config.observability.otel;
    let sampler = match otel.resolved_sampler() {
        ResolvedSampler::AlwaysOn => Sampler::AlwaysOn,
        ResolvedSampler::AlwaysOff => Sampler::AlwaysOff,
        ResolvedSampler::TraceIdRatio(ratio) => Sampler::TraceIdRatio(ratio),
        ResolvedSampler::ParentBasedTraceIdRatio(ratio) => Sampler::ParentBasedTraceIdRatio(ratio),
    };
    TracingOptions {
        service_name: otel.service_name.clone(),
        service_version: config.app.version.clone(),
        vcs_revision: config.app.commit.clone(),
        instance_id,
        deployment_environment: config.app.env.clone(),
        sampler,
        otlp_endpoint: otel.exporter.otlp_endpoint.clone(),
        otlp_headers: secrecy::SecretString::from(
            otel.exporter.otlp_headers.expose_secret().to_owned(),
        ),
    }
}

/// One record with the non-secret facts an operator needs on every boot.
fn log_startup_summary(config: &Config, exporter: &ExporterState) {
    match exporter {
        ExporterState::Degraded { reason } => tracing::warn!(
            reason = %reason,
            "trace exporter degraded; spans are recorded but not exported"
        ),
        ExporterState::Initialized { endpoint_source } => {
            tracing::info!(endpoint_source, "trace exporter initialized");
        }
        ExporterState::Disabled => {}
    }
    tracing::info!(
        app.env = %config.app.env,
        app.version = %config.app.version,
        app.commit = %config.app.commit,
        http.addr = %config.http.addr,
        http.request_timeout = ?config.http.request_timeout,
        http.shutdown_timeout = ?config.http.shutdown_timeout,
        http.grace_period = ?config.http.grace_period,
        observability.metrics.addr = %config.observability.metrics.addr,
        postgres.enabled = config.postgres.enabled,
        log.level = %config.log.level,
        tracing.exporter = exporter.as_str(),
        "service_starting"
    );
}
