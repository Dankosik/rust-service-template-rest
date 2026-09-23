//! Composition root: configuration, telemetry, readiness, transports, and
//! process lifecycle.
//!
//! Startup order: flags → config → signal handlers → tracer provider →
//! subscriber → metrics recorder → background tasks → dependency pools →
//! readiness admission → HTTP listeners → ready. Shutdown order lives in
//! [`shutdown`]. Handlers and feature code never own this sequence.

mod shutdown;

use std::ffi::OsString;
use std::process::ExitCode;
use std::time::Duration;

use health::{Probe, Readiness, RefreshPolicy};
use infra_http::{HTTP_REQUESTS_DURATION_SECONDS, HardenOptions, Server, ServerOptions};
// template:begin authn:bootstrap-authn-imports
use infra_bearerauthn::Verifier;
// template:end authn:bootstrap-authn-imports
// template:begin postgres:bootstrap-imports
use infra_postgres::{Dsn, PgPool, PoolOptions, PostgresProbe};
// template:end postgres:bootstrap-imports
use infra_telemetry::{
    ExporterState, LoggingFormat, LoggingOptions, Metrics, ResolvedSampler, TracingOptions,
    diagnostics_router, install_subscriber, install_tracer_provider,
};
// template:begin postgres:bootstrap-postgres-secret-import
use secrecy::ExposeSecret;
// template:end postgres:bootstrap-postgres-secret-import
use service_config::{
    AppConfig, BuildInfo, Config, FromArgs, LogFormat, TracesSampler, process_failure,
};
// template:begin authn:bootstrap-authn-config-import
use service_config::AuthnMode;
// template:end authn:bootstrap-authn-config-import
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
    // template:begin authn:bootstrap-authn-errors
    #[error("authentication startup failed")]
    AuthenticationStartup,
    // template:end authn:bootstrap-authn-errors
    // template:begin postgres:bootstrap-errors
    #[error("configuration is invalid: postgres.dsn: {0}")]
    PostgresDsn(#[from] infra_postgres::DsnError),
    #[error(transparent)]
    Postgres(#[from] infra_postgres::ConnectError),
    // template:end postgres:bootstrap-errors
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
    let options = match FromArgs::from_argv(args) {
        FromArgs::Run(options) => options,
        FromArgs::Exit(code) => return code,
    };
    let config = match service_config::load(&options, BUILD_INFO) {
        Ok(config) => config,
        Err(err) => return process_failure(&err.to_string()),
    };
    if let Err(err) = shutdown::validate_grace_budget(&config.http) {
        return process_failure(&err.to_string());
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return process_failure(&format!("build tokio runtime: {err}")),
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
            process_failure(&err.to_string())
        }
    }
}

async fn serve(config: Config) -> Result<Outcome, BootstrapError> {
    // Before this point SIGTERM has its default disposition and kills the
    // process; install the handlers first and keep them for the lifetime.
    let mut signals = Signals::install().map_err(BootstrapError::Signals)?;

    let tracer_provider =
        install_tracer_provider(&tracing_options(&config, replica_instance_id(&config.app)))?;
    install_subscriber(&LoggingOptions {
        level: &config.log.level,
        format: match config.log.format {
            LogFormat::Json => LoggingFormat::Json,
            LogFormat::Text => LoggingFormat::Text,
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
    // One error exit owns cancel/join/close, including the path where the
    // pool never opened (`postgres_pool` stays `None`).
    // template:begin postgres:bootstrap-startup-pool
    let mut postgres_pool = None;
    // template:end postgres:bootstrap-startup-pool
    let outcome = async {
        let probes: Vec<Box<dyn Probe>> = Vec::new();
        // template:begin authn:bootstrap-authn-prepare
        prepare_auth(&config, &tracker, &cancel).await?;
        // template:end authn:bootstrap-authn-prepare
        // template:begin postgres:bootstrap-postgres-startup
        let (probes, pool) = prepare_postgres(probes, &config, &tracker, &cancel).await?;
        postgres_pool = pool;
        // template:end postgres:bootstrap-postgres-startup

        // Admission runs even without probes so the first probe after bind
        // answers from an evaluation.
        let readiness = Readiness::new(probes);
        let policy = RefreshPolicy {
            interval: config.health.refresh_interval,
            probe_budget: config.health.probe_budget,
            failure_threshold: config.health.failure_threshold,
        };

        admit_and_serve(Prepared {
            config: &config,
            signals: &mut signals,
            tracer_provider,
            metrics,
            cancel: cancel.clone(),
            tracker: tracker.clone(),
            readiness,
            policy,
            // template:begin postgres:bootstrap-prepared-pool
            postgres_pool: postgres_pool.clone(),
            // template:end postgres:bootstrap-prepared-pool
        })
        .await
    }
    .await;
    // Bind, admission, or connect failure: cancel and join tracked tasks,
    // then close any opened pool. `Server` only cancels accept. Dropping
    // `TracerProviderHandle` is not last-ref: the global SDK clone remains
    // until process teardown.
    if outcome.is_err() {
        shutdown::close_opened_dependencies(&cancel, &tracker).await;
        // template:begin postgres:bootstrap-startup-pool-close
        if let Some(pool) = postgres_pool.as_ref() {
            shutdown::close_opened_postgres(pool).await;
        }
        // template:end postgres:bootstrap-startup-pool-close
    }
    outcome
}

// template:begin authn:bootstrap-prepare-auth-prefix
#[allow(
    clippy::unused_async,
    reason = "JWT discovery awaits I/O; introspection-only output preserves the same bootstrap future without provider I/O"
)]
async fn prepare_auth(
    config: &Config,
    tracker: &TaskTracker,
    cancel: &CancellationToken,
) -> Result<Verifier, BootstrapError> {
    match config.authn.mode {
        AuthnMode::None => Ok(Verifier::disabled()),
        // template:end authn:bootstrap-prepare-auth-prefix
        // template:begin oidc-jwt:bootstrap-prepare-auth-jwt
        AuthnMode::OidcJwt => {
            let (verifier, refresh) = infra_bearerauthn::prepare_jwt(
                infra_bearerauthn::JwtOptions {
                    issuer: config.authn.issuer.clone(),
                    audience: config.authn.audience.clone(),
                    token_profile: match config.authn.token_profile() {
                        service_config::TokenProfile::ResourceServer => {
                            infra_bearerauthn::TokenProfile::ResourceServer
                        }
                        service_config::TokenProfile::Rfc9068 => {
                            infra_bearerauthn::TokenProfile::Rfc9068
                        }
                    },
                },
                tracker.clone(),
                cancel.child_token(),
            )
            .await
            .map_err(|_| BootstrapError::AuthenticationStartup)?;
            tracker.spawn(refresh);
            Ok(verifier)
        }
        // template:end oidc-jwt:bootstrap-prepare-auth-jwt
        // template:begin oidc-introspection:bootstrap-prepare-auth-introspection
        AuthnMode::OidcIntrospection => infra_bearerauthn::prepare_introspection(
            infra_bearerauthn::IntrospectionOptions {
                issuer: config.authn.issuer.clone(),
                audience: config.authn.audience.clone(),
                endpoint: config.authn.introspection_endpoint.clone(),
                client_id: config.authn.introspection_client_id.clone(),
                client_secret: config.authn.introspection_client_secret.clone(),
            },
            tracker.clone(),
            cancel.child_token(),
        )
        .map_err(|_| BootstrapError::AuthenticationStartup),
        // template:end oidc-introspection:bootstrap-prepare-auth-introspection
        // template:begin authn:bootstrap-prepare-auth-suffix
    }
}
// template:end authn:bootstrap-prepare-auth-suffix

// template:begin postgres:bootstrap-open-postgres
async fn prepare_postgres(
    mut probes: Vec<Box<dyn Probe>>,
    config: &Config,
    tracker: &TaskTracker,
    cancel: &CancellationToken,
) -> Result<(Vec<Box<dyn Probe>>, Option<PgPool>), BootstrapError> {
    let mut postgres_pool = None;
    if config.postgres.enabled {
        let pool = open_postgres(config).await?;
        probes.push(Box::new(PostgresProbe::new(pool.clone())));
        tracker.spawn(infra_postgres::record_metrics_periodically(
            pool.clone(),
            METRICS_MAINTENANCE_INTERVAL,
            cancel.child_token(),
        ));
        postgres_pool = Some(pool);
    }
    Ok((probes, postgres_pool))
}

async fn open_postgres(config: &Config) -> Result<PgPool, BootstrapError> {
    let dsn = Dsn::admit(config.postgres.required_dsn()?.expose_secret())?;
    let pool = infra_postgres::connect(
        &dsn,
        &PoolOptions {
            max_connections: config.postgres.pool_max_connections()?,
            // Same process identity as traces (`service.name`).
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
// template:end postgres:bootstrap-open-postgres

/// Runtime pieces built before listeners bind: admission, then serve.
///
/// `cancel`, `tracker`, and `postgres_pool` are shared clones: `serve` still
/// owns `close_opened_dependencies` on the error path. `tracer_provider` and
/// `metrics` are unique moves; Drop in this callee is enough on `Err`, and
/// success transfers them into [`shutdown::Plan`].
struct Prepared<'a> {
    config: &'a Config,
    signals: &'a mut Signals,
    tracer_provider: infra_telemetry::TracerProviderHandle,
    metrics: Metrics,
    cancel: CancellationToken,
    tracker: TaskTracker,
    readiness: Readiness,
    policy: RefreshPolicy,
    // template:begin postgres:bootstrap-prepared-field
    postgres_pool: Option<PgPool>,
    // template:end postgres:bootstrap-prepared-field
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
        // template:begin postgres:bootstrap-destructure-pool
        postgres_pool,
        // template:end postgres:bootstrap-destructure-pool
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
        max_connections: config.http.connection_cap(),
    };
    // The routes and the committed OpenAPI document are the two halves of
    // one contract; only the routes are needed here.
    let contract = service::api::contract();
    let (routes, _document) = contract.split_for_parts();
    let app = infra_http::harden(
        routes.with_state(readiness.reader()),
        &HardenOptions {
            max_body_bytes: usize::try_from(config.http.max_body_bytes.as_u64())
                .unwrap_or(usize::MAX),
            request_timeout: config.http.request_timeout,
            max_in_flight: config.http.in_flight_cap(),
            log_health_probes: config.http.access_log_health_probes,
        },
    );
    let app_listener = Server::bind(config.http.listen_addr()?, app, server_options).await?;
    tracing::info!(addr = %app_listener.local_addr(), "http listener bound");

    let diagnostics = match config.observability.metrics.listen_addr()? {
        None => None,
        Some(addr) => {
            // Intentionally unhardened: Prometheus text on a private listener.
            // `server_options` is shared HTTP transport policy, not `harden`.
            let server =
                Server::bind(addr, diagnostics_router(metrics.clone()), server_options).await?;
            tracing::info!(addr = %server.local_addr(), "diagnostics listener bound");
            Some(server)
        }
    };

    tracing::info!("service_ready");
    signals.wait().await;

    Ok(shutdown::run(shutdown::Plan {
        http_config: &config.http,
        readiness: &readiness,
        app_listener,
        diagnostics,
        cancel,
        tracker,
        // template:begin postgres:bootstrap-shutdown-plan-pool
        postgres_pool,
        // template:end postgres:bootstrap-shutdown-plan-pool
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
    let sampler = match otel.traces_sampler {
        TracesSampler::AlwaysOn => ResolvedSampler::AlwaysOn,
        TracesSampler::AlwaysOff => ResolvedSampler::AlwaysOff,
        TracesSampler::TraceIdRatio => ResolvedSampler::TraceIdRatio(otel.traces_sampler_arg),
        TracesSampler::ParentBasedTraceIdRatio => {
            ResolvedSampler::ParentBasedTraceIdRatio(otel.traces_sampler_arg)
        }
    };
    TracingOptions {
        service_name: otel.service_name.clone(),
        service_version: config.app.version.clone(),
        vcs_revision: config.app.commit.clone(),
        instance_id,
        deployment_environment: config.app.env.clone(),
        sampler,
        otlp_endpoint: otel.exporter.otlp_endpoint.clone(),
        otlp_headers: otel.exporter.otlp_headers.clone(),
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
            tracing::info!(
                endpoint_source = endpoint_source.as_str(),
                "trace exporter initialized"
            );
        }
        ExporterState::Disabled => {}
    }
    tracing::info!(
        app.env = %config.app.env,
        app.version = %config.app.version,
        app.commit = %config.app.commit,
        http.addr = %config.http.addr,
        http.request_timeout = ?config.http.request_timeout,
        http.drain_timeout = ?config.http.drain_timeout,
        http.grace_period = ?config.http.grace_period,
        observability.metrics.addr = %config.observability.metrics.addr,
        // template:begin postgres:bootstrap-startup-log-postgres
        postgres.enabled = config.postgres.enabled,
        // template:end postgres:bootstrap-startup-log-postgres
        log.level = %config.log.level,
        tracing.exporter = exporter.as_str(),
        "service_starting"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use service_config::OtelConfig;

    #[test]
    fn tracing_options_attaches_the_ratio_only_to_ratio_variants() {
        let mut config = Config::default();
        config.observability.otel = OtelConfig {
            traces_sampler: TracesSampler::AlwaysOn,
            traces_sampler_arg: 0.5,
            ..OtelConfig::default()
        };
        let options = tracing_options(&config, "i".into());
        assert!(matches!(options.sampler, ResolvedSampler::AlwaysOn));

        config.observability.otel.traces_sampler = TracesSampler::TraceIdRatio;
        let options = tracing_options(&config, "i".into());
        assert!(
            matches!(options.sampler, ResolvedSampler::TraceIdRatio(ratio) if (ratio - 0.5).abs() < f64::EPSILON)
        );
    }
}
