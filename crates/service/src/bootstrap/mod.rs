//! Composition root: configuration, telemetry, readiness, transports, and
//! process lifecycle.
//!
//! Startup order: flags → config → signal handlers → tracer provider →
//! subscriber → metrics recorder → background tasks → dependencies → API
//! contract → readiness admission → listeners → ready. A stop signal ends an
//! unfinished startup, and a background task that ends on its own ends the
//! service. Every exit after the signal handlers runs the one teardown in
//! [`shutdown`]. Handlers and feature code never own this sequence.
//!
//! Each optional profile keeps its own startup step in a module beside this
//! one; this file is the order they run in.

// template:begin authn:bootstrap-authn-module
mod authn;
// template:end authn:bootstrap-authn-module
// template:begin cache:service-bootstrap-cache-module
mod cache;
// template:end cache:service-bootstrap-cache-module
// template:begin http-idempotency:bootstrap-http-idempotency-module
mod idempotency;
// template:end http-idempotency:bootstrap-http-idempotency-module
// template:begin object-storage:service-bootstrap-object-storage-module
mod object_storage;
// template:end object-storage:service-bootstrap-object-storage-module
// template:begin postgres:bootstrap-postgres-module
mod postgres;
// template:end postgres:bootstrap-postgres-module
mod shutdown;
// template:begin inbound-webhooks:bootstrap-webhooks-module
mod webhooks;
// template:end inbound-webhooks:bootstrap-webhooks-module

use std::ffi::OsString;
use std::process::ExitCode;
use std::time::Duration;

use health::{Probe, Readiness, RefreshPolicy};
use infra_http::{
    HTTP_REQUESTS_DURATION_BUCKETS, HTTP_REQUESTS_DURATION_SECONDS, HardenOptions, Server,
    ServerOptions,
};
use infra_telemetry::{
    ExporterState, LoggingFormat, LoggingOptions, Metrics, ResolvedSampler, TracingOptions,
    diagnostics_router, install_subscriber, install_tracer_provider, runtime_metrics,
};
use service_config::{
    AppConfig, BuildInfo, Config, LoadOptions, LogFormat, TracesSampler, process_failure,
};
use tokio::task::{JoinError, JoinSet};
use tokio_util::sync::CancellationToken;

use self::shutdown::{Dependencies, Outcome, Serving, Signals};

/// Version and revision stamped into this binary.
pub(crate) const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

/// Exit code when the process shut down but a teardown stage overran its
/// budget: the platform and the process test can tell it from a crash.
const EXIT_DEGRADED_SHUTDOWN: u8 = 3;

/// Bound for dropping whatever the runtime still owns after the ordered
/// teardown: HTTP connection tasks that outlived drain and `pool.close`,
/// and any blocking tracer-provider shutdown that outlived its budget.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// Interval for the Tokio runtime and connection pool metrics.
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
    // template:begin cache:service-bootstrap-cache-errors
    #[error("cache startup: {0}")]
    Cache(#[from] infra_cache::CacheError),
    // template:end cache:service-bootstrap-cache-errors
    // template:begin object-storage:service-bootstrap-object-storage-errors
    #[error("object storage configuration: {0}")]
    ObjectStorage(#[from] infra_object_storage::ConfigError),
    // template:end object-storage:service-bootstrap-object-storage-errors
    #[error("startup admission: {0}")]
    Admission(health::NotReady),
    #[error("configuration is invalid: {0}")]
    Config(#[from] service_config::ValidationError),
    // template:begin authn:bootstrap-authn-errors
    #[error("authentication preparation failed for {mode} {key}: {source}")]
    AuthenticationPreparation {
        mode: &'static str,
        key: &'static str,
        #[source]
        source: infra_bearerauthn::PreparationError,
    },
    // template:end authn:bootstrap-authn-errors
    #[error(transparent)]
    HttpContract(#[from] infra_http::FinalizeError),
    #[error("http contract composition: {0}")]
    HttpComposition(#[from] crate::api::ContractError),
    // template:begin postgres:bootstrap-errors
    #[error("configuration is invalid: postgres.dsn: {0}")]
    PostgresDsn(#[from] infra_postgres::DsnError),
    #[error(transparent)]
    Postgres(#[from] infra_postgres::ConnectError),
    #[error("postgres migration history: {0}")]
    PostgresHistory(#[from] migrate::HistoryError),
    // template:end postgres:bootstrap-errors
    // template:begin http-idempotency:bootstrap-http-idempotency-errors
    #[error("http idempotency startup: {0}")]
    HttpIdempotencyStartup(#[from] infra_idempotency_store::StartupError),
    // template:end http-idempotency:bootstrap-http-idempotency-errors
    // template:begin inbound-webhooks:bootstrap-webhooks-errors
    #[error("inbound webhook endpoint {endpoint} references unavailable key {key}")]
    InboundWebhookKeyReference { endpoint: String, key: String },
    #[error("inbound webhook endpoint {endpoint} key {key} is invalid: {source}")]
    InboundWebhookKey {
        endpoint: String,
        key: String,
        #[source]
        source: infra_webhooks::protocol::ProtocolError,
    },
    // template:end inbound-webhooks:bootstrap-webhooks-errors
    #[error(transparent)]
    Server(#[from] infra_http::ServerError),
    // template:begin grpc:bootstrap-grpc-error
    #[error(transparent)]
    Grpc(#[from] infra_grpc::Error),
    // template:end grpc:bootstrap-grpc-error
    #[error("a background task panicked: {0}")]
    BackgroundPanicked(#[source] JoinError),
    #[error("a background task ended before shutdown")]
    BackgroundEnded,
}

/// Parse flags, load configuration, run the service, and map the result to
/// an exit code. `--help` exits 0 and a flag error exits 2, both printed by
/// clap. Later failures do not call `process::exit`, so destructors run.
/// Version is not a loader flag.
pub(crate) fn run<I>(
    args: I,
    // template:begin grpc:bootstrap-run-registration-parameter
    grpc_registration: Option<crate::GrpcRegistration>,
    // template:end grpc:bootstrap-run-registration-parameter
) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    let options = LoadOptions::parse_from(args);
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

    let outcome = runtime.block_on(serve(
        config,
        // template:begin grpc:bootstrap-grpc-serve-registration-argument
        grpc_registration,
        // template:end grpc:bootstrap-grpc-serve-registration-argument
    ));
    // Drops connection tasks that outlived the drain and any blocking work.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);

    match outcome {
        Ok(Outcome::Graceful) => ExitCode::SUCCESS,
        Ok(Outcome::Degraded) => ExitCode::from(EXIT_DEGRADED_SHUTDOWN),
        Err(err) => {
            // The subscriber may or may not be installed; report both ways.
            tracing::error!(error = %err, "service failed");
            process_failure(&err.to_string())
        }
    }
}

/// The metrics recorder with every retained crate's histogram buckets.
fn install_metrics() -> Result<Metrics, BootstrapError> {
    Ok(Metrics::install(&[
        (
            HTTP_REQUESTS_DURATION_SECONDS,
            HTTP_REQUESTS_DURATION_BUCKETS,
        ),
        // template:begin postgres:bootstrap-postgres-histograms
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
        // template:end postgres:bootstrap-postgres-histograms
        // template:begin outbound-http:service-bootstrap-outbound-histogram
        (
            infra_outbound_http::REQUEST_DURATION_METRIC,
            infra_outbound_http::REQUEST_DURATION_BUCKETS,
        ),
        // template:end outbound-http:service-bootstrap-outbound-histogram
        // template:begin grpc:bootstrap-grpc-histograms
        (
            infra_grpc::SERVER_HANDLING_SECONDS,
            infra_grpc::HANDLING_SECONDS_BUCKETS,
        ),
        (
            infra_grpc::CLIENT_HANDLING_SECONDS,
            infra_grpc::HANDLING_SECONDS_BUCKETS,
        ),
        // template:end grpc:bootstrap-grpc-histograms
        // template:begin cache:service-bootstrap-cache-histogram
        (
            infra_cache::OPERATION_DURATION_METRIC,
            infra_cache::OPERATION_DURATION_BUCKETS,
        ),
        // template:end cache:service-bootstrap-cache-histogram
        // template:begin object-storage:service-bootstrap-object-storage-histogram
        (
            infra_object_storage::OPERATION_DURATION_METRIC,
            infra_object_storage::OPERATION_DURATION_BUCKETS,
        ),
        // template:end object-storage:service-bootstrap-object-storage-histogram
    ])?)
}

async fn serve(
    config: Config,
    // template:begin grpc:bootstrap-grpc-serve-registration-parameter
    grpc_registration: Option<crate::GrpcRegistration>,
    // template:end grpc:bootstrap-grpc-serve-registration-parameter
) -> Result<Outcome, BootstrapError> {
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
    })?;
    let metrics = install_metrics()?;
    metrics.record_trace_exporter_initialized(matches!(
        tracer_provider.exporter_state,
        ExporterState::Initialized { .. }
    ));
    log_startup_summary(&config, &tracer_provider.exporter_state);

    let cancel = CancellationToken::new();
    let mut background = JoinSet::new();
    background.spawn(metrics.clone().upkeep(cancel.child_token()));
    background.spawn(runtime_metrics(
        METRICS_MAINTENANCE_INTERVAL,
        cancel.child_token(),
    ));

    let mut dependencies = Dependencies::default();
    // A stop signal ends an unfinished startup: the dropped future releases
    // what it still held, and teardown releases what it had handed over. A
    // startup that completes in the same poll wins, so its listeners drain.
    let started = tokio::select! {
        biased;
        started = Box::pin(start(
            &config,
            // template:begin grpc:bootstrap-grpc-start-registration-argument
            grpc_registration,
            // template:end grpc:bootstrap-grpc-start-registration-argument
            &metrics,
            &cancel,
            &mut background,
            &mut dependencies,
        )) => started.map(Some),
        () = signals.wait() => Ok(None),
    };
    let (serving, failure) = match started {
        Ok(Some(serving)) => {
            tracing::info!("service_ready");
            let failure = tokio::select! {
                () = signals.wait() => None,
                failure = background_failure(&mut background) => Some(failure),
            };
            (Some(serving), failure)
        }
        Ok(None) => (None, None),
        Err(err) => (None, Some(err)),
    };
    // Dropping `TracerProviderHandle` is not last-ref: the global SDK clone
    // remains until process teardown, so a failed startup flushes too.
    let outcome = shutdown::run(shutdown::Plan {
        http_config: &config.http,
        signals: &mut signals,
        serving,
        cancel,
        background,
        dependencies,
        tracer_provider,
    })
    .await;
    match failure {
        Some(err) => Err(err),
        None => Ok(outcome),
    }
}

/// Resolve when a background task ends while the service is serving. Every
/// task runs until its token is cancelled, and nothing cancels before
/// teardown, so an early end is a panic or a defect: the caller stops the
/// service rather than serve without the task. An empty set never resolves.
async fn background_failure(background: &mut JoinSet<()>) -> BootstrapError {
    match background.join_next().await {
        Some(Ok(())) => BootstrapError::BackgroundEnded,
        Some(Err(err)) => BootstrapError::BackgroundPanicked(err),
        None => std::future::pending().await,
    }
}

/// Open dependencies, compose and admit the routes, and bind the listeners.
/// What startup opens lands in `dependencies` or `background`, so teardown
/// releases it on every path, including a startup dropped at a stop signal.
#[allow(
    clippy::too_many_lines,
    reason = "startup is one ordered sequence; each step names the profile that owns it"
)]
async fn start(
    config: &Config,
    // template:begin grpc:bootstrap-grpc-start-registration-parameter
    grpc_registration: Option<crate::GrpcRegistration>,
    // template:end grpc:bootstrap-grpc-start-registration-parameter
    metrics: &Metrics,
    cancel: &CancellationToken,
    background: &mut JoinSet<()>,
    #[allow(unused_variables, reason = "dependency-free profiles open nothing")]
    dependencies: &mut Dependencies,
) -> Result<Serving, BootstrapError> {
    #[allow(
        unused_mut,
        reason = "profiles without PostgreSQL or messaging have no probe"
    )]
    let mut probes: Vec<Box<dyn Probe>> = Vec::new();
    #[allow(
        unused_variables,
        reason = "the no-auth projection uses this fallback; retained authentication shadows it"
    )]
    let auth = PreparedAuth::None;
    // template:begin authn:bootstrap-authn-prepare
    let auth = authn::prepare(config, background, cancel).await?;
    // template:end authn:bootstrap-authn-prepare
    // template:begin postgres:bootstrap-postgres-startup
    dependencies.postgres = postgres::open(config, background, cancel).await?;
    if let Some(pool) = &dependencies.postgres {
        probes.push(Box::new(infra_postgres::PostgresProbe::new(pool.clone())));
        migrate::verify_history(pool).await?;
    }
    // template:end postgres:bootstrap-postgres-startup
    // template:begin cache:service-bootstrap-cache-startup
    dependencies.cache = cache::open(config).await?;
    // template:end cache:service-bootstrap-cache-startup
    // template:begin object-storage:service-bootstrap-object-storage-startup
    dependencies.object_storage = object_storage::open(config)?;
    // template:end object-storage:service-bootstrap-object-storage-startup
    // template:begin http-idempotency:bootstrap-http-idempotency-composer
    let mut composer = idempotency::prepare(config, dependencies.postgres.as_ref());
    // template:end http-idempotency:bootstrap-http-idempotency-composer
    // template:begin inbound-webhooks:bootstrap-webhooks-prepare
    let webhook_state =
        webhooks::prepare(config, dependencies.postgres.as_ref(), background, cancel)?;
    // template:end inbound-webhooks:bootstrap-webhooks-prepare
    let readiness = Readiness::new(
        probes,
        RefreshPolicy {
            interval: config.health.refresh_interval,
            probe_budget: config.health.probe_budget,
            failure_threshold: config.health.failure_threshold,
        },
    );

    // template:begin grpc:bootstrap-grpc-prepare-start
    let grpc_prepared = if config.grpc.enabled {
        // template:end grpc:bootstrap-grpc-prepare-start
        // template:begin grpc-authn:bootstrap-grpc-verifier
        let verifier = match &auth {
            PreparedAuth::None => {
                return Err(service_config::ValidationError::new(
                    "grpc.enabled",
                    "requires authn.mode = oidc-jwt or oidc-introspection",
                )
                .into());
            }
            PreparedAuth::Enabled(verifier) => (**verifier).clone(),
        };
        // template:end grpc-authn:bootstrap-grpc-verifier
        // template:begin grpc:bootstrap-grpc-prepare-call
        if config.http.effective_drain_budget() < infra_grpc::CALL_DEADLINE_CAP {
            return Err(service_config::ValidationError::new(
                "http.drain_timeout",
                format!(
                    "minus http.readiness_propagation_delay must cover the {:?} gRPC unary deadline",
                    infra_grpc::CALL_DEADLINE_CAP
                ),
            )
            .into());
        }
        Some((
            infra_grpc::router(
                crate::grpc::services(grpc_registration)?,
                readiness.reader(),
                // template:end grpc:bootstrap-grpc-prepare-call
                // template:begin grpc-authn:bootstrap-grpc-verifier-argument
                verifier,
                // template:end grpc-authn:bootstrap-grpc-verifier-argument
                // template:begin grpc:bootstrap-grpc-prepare-finish
            ),
            crate::grpc::tls(config)?,
        ))
    } else {
        None
    };
    // template:end grpc:bootstrap-grpc-prepare-finish
    // The routes and the committed OpenAPI document are the two halves of
    // one contract. Assembly is pure, so it runs before readiness admission.
    let contract = crate::api::contract(
        // template:begin http-idempotency:bootstrap-http-idempotency-contract-composer
        &mut composer,
        // template:end http-idempotency:bootstrap-http-idempotency-contract-composer
    )?;
    let routes = match auth {
        PreparedAuth::None => infra_http::finalize_public(contract)?,
        // template:begin authn:bootstrap-authn-finalize-enabled
        PreparedAuth::Enabled(verifier) => infra_http::authn::finalize(contract, *verifier)?,
        // template:end authn:bootstrap-authn-finalize-enabled
    };
    // template:begin http-idempotency:bootstrap-http-idempotency-activation
    idempotency::activate(composer, config, background, cancel).await?;
    // template:end http-idempotency:bootstrap-http-idempotency-activation
    // Admission runs even without probes so the first probe after bind
    // answers from an evaluation.
    readiness.refresh().await;
    readiness
        .reader()
        .verdict()
        .map_err(BootstrapError::Admission)?;
    background.spawn({
        let readiness = readiness.clone();
        let cancel = cancel.child_token();
        async move { readiness.refresh_until(cancel).await }
    });

    let server_options = ServerOptions {
        header_read_timeout: config.http.header_read_timeout,
        max_header_bytes: usize::try_from(config.http.max_header_bytes.as_u64())
            .unwrap_or(usize::MAX),
        max_connections: config.http.connection_cap(),
    };
    let state = crate::AppState {
        readiness: readiness.reader(),
        // template:begin inbound-webhooks:bootstrap-webhooks-route-state
        webhooks: webhook_state,
        // template:end inbound-webhooks:bootstrap-webhooks-route-state
    };
    let app = infra_http::harden(
        routes.with_state(state),
        &HardenOptions {
            max_body_bytes: usize::try_from(config.http.max_body_bytes.as_u64())
                .unwrap_or(usize::MAX),
            request_timeout: config.http.request_timeout,
            max_in_flight: config.http.in_flight_cap(),
            log_health_probes: config.http.access_log_health_probes,
        },
    );
    let app_listener = Server::bind(config.http.addr, app, server_options).await?;
    tracing::info!(addr = %app_listener.local_addr(), "http listener bound");

    let diagnostics = match config.observability.metrics.addr {
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

    // template:begin grpc:bootstrap-grpc-bind
    let grpc_listener = match grpc_prepared {
        Some((grpc_router, tls)) => {
            let addr = config.grpc.listen_addr()?;
            let bound = match tls {
                Some(tls) => {
                    Server::bind_tls(addr, grpc_router, infra_grpc::server_options(), tls).await?
                }
                None => Server::bind(addr, grpc_router, infra_grpc::server_options()).await?,
            };
            tracing::info!(addr = %bound.local_addr(), "grpc listener bound");
            Some(bound)
        }
        None => None,
    };
    // template:end grpc:bootstrap-grpc-bind

    Ok(Serving {
        readiness,
        app_listener,
        diagnostics,
        // template:begin grpc:bootstrap-serving-grpc
        grpc_listener,
        // template:end grpc:bootstrap-serving-grpc
    })
}

enum PreparedAuth {
    None,
    // template:begin authn:bootstrap-prepared-auth-enabled
    Enabled(Box<infra_bearerauthn::Verifier>),
    // template:end authn:bootstrap-prepared-auth-enabled
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
    exporter.log();
    tracing::info!(
        app.env = %config.app.env,
        app.version = %config.app.version,
        app.commit = %config.app.commit,
        http.addr = %config.http.addr,
        http.request_timeout = ?config.http.request_timeout,
        http.drain_timeout = ?config.http.drain_timeout,
        http.grace_period = ?config.http.grace_period,
        observability.metrics.addr = ?config.observability.metrics.addr,
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

    #[tokio::test]
    async fn a_background_task_that_ends_while_serving_is_a_failure() {
        let mut background = JoinSet::new();
        background.spawn(std::future::pending());
        background.spawn(async {});
        assert!(matches!(
            background_failure(&mut background).await,
            BootstrapError::BackgroundEnded
        ));

        background.spawn(async { panic!("task defect") });
        assert!(matches!(
            background_failure(&mut background).await,
            BootstrapError::BackgroundPanicked(err) if err.is_panic()
        ));
    }
}
