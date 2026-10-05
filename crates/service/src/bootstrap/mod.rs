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
use std::panic::AssertUnwindSafe;
use std::process::ExitCode;
use std::time::Duration;

use futures_util::FutureExt;

use health::{Probe, Readiness, RefreshPolicy};
use infra_http::{
    HTTP_REQUESTS_DURATION_BUCKETS, HTTP_REQUESTS_DURATION_SECONDS, HardenOptions, Server,
    ServerOptions,
};
use infra_telemetry::{
    ExporterState, LoggingFormat, LoggingOptions, Metrics, PanicMessage, ResolvedSampler,
    TracingOptions, diagnostics_router, install_panic_hook, install_subscriber,
    install_tracer_provider, runtime_metrics,
};
use service_config::{
    AppConfig, BuildInfo, Config, LoadOptions, LogFormat, TracesSampler, process_failure,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use self::shutdown::{Background, BackgroundFailure, Dependencies, Outcome, Serving, Signals};

/// Version and revision stamped into this binary.
pub(crate) const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

/// Exit code when the process shut down but a teardown stage overran its
/// budget: the platform and the process test can tell it from a crash.
const EXIT_DEGRADED_SHUTDOWN: u8 = 3;

/// Bound for waiting on runtime shutdown after ordered teardown. Running
/// blocking work may outlive this wait and is never certified terminated.
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
    #[error("background task {name} ended unexpectedly (panicked: {panicked})")]
    Background { name: &'static str, panicked: bool },
    #[error("service bootstrap panicked")]
    Panicked,
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
        .worker_threads(config.runtime.effective_worker_threads())
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return process_failure(&format!("build tokio runtime: {err}")),
    };

    let mut signals = None;
    let mut deadline = None;
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(serve(
            config,
            // template:begin grpc:bootstrap-grpc-serve-registration-argument
            grpc_registration,
            // template:end grpc:bootstrap-grpc-serve-registration-argument
            &mut signals,
            &mut deadline,
        ))
    }))
    .unwrap_or(Err(BootstrapError::Panicked));
    // This bounds the wait; already running blocking work can outlive it.
    let remaining = deadline.map_or(RUNTIME_SHUTDOWN_TIMEOUT, |deadline: Instant| {
        RUNTIME_SHUTDOWN_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()))
    });
    runtime.shutdown_timeout(remaining);
    drop(signals);

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

#[allow(
    clippy::too_many_lines,
    reason = "retained lifecycle state stays beside its single unwind and teardown boundary"
)]
async fn serve(
    config: Config,
    // template:begin grpc:bootstrap-grpc-serve-registration-parameter
    grpc_registration: Option<crate::GrpcRegistration>,
    // template:end grpc:bootstrap-grpc-serve-registration-parameter
    retained_signals: &mut Option<Signals>,
    deadline: &mut Option<Instant>,
) -> Result<Outcome, BootstrapError> {
    let signals = retained_signals.insert(Signals::install().map_err(BootstrapError::Signals)?);
    let cancel = CancellationToken::new();
    let mut background = Background::new(cancel.clone());
    let mut observer = background.observer();
    let mut dependencies = Dependencies::default();
    let mut serving = Serving::default();
    let mut tracer_provider = None;

    // Only cleanup may consume mutated state after this guarded operation fails.
    let failure = AssertUnwindSafe(async {
        let admitted = {
            let startup = async {
                let provider = tracer_provider.insert(install_tracer_provider(&tracing_options(
                    &config,
                    replica_instance_id(&config.app),
                ))?);
                install_subscriber(&LoggingOptions {
                    level: &config.log.level,
                    format: match config.log.format {
                        LogFormat::Json => LoggingFormat::Json,
                        LogFormat::Text => LoggingFormat::Text,
                    },
                    tracer_provider: Some(provider),
                })?;
                install_panic_hook(PanicMessage::Recorded);
                let metrics = install_metrics()?;
                metrics.record_trace_exporter_initialized(matches!(
                    provider.exporter_state,
                    ExporterState::Initialized { .. }
                ));
                log_startup_summary(&config, &provider.exporter_state);
                background.spawn(
                    "metrics_upkeep",
                    metrics.clone().upkeep(cancel.child_token()),
                );
                background.spawn(
                    "runtime_metrics",
                    runtime_metrics(METRICS_MAINTENANCE_INTERVAL, cancel.child_token()),
                );
                start(
                    &config,
                    // template:begin grpc:bootstrap-grpc-start-registration-argument
                    grpc_registration,
                    // template:end grpc:bootstrap-grpc-start-registration-argument
                    &metrics,
                    &cancel,
                    &mut background,
                    &mut dependencies,
                    &mut serving,
                )
                .await
            };
            tokio::select! {
                biased;
                stopped = signals.wait() => stopped.map(|()| false).map_err(BootstrapError::Signals),
                failed = shutdown::background_failure(&mut observer) => Err(background_error(failed)),
                started = startup => started.map(|()| true),
            }
        };
        // Faults are sticky, including when a stop won the select's priority.
        let mut failure = pending_failure(&background, &serving);
        let mut ready = false;
        match admitted {
            Err(error) => {
                failure.get_or_insert(error);
            }
            Ok(true) if failure.is_none() => match signals.pending() {
                Ok(false) => ready = true,
                Ok(true) => {}
                Err(error) => {
                    failure = Some(BootstrapError::Signals(error));
                }
            },
            Ok(_) => {}
        }
        if ready {
            serving.admitted = true;
            tracing::info!("service_ready");
            failure = {
                tokio::select! {
                    biased;
                    stopped = signals.wait() => stopped.err().map(BootstrapError::Signals),
                    failed = shutdown::background_failure(&mut observer) => Some(background_error(failed)),
                }
            };
            if failure.is_none() {
                failure = pending_failure(&background, &serving);
            }
        }
        failure
    })
    .catch_unwind()
    .await
    .unwrap_or(Some(BootstrapError::Panicked));
    let deadline = *deadline.get_or_insert_with(|| {
        signals.first_stop().unwrap_or_else(Instant::now) + config.http.grace_period
    });
    let outcome = shutdown::run(shutdown::Plan {
        http_config: &config.http,
        signals,
        deadline,
        serving,
        background,
        dependencies,
        tracer_provider,
    })
    .await;
    match failure {
        Some(error) => Err(error),
        None => Ok(outcome),
    }
}

fn pending_failure(background: &Background, serving: &Serving) -> Option<BootstrapError> {
    if let Some(failure) = background.failed() {
        return Some(background_error(failure));
    }
    for (name, listener) in [
        ("http_accept", serving.app_listener.as_ref()),
        ("diagnostics_accept", serving.diagnostics.as_ref()),
        // template:begin grpc:bootstrap-pending-grpc
        ("grpc_accept", serving.grpc_listener.as_ref()),
        // template:end grpc:bootstrap-pending-grpc
    ] {
        if let Some(listener) = listener
            && let Some(failure) = listener.failure().now_or_never()
        {
            return Some(background_error(BackgroundFailure {
                name,
                panicked: failure == infra_http::AcceptFailure::Panicked,
            }));
        }
    }
    None
}

fn background_error(failure: BackgroundFailure) -> BootstrapError {
    BootstrapError::Background {
        name: failure.name,
        panicked: failure.panicked,
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
    background: &mut Background,
    #[allow(unused_variables, reason = "dependency-free profiles open nothing")]
    dependencies: &mut Dependencies,
    serving: &mut Serving,
) -> Result<(), BootstrapError> {
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
    postgres::open(config, background, cancel, &mut dependencies.postgres).await?;
    if let Some(pool) = &dependencies.postgres {
        probes.push(Box::new(infra_postgres::PostgresProbe::new(pool.clone())));
        migrate::verify_history(pool).await?;
    }
    // template:end postgres:bootstrap-postgres-startup
    // template:begin cache:service-bootstrap-cache-startup
    cache::open(config, &mut dependencies.cache).await?;
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
    let readiness = serving.readiness.insert(Readiness::new(
        probes,
        RefreshPolicy {
            interval: config.health.refresh_interval,
            probe_budget: config.health.probe_budget,
            failure_threshold: config.health.failure_threshold,
        },
    ));

    // Built before any route or registration reads it, so both transports
    // hand their handlers the same dependencies.
    let state = crate::AppState {
        readiness: readiness.reader(),
        // template:begin inbound-webhooks:bootstrap-webhooks-route-state
        webhooks: webhook_state,
        // template:end inbound-webhooks:bootstrap-webhooks-route-state
    };

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
        let limits = crate::grpc::limits(config);
        Some((
            infra_grpc::router(
                crate::grpc::services(grpc_registration, &state)?,
                readiness.reader(),
                // template:end grpc:bootstrap-grpc-prepare-call
                // template:begin grpc-authn:bootstrap-grpc-verifier-argument
                verifier,
                // template:end grpc-authn:bootstrap-grpc-verifier-argument
                // template:begin grpc:bootstrap-grpc-prepare-finish
                limits,
            )?,
            crate::grpc::tls(config)?,
            infra_grpc::server_options(limits),
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
    background.spawn("readiness", {
        let readiness = readiness.clone();
        let cancel = cancel.child_token();
        async move { readiness.refresh_until(cancel).await }
    });

    let server_options = ServerOptions {
        header_read_timeout: config.http.header_read_timeout,
        max_header_bytes: usize::try_from(config.http.max_header_bytes.as_u64())
            .unwrap_or(usize::MAX),
        max_connections: config.http.connection_cap(),
        max_connection_age: config.http.connection_age(),
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
    #[cfg(feature = "hotpath")]
    let app = hotpath::axum!(app);
    #[cfg(feature = "hotpath")]
    hotpath::tokio_runtime!();
    let app_listener = serving
        .app_listener
        .insert(Server::bind(config.http.addr, app, server_options).await?);
    background.watch_listener("http_accept", app_listener);
    tracing::info!(addr = %app_listener.local_addr(), "http listener bound");

    if let Some(addr) = config.observability.metrics.addr {
        // Intentionally unhardened: Prometheus text on a private listener.
        // `server_options` is shared HTTP transport policy, not `harden`.
        // Liveness is served here as well: this listener has its own
        // connection cap, so a full application listener cannot fail it.
        let diagnostics = diagnostics_router(metrics.clone()).merge(infra_http::liveness_router());
        let server = serving
            .diagnostics
            .insert(Server::bind(addr, diagnostics, server_options).await?);
        background.watch_listener("diagnostics_accept", server);
        tracing::info!(addr = %server.local_addr(), "diagnostics listener bound");
    }

    // template:begin grpc:bootstrap-grpc-bind
    if let Some((grpc_router, tls, grpc_options)) = grpc_prepared {
        let addr = config.grpc.listen_addr()?;
        let bound = match tls {
            Some(tls) => Server::bind_tls(addr, grpc_router, grpc_options, tls).await?,
            None => Server::bind(addr, grpc_router, grpc_options).await?,
        };
        let listener = serving.grpc_listener.insert(bound);
        background.watch_listener("grpc_accept", listener);
        tracing::info!(addr = %listener.local_addr(), "grpc listener bound");
    }
    // template:end grpc:bootstrap-grpc-bind

    Ok(())
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
        runtime.worker_threads = tokio::runtime::Handle::current().metrics().num_workers(),
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
    async fn a_later_bind_failure_retains_and_closes_the_first_listener_and_live_peer() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = Config::default();
        config.http.addr = "127.0.0.1:0".parse().unwrap();
        config.observability.metrics.addr = Some(occupied.local_addr().unwrap());
        // An unadmitted startup must skip this wait entirely.
        config.http.readiness_propagation_delay = Duration::from_secs(20);
        let metrics = install_metrics().unwrap();
        let cancel = CancellationToken::new();
        let mut background = Background::new(cancel.clone());
        let mut dependencies = Dependencies::default();
        let mut serving = Serving::default();
        let result = start(
            &config,
            // template:begin grpc:bootstrap-test-grpc-registration
            None,
            // template:end grpc:bootstrap-test-grpc-registration
            &metrics,
            &cancel,
            &mut background,
            &mut dependencies,
            &mut serving,
        )
        .await;
        assert!(matches!(
            result,
            Err(BootstrapError::Server(infra_http::ServerError::Bind { .. }))
        ));
        assert!(!serving.admitted);
        let address = serving.app_listener.as_ref().unwrap().local_addr();
        let mut peer = tokio::net::TcpStream::connect(address).await.unwrap();
        peer.write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !response.ends_with(b"ok") {
                let count = peer.read_buf(&mut response).await.unwrap();
                assert_ne!(count, 0);
            }
        })
        .await
        .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
        let mut signals = Signals::install().unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            shutdown::run(shutdown::Plan {
                http_config: &config.http,
                signals: &mut signals,
                deadline: Instant::now() + config.http.grace_period,
                serving,
                background,
                dependencies,
                tracer_provider: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Graceful);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), peer.read(&mut [0_u8; 1]))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }
}
