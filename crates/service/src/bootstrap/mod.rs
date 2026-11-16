mod background;
// template:begin grpc:bootstrap-grpc-module
mod grpc;
// template:end grpc:bootstrap-grpc-module
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
// template:begin service-auth:bootstrap-service-auth-module
mod service_auth;
// template:end service-auth:bootstrap-service-auth-module
mod shutdown;
mod signals;
mod telemetry;
// template:begin inbound-webhooks:bootstrap-webhooks-module
mod inbound_webhooks;
// template:end inbound-webhooks:bootstrap-webhooks-module

use std::ffi::OsString;
use std::panic::AssertUnwindSafe;
use std::process::ExitCode;
use std::time::Duration;

use futures_util::FutureExt;

use health::{ArmError, Probe, Readiness, RefreshPolicy};
use infra_http::{BoundListener, HardenOptions, ServerOptions};
use infra_telemetry::{
    LoggingFormat, LoggingOptions, Metrics, diagnostics_router, install_panic_hook,
    install_subscriber, install_tracer_provider, runtime_metrics,
};
use service_config::{BuildInfo, Config, LoadOptions, LogFormat, process_failure};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use self::background::{Background, BackgroundFailure, background_failure};
pub use self::background::{BackgroundFailureReporter, BackgroundRegistration};
// template:begin grpc:bootstrap-grpc-registration-export
pub use self::grpc::GrpcRegistration;
// template:end grpc:bootstrap-grpc-registration-export
use self::shutdown::{Dependencies, Outcome, Serving};
use self::signals::Signals;

pub(crate) const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

const EXIT_DEGRADED_SHUTDOWN: u8 = 3;

const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

const METRICS_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub(crate) enum BootstrapError {
    #[error("install stop signal handlers: {0}")]
    Signals(#[source] std::io::Error),
    #[error(transparent)]
    SignalClosed(#[from] signals::SignalError),
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
    #[error("authentication ({mode}, {key}): {source}")]
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
    #[error("inbound webhook endpoints are configured but no PostgreSQL pool was opened")]
    InboundWebhooksWithoutPostgres,
    #[error("inbound webhook endpoint {endpoint} references unavailable key {key}")]
    InboundWebhookKeyReference { endpoint: String, key: String },
    #[error("inbound webhook endpoint {endpoint} key {key} is invalid: {source}")]
    InboundWebhookKey {
        endpoint: String,
        key: String,
        #[source]
        source: infra_webhooks::protocol::InvalidKey,
    },
    // template:end inbound-webhooks:bootstrap-webhooks-errors
    #[error(transparent)]
    Server(#[from] infra_http::ServerError),
    // template:begin grpc:bootstrap-grpc-error
    #[error(transparent)]
    Grpc(#[from] infra_grpc::Error),
    // template:end grpc:bootstrap-grpc-error
    #[error("background task {name} failed (panicked: {panicked})")]
    Background { name: &'static str, panicked: bool },
    #[error("startup {stage} blocking task failed: {source}")]
    BlockingStartup {
        stage: &'static str,
        #[source]
        source: tokio::task::JoinError,
    },
    #[error("service bootstrap panicked")]
    Panicked,
}

#[allow(
    dead_code,
    reason = "the postgres and cache profiles are optional; a dependency-free service retains no blocking startup call"
)]
async fn blocking_startup<T, E>(
    stage: &'static str,
    operation: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, BootstrapError>
where
    T: Send + 'static,
    E: Into<BootstrapError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|source| BootstrapError::BlockingStartup { stage, source })?
        .map_err(Into::into)
}

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
        Err(err) => return process_failure(&err),
    };
    if let Err(err) = shutdown::validate_grace_budget(&config.http) {
        return process_failure(&err);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.runtime.effective_worker_threads())
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return process_failure(format!("build tokio runtime: {err}")),
    };

    let mut logger_installed = false;
    let mut signals = None;
    let mut deadline = None;
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(serve(
            config,
            &mut logger_installed,
            // template:begin grpc:bootstrap-grpc-serve-registration-argument
            grpc_registration,
            // template:end grpc:bootstrap-grpc-serve-registration-argument
            &mut signals,
            &mut deadline,
        ))
    }))
    .unwrap_or(Err(BootstrapError::Panicked));
    let remaining = deadline.map_or(RUNTIME_SHUTDOWN_TIMEOUT, |deadline: Instant| {
        RUNTIME_SHUTDOWN_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()))
    });
    runtime.shutdown_timeout(remaining);
    drop(signals);

    match outcome {
        Ok(Outcome::Graceful) => ExitCode::SUCCESS,
        Ok(Outcome::Degraded) => ExitCode::from(EXIT_DEGRADED_SHUTDOWN),
        Err(err) => {
            if logger_installed {
                ExitCode::FAILURE
            } else {
                process_failure(&err)
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "retained lifecycle state stays beside its single unwind and teardown boundary"
)]
async fn serve(
    config: Config,
    logger_installed: &mut bool,
    // template:begin grpc:bootstrap-grpc-serve-registration-parameter
    grpc_registration: Option<crate::GrpcRegistration>,
    // template:end grpc:bootstrap-grpc-serve-registration-parameter
    retained_signals: &mut Option<Signals>,
    deadline: &mut Option<Instant>,
) -> Result<Outcome, BootstrapError> {
    let signals = retained_signals.insert(Signals::install().map_err(BootstrapError::Signals)?);
    let mut background = Background::new(CancellationToken::new());
    let mut observer = background.observer();
    let mut dependencies = Dependencies::default();
    let mut serving = Serving::default();
    let mut tracer_provider = None;
    let mut logger = None;

    let failure = AssertUnwindSafe(async {
        let race = {
            let startup = async {
                let provider =
                    tracer_provider.insert(install_tracer_provider(&telemetry::tracing_options(&config))?);
                logger = Some(install_subscriber(&LoggingOptions {
                    level: &config.log.level,
                    format: match config.log.format {
                        LogFormat::Json => LoggingFormat::Json,
                        LogFormat::Text => LoggingFormat::Text,
                    },
                    tracer_provider: Some(provider),
                })?);
                *logger_installed = true;
                install_panic_hook();
                if let Err(error) = rlimit::increase_nofile_limit(u64::MAX) {
                    tracing::warn!(error = %error, "open_file_limit_unchanged");
                }
                let metrics = telemetry::install_metrics()?;
                metrics.record_trace_exporter(Some(&provider.exporter_state));
                telemetry::log_startup_summary(&config, &provider.exporter_state);
                background.spawn(
                    "metrics_upkeep",
                    metrics.clone().upkeep(background.child_token()),
                );
                background.spawn(
                    "runtime_progress",
                    metrics.clone().runtime_progress(background.child_token()),
                );
                background.spawn(
                    "runtime_metrics",
                    runtime_metrics(METRICS_MAINTENANCE_INTERVAL, background.child_token()),
                );
                start(
                    &config,
                    // template:begin grpc:bootstrap-grpc-start-registration-argument
                    grpc_registration,
                    // template:end grpc:bootstrap-grpc-start-registration-argument
                    &metrics,
                    &mut background,
                    &mut dependencies,
                    &mut serving,
                )
                .await
            };
            tokio::select! {
                biased;
                stopped = signals.wait() => stopped.map(|()| StartupRace::Stopped).map_err(BootstrapError::from),
                failed = background_failure(&mut observer) => Err(background_error(failed)),
                started = startup => started.map(|()| StartupRace::Completed),
            }
        };
        let mut failure = pending_failure(&background, &serving);
        let mut ready = false;
        match race {
            Err(error) => {
                failure.get_or_insert(error);
            }
            Ok(StartupRace::Completed) if failure.is_none() => match signals.pending() {
                Ok(false) => ready = true,
                Ok(true) => {}
                Err(error) => failure = Some(error.into()),
            },
            Ok(StartupRace::Completed | StartupRace::Stopped) => {}
        }
        if ready {
            serving.admitted = true;
            tracing::info!("service_ready");
            failure = {
                tokio::select! {
                    biased;
                    stopped = signals.wait() => stopped.err().map(BootstrapError::from),
                    failed = background_failure(&mut observer) => Some(background_error(failed)),
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
    let failure = stop_progress(&background, &serving, failure);
    let deadline = *deadline.get_or_insert_with(|| {
        signals.first_stop().unwrap_or_else(Instant::now) + config.http.grace_period
    });
    if let Some(error) = &failure
        && logger.is_some()
    {
        tracing::error!(error = %error, "service failed");
    }
    let outcome = shutdown::run(shutdown::Plan {
        http_config: &config.http,
        signals,
        deadline,
        serving,
        background,
        dependencies,
        tracer_provider,
        logger,
    })
    .await;
    match failure {
        Some(error) => Err(error),
        None => Ok(outcome),
    }
}

enum StartupRace {
    Completed,
    Stopped,
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
            && let Some(failure) = listener.accept_failure().now_or_never()
        {
            return Some(background_error(BackgroundFailure {
                name,
                panicked: failure == infra_http::AcceptFailure::Panicked,
            }));
        }
    }
    serving
        .readiness
        .as_ref()
        .filter(|readiness| readiness.progress_lost())
        .map(|_| progress_error())
}

fn progress_error() -> BootstrapError {
    BootstrapError::Background {
        name: "readiness_progress",
        panicked: false,
    }
}

fn stop_progress(
    background: &Background,
    serving: &Serving,
    failure: Option<BootstrapError>,
) -> Option<BootstrapError> {
    if let Some(readiness) = &serving.readiness {
        readiness.stop_progress();
    }
    failure.or_else(|| pending_failure(background, serving))
}

fn start_progress(
    readiness: &Readiness,
    background: &mut Background,
) -> Result<(), BootstrapError> {
    readiness.arm_progress().map_err(|error| match error {
        ArmError::ProgressLost => progress_error(),
        ArmError::Refused(reason) => BootstrapError::Admission(reason),
    })?;
    background.spawn("readiness", {
        let readiness = readiness.clone();
        let cancel = background.child_token();
        async move { readiness.refresh_until(cancel).await }
    });
    BackgroundRegistration { background }.spawn("readiness_progress", {
        let readiness = readiness.clone();
        move |cancel, reporter| async move {
            readiness
                .observe_progress(cancel, || reporter.report())
                .await;
            Ok::<(), std::convert::Infallible>(())
        }
    });
    Ok(())
}

fn background_error(failure: BackgroundFailure) -> BootstrapError {
    BootstrapError::Background {
        name: failure.name,
        panicked: failure.panicked,
    }
}

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
    let auth = authn::prepare(config, background).await?;
    // template:end authn:bootstrap-authn-prepare
    // template:begin service-auth:bootstrap-service-auth-prepare
    let service_auth = service_auth::prepare(config)?;
    // template:end service-auth:bootstrap-service-auth-prepare
    // template:begin postgres:bootstrap-postgres-startup
    postgres::open(config, background, &mut dependencies.postgres).await?;
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
    let mut composer = idempotency::prepare(config, dependencies.postgres.as_ref())?;
    // template:end http-idempotency:bootstrap-http-idempotency-composer
    // template:begin inbound-webhooks:bootstrap-webhooks-prepare
    let webhook_state =
        inbound_webhooks::prepare(config, dependencies.postgres.as_ref(), background)?;
    // template:end inbound-webhooks:bootstrap-webhooks-prepare
    let readiness = serving.readiness.insert(Readiness::new(
        probes,
        RefreshPolicy {
            interval: config.health.refresh_interval,
            probe_budget: config.health.probe_budget,
            failure_threshold: config.health.failure_threshold,
        },
    ));

    let state = crate::AppState {
        readiness: readiness.reader(),
        // template:begin inbound-webhooks:bootstrap-webhooks-route-state
        webhooks: webhook_state,
        // template:end inbound-webhooks:bootstrap-webhooks-route-state
        // template:begin service-auth:bootstrap-service-auth-route-state
        service_tokens: service_auth.tokens.clone(),
        // template:end service-auth:bootstrap-service-auth-route-state
    };

    // template:begin grpc:bootstrap-grpc-prepare-start
    let grpc_prepared = if let Some(listener) = &config.grpc {
        Some(grpc::prepare(
            listener,
            grpc_registration,
            &state,
            readiness,
            &auth,
            // template:end grpc:bootstrap-grpc-prepare-start
            // template:begin service-auth:bootstrap-grpc-service-auth-argument
            &service_auth,
            // template:end service-auth:bootstrap-grpc-service-auth-argument
            // template:begin grpc:bootstrap-grpc-prepare-finish
            background,
        )?)
    } else {
        None
    };
    // template:end grpc:bootstrap-grpc-prepare-finish
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
    idempotency::activate(composer, config, background).await?;
    // template:end http-idempotency:bootstrap-http-idempotency-activation
    let app_listener = BoundListener::bind(config.http.addr).await?;
    let diagnostics_listener = match config.observability.metrics.addr {
        Some(addr) => Some(BoundListener::bind(addr).await?),
        None => None,
    };
    // template:begin grpc:bootstrap-grpc-bind-before-ready
    let grpc_listener = match grpc_prepared {
        Some(grpc) => Some((BoundListener::bind(grpc.addr).await?, grpc)),
        None => None,
    };
    // template:end grpc:bootstrap-grpc-bind-before-ready
    readiness.admit().await.map_err(BootstrapError::Admission)?;
    start_progress(readiness, background)?;

    let server_options = ServerOptions {
        header_read_timeout: config.http.header_read_timeout,
        max_header_bytes: usize::try_from(config.http.max_header_bytes.as_u64())
            .unwrap_or(usize::MAX),
        max_connections: config.http.connection_cap(),
        max_connection_age: config.http.connection_age_cap(),
    };
    let app = infra_http::harden(
        routes.with_state(state),
        &HardenOptions {
            max_body_bytes: usize::try_from(config.http.max_body_bytes.as_u64())
                .unwrap_or(usize::MAX),
            request_timeout: config.http.request_timeout,
            max_in_flight: config.http.in_flight_cap(),
            admission_wait: config.http.admission_wait_cap(),
            log_health_probes: config.http.access_log_health_probes,
            draining: Some(readiness.reader()),
        },
    );
    #[cfg(feature = "hotpath")]
    let app = hotpath::axum!(app);
    #[cfg(feature = "hotpath")]
    hotpath::tokio_runtime!();
    let app_listener = serving
        .app_listener
        .insert(app_listener.serve(app, server_options));
    background.watch_listener("http_accept", app_listener);
    tracing::info!(addr = %app_listener.local_addr(), "http listener bound");

    if let Some(listener) = diagnostics_listener {
        let diagnostics = diagnostics_router(metrics.clone()).merge(infra_http::liveness_router());
        let server = serving
            .diagnostics
            .insert(listener.serve(diagnostics, server_options));
        background.watch_listener("diagnostics_accept", server);
        tracing::info!(addr = %server.local_addr(), "diagnostics listener bound");
    }

    // template:begin grpc:bootstrap-grpc-bind
    if let Some((listener, grpc)) = grpc_listener {
        let server = match grpc.tls {
            Some(tls) => listener.serve_tls(grpc.router, grpc.options, tls),
            None => listener.serve(grpc.router, grpc.options),
        };
        let listener = serving.grpc_listener.insert(server);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_metrics() -> Metrics {
        static METRICS: std::sync::OnceLock<Metrics> = std::sync::OnceLock::new();
        METRICS
            .get_or_init(|| telemetry::install_metrics().unwrap())
            .clone()
    }

    #[tokio::test(start_paused = true)]
    async fn final_stop_retains_unreported_progress_loss_and_the_original_failure() {
        for (late, prior_failure) in [(false, false), (true, false), (true, true)] {
            let readiness = Readiness::new(
                Vec::new(),
                RefreshPolicy {
                    interval: Duration::from_secs(1),
                    probe_budget: Duration::from_secs(1),
                    failure_threshold: std::num::NonZeroU32::new(3).unwrap(),
                },
            );
            readiness.refresh().await;
            readiness.arm_progress().unwrap();
            let serving = Serving {
                readiness: Some(readiness.clone()),
                ..Serving::default()
            };
            let background = Background::new(CancellationToken::new());
            if late {
                tokio::time::advance(Duration::from_secs(5)).await;
            }
            let failure = stop_progress(
                &background,
                &serving,
                prior_failure.then_some(BootstrapError::Panicked),
            );
            match (late, prior_failure, failure) {
                (_, true, Some(BootstrapError::Panicked))
                | (
                    true,
                    false,
                    Some(BootstrapError::Background {
                        name: "readiness_progress",
                        panicked: false,
                    }),
                )
                | (false, false, None) => {}
                other => panic!("incorrect primary failure: {other:?}"),
            }
            tokio::time::advance(Duration::from_secs(5)).await;
            readiness.refresh().await;
            assert_eq!(readiness.progress_lost(), late);
            assert!(background.is_empty());
        }
    }

    #[tokio::test]
    async fn a_later_bind_failure_never_publishes_readiness() {
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = Config::default();
        config.http.addr = "127.0.0.1:0".parse().unwrap();
        config.observability.metrics.addr = Some(occupied.local_addr().unwrap());
        config.http.readiness_propagation_delay = Duration::from_secs(20);
        let metrics = shared_metrics();
        let mut background = Background::new(CancellationToken::new());
        let mut dependencies = Dependencies::default();
        let mut serving = Serving::default();
        let result = start(
            &config,
            // template:begin grpc:bootstrap-test-grpc-registration
            None,
            // template:end grpc:bootstrap-test-grpc-registration
            &metrics,
            &mut background,
            &mut dependencies,
            &mut serving,
        )
        .await;
        std::assert_matches!(
            result,
            Err(BootstrapError::Server(infra_http::ServerError::Bind { .. }))
        );
        assert!(!serving.admitted);
        assert!(serving.app_listener.is_none());
        std::assert_matches!(
            serving.readiness.as_ref().unwrap().reader().verdict(),
            Err(health::NotReady::NotEvaluated)
        );
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
                logger: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Graceful);
    }

    // template:begin service-auth:bootstrap-service-auth-test
    mod service_auth_registration {
        use std::pin::Pin;

        use grpc_contracts::example::v1::{
            BidiStreamRequest, BidiStreamResponse, ClientStreamRequest, ClientStreamResponse,
            ServerStreamRequest, ServerStreamResponse, UnaryRequest, UnaryResponse,
            echo_service_server::{EchoService, EchoServiceServer},
        };
        use tonic::{Request, Response, Status};

        type Stream<T> =
            Pin<Box<dyn tonic::codegen::tokio_stream::Stream<Item = Result<T, Status>> + Send>>;

        #[derive(Clone, Copy, Debug, Default)]
        struct Echo;

        #[tonic::async_trait]
        impl EchoService for Echo {
            type ServerStreamStream = Stream<ServerStreamResponse>;
            type BidiStreamStream = Stream<BidiStreamResponse>;

            async fn unary(
                &self,
                request: Request<UnaryRequest>,
            ) -> Result<Response<UnaryResponse>, Status> {
                let caller = request
                    .extensions()
                    .get::<infra_grpc::Caller>()
                    .map(|caller| caller.name().as_str());
                Ok(Response::new(UnaryResponse {
                    message: caller.unwrap_or("none").to_owned(),
                }))
            }

            async fn client_stream(
                &self,
                _: Request<tonic::Streaming<ClientStreamRequest>>,
            ) -> Result<Response<ClientStreamResponse>, Status> {
                Err(Status::unimplemented("unused"))
            }

            async fn server_stream(
                &self,
                _: Request<ServerStreamRequest>,
            ) -> Result<Response<Self::ServerStreamStream>, Status> {
                Err(Status::unimplemented("unused"))
            }

            async fn bidi_stream(
                &self,
                _: Request<tonic::Streaming<BidiStreamRequest>>,
            ) -> Result<Response<Self::BidiStreamStream>, Status> {
                Err(Status::unimplemented("unused"))
            }
        }

        pub(super) fn register(
            services: &mut infra_grpc::Services,
            _: &crate::AppState,
            _: &mut crate::BackgroundRegistration<'_>,
        ) -> Result<(), infra_grpc::Error> {
            services.describe(grpc_contracts::FILE_DESCRIPTOR_SET)?;
            services.add(EchoServiceServer::new(Echo))?;
            for method in ["Unary", "ClientStream", "ServerStream", "BidiStream"] {
                services
                    .allow_callers(&format!("/example.v1.EchoService/{method}"), &["orders"])?;
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_process_verifying_service_tokens_refuses_a_tokenless_grpc_call() {
        use grpc_contracts::example::v1::{UnaryRequest, echo_service_client::EchoServiceClient};
        use infra_serviceauth::test_support::{TestKey, registry};

        let key = TestKey::new("orders/1");
        let mut config = Config::default();
        config.http.addr = "127.0.0.1:0".parse().unwrap();
        config.grpc = Some(service_config::GrpcConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
            security: service_config::GrpcListenerSecurity::Plaintext,
            request_timeout: Duration::from_secs(8),
            max_in_flight: 256,
            admission_wait: Duration::from_secs(1),
            max_connections: 4096,
            max_connection_age: Duration::from_mins(30),
        });
        config.service_auth = service_config::ServiceAuthConfig::Jwt {
            name: "catalog".to_owned(),
            jwt_type: "example-svc+jwt".to_owned(),
            trust: registry(&[("orders", &[&key])]),
            signing_key: None,
        };
        let metrics = shared_metrics();
        let mut background = Background::new(CancellationToken::new());
        let mut dependencies = Dependencies::default();
        let mut serving = Serving::default();
        start(
            &config,
            Some(service_auth_registration::register),
            &metrics,
            &mut background,
            &mut dependencies,
            &mut serving,
        )
        .await
        .unwrap();
        let address = serving.grpc_listener.as_ref().unwrap().local_addr();
        let resource = infra_grpc::Client::try_new(
            &format!("http://{address}"),
            infra_grpc::ClientSecurity::Plaintext,
            Duration::from_secs(5),
        )
        .unwrap();
        let status = EchoServiceClient::new(resource.clone())
            .unary(UnaryRequest::default())
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
        let signing = infra_serviceauth::TokenSource::try_new(infra_serviceauth::SigningOptions {
            name: infra_serviceauth::ServiceName::parse("orders").unwrap(),
            jwt_type: infra_serviceauth::JwtType::parse("example-svc+jwt").unwrap(),
            key_id: infra_serviceauth::KeyId::parse(key.kid()).unwrap(),
            private_key: key.pem().into(),
        })
        .unwrap();
        let admitted = EchoServiceClient::new(
            resource.with_service_tokens(signing.receiver("catalog").unwrap()),
        )
        .unary(UnaryRequest::default())
        .await
        .unwrap();
        assert_eq!(admitted.into_inner().message, "orders");
        let mut signals = Signals::install().unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            shutdown::run(shutdown::Plan {
                http_config: &config.http,
                signals: &mut signals,
                deadline: Instant::now() + config.http.grace_period,
                serving,
                background,
                dependencies,
                tracer_provider: None,
                logger: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Graceful);
    }
    // template:end service-auth:bootstrap-service-auth-test
}
