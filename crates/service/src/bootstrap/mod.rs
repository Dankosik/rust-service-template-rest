//! Composition root: configuration, telemetry, readiness, transports, and
//! process lifecycle.
//!
//! Startup order: flags → config → signal handlers → tracer provider →
//! subscriber → metrics recorder → background tasks → readiness admission →
//! HTTP listeners → ready. Shutdown order lives in [`shutdown`]. Handlers and
//! feature code never own this sequence.

mod shutdown;

use std::ffi::OsString;
use std::process::ExitCode;
use std::time::Duration;

use health::{Readiness, RefreshPolicy};
use infra_http::{HardenOptions, Server, ServerOptions};
use infra_telemetry::{
    ExporterState, LoggingOptions, Metrics, Sampler, TracingOptions, build_tracer_provider,
    diagnostics_router, install_subscriber,
};
use secrecy::ExposeSecret;
use service_config::{BuildInfo, Config, LoadOptions, LogFormat, TracesSampler};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use self::shutdown::{Outcome, Signals};

/// Version and revision stamped into this binary.
pub(crate) const BUILD_INFO: BuildInfo = BuildInfo {
    version: env!("CARGO_PKG_VERSION"),
    commit: env!("VERGEN_GIT_SHA"),
};

/// Exit code when the process shut down but a teardown stage overran its
/// budget: the platform and the process test can tell it from a crash.
const EXIT_DEGRADED_SHUTDOWN: u8 = 3;

/// Bound for dropping whatever the runtime still owns after the ordered
/// teardown, such as connection tasks that outlived the drain.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// Interval for Prometheus histogram upkeep and Tokio runtime metrics.
const METRICS_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub(crate) enum BootstrapError {
    #[error(transparent)]
    Signals(std::io::Error),
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
    #[error(transparent)]
    Server(#[from] infra_http::ServerError),
}

/// Parse flags, load configuration, run the service, and map the result to
/// an exit code. Never calls `process::exit`, so destructors run.
pub(crate) fn run<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    let options = match LoadOptions::parse_args(args) {
        Ok(options) => options,
        Err(err) => return startup_failure(&err.to_string()),
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

    let telemetry_tracer = build_tracer_provider(&tracing_options(&config))?;
    install_subscriber(LoggingOptions {
        level: config.log.level.clone(),
        format: match config.log.format {
            LogFormat::Json => infra_telemetry::LogFormat::Json,
            LogFormat::Text => infra_telemetry::LogFormat::Text,
        },
        tracer: Some(telemetry_tracer.tracer(&config.observability.otel.service_name)),
    })?;
    let metrics = Metrics::install(axum_prometheus::AXUM_HTTP_REQUESTS_DURATION_SECONDS)?;
    metrics.record_trace_exporter_initialized(matches!(
        telemetry_tracer.exporter,
        ExporterState::Initialized { .. }
    ));
    log_startup_summary(&config, &telemetry_tracer.exporter);

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

    // No dependency probes exist yet; profiles add theirs here. Admission
    // still runs so the first probe after bind answers from an evaluation.
    let readiness = Readiness::new(Vec::new());
    let policy = RefreshPolicy {
        interval: config.health.refresh_interval,
        probe_budget: config.http.readiness_timeout,
        failure_threshold: config.health.failure_threshold,
    };
    readiness
        .refresh(policy)
        .await
        .map_err(BootstrapError::Admission)?;
    tracker.spawn({
        let readiness = readiness.clone();
        let cancel = cancel.child_token();
        async move { readiness.watch(policy, cancel).await }
    });

    let server_options = ServerOptions {
        header_read_timeout: config.http.header_read_timeout,
        max_header_bytes: usize::try_from(config.http.max_header_bytes.as_u64())
            .unwrap_or(usize::MAX),
        max_connections: config.http.max_connections,
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
            max_in_flight: config.http.max_in_flight,
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
        tracer: telemetry_tracer,
        signals: &mut signals,
    })
    .await)
}

fn tracing_options(config: &Config) -> TracingOptions {
    let otel = &config.observability.otel;
    let sampler = match otel.traces_sampler {
        TracesSampler::AlwaysOn => Sampler::AlwaysOn,
        TracesSampler::AlwaysOff => Sampler::AlwaysOff,
        TracesSampler::TraceIdRatio => Sampler::TraceIdRatio(otel.traces_sampler_arg),
        TracesSampler::ParentBasedTraceIdRatio => {
            Sampler::ParentBasedTraceIdRatio(otel.traces_sampler_arg)
        }
    };
    let instance_id = if config.app.instance_id.trim().is_empty() {
        gethostname::gethostname().to_string_lossy().into_owned()
    } else {
        config.app.instance_id.clone()
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
        log.level = %config.log.level,
        tracing.exporter = exporter.as_str(),
        "service_starting"
    );
}
