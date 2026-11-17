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
use std::io::Write as _;
use std::panic::AssertUnwindSafe;
use std::process::ExitCode;
use std::time::Duration;

use futures_util::FutureExt;

use health::{PreparedReadiness, Probe, Readiness, RefreshPolicy};
use infra_http::{Authentication, HardenOptions};
use infra_http_transport::{BoundListener, ConnectionLimits, Http1Options};
use infra_telemetry::{
    LoggingFormat, LoggingOptions, Metrics, diagnostics_router, install_panic_hook,
    install_subscriber, install_tracer_provider, runtime_metrics,
};
use service_config::{BuildInfo, Config, LoadOptions, LogFormat};
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
    #[error("inbound webhook endpoint {endpoint} key {key} is invalid: {source}")]
    InboundWebhookKey {
        endpoint: String,
        key: String,
        #[source]
        source: infra_webhooks::protocol::InvalidKey,
    },
    // template:end inbound-webhooks:bootstrap-webhooks-errors
    #[error(transparent)]
    Server(#[from] infra_http_transport::ServerError),
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
    reason = "the postgres profile is optional; a dependency-free service retains no blocking startup call"
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

#[expect(
    clippy::disallowed_methods,
    reason = "terminal diagnostic before tracing is available"
)]
fn process_failure(message: impl std::fmt::Display) -> ExitCode {
    let _ = writeln!(std::io::stderr().lock(), "{message}");
    ExitCode::FAILURE
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
    let options = match LoadOptions::try_parse_from(args) {
        Ok(options) => options,
        Err(error) => {
            return if error.print().is_ok() {
                ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(1))
            } else {
                ExitCode::FAILURE
            };
        }
    };
    let config = match service_config::load(&options, BUILD_INFO, shutdown::TAIL) {
        Ok(config) => config,
        Err(err) => return process_failure(&err),
    };
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
                let provider = tracer_provider.insert(install_tracer_provider(
                    &telemetry::tracing_options(&config),
                )?);
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
                raise_open_file_limit(&config);
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
                stopped = signals.wait() => stopped.map(|()| None).map_err(BootstrapError::from),
                failed = background_failure(&mut observer) => Err(background_error(failed)),
                started = startup => started.map(Some),
            }
        };
        if let Err(error) = finish_startup(race, signals, &mut background, &mut serving) {
            return Some(error);
        }
        if !serving.admitted {
            return None;
        }
        tokio::select! {
            biased;
            stopped = signals.wait() => stopped.err().map(BootstrapError::from),
            failed = background_failure(&mut observer) => Some(background_error(failed)),
        }
    })
    .catch_unwind()
    .await
    .unwrap_or(Some(BootstrapError::Panicked));
    let failure = failure.or_else(|| pending_failure(&background, &serving));
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

fn raise_open_file_limit(config: &Config) {
    let connection_caps = config.finite_listener_connection_caps();
    match rlimit::increase_nofile_limit(u64::MAX) {
        Err(error) => tracing::warn!(error = %error, "open_file_limit_unchanged"),
        Ok(open_file_limit) if open_file_limit < connection_caps => {
            tracing::warn!(
                open_file_limit,
                connection_caps,
                "open_file_limit_below_connection_caps"
            );
        }
        Ok(_) => {}
    }
}

fn finish_startup(
    started: Result<Option<PreparedReadiness>, BootstrapError>,
    signals: &mut Signals,
    background: &mut Background,
    serving: &mut Serving,
) -> Result<(), BootstrapError> {
    if let Some(error) = pending_failure(background, serving) {
        return Err(error);
    }
    if let Some(prepared) = started?
        && !signals.pending()?
    {
        let refresher = prepared.activate().map_err(BootstrapError::Admission)?;
        serving.admitted = true;
        background.spawn(
            "readiness",
            refresher.refresh_until(background.child_token()),
        );
        tracing::info!("service_ready");
    }
    Ok(())
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
                panicked: failure == infra_http_transport::AcceptFailure::Panicked,
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
) -> Result<PreparedReadiness, BootstrapError> {
    #[allow(
        unused_mut,
        reason = "profiles without PostgreSQL or messaging have no probe"
    )]
    let mut probes: Vec<Box<dyn Probe>> = Vec::new();
    // template:begin authn:bootstrap-authn-prepare
    let bearer = authn::prepare(config, background).await?;
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
    // template:begin inbound-webhooks:bootstrap-webhooks-prepare
    let webhook_state =
        inbound_webhooks::prepare(config, dependencies.postgres.as_ref(), background)?;
    // template:end inbound-webhooks:bootstrap-webhooks-prepare
    let readiness = Readiness::new(
        probes,
        RefreshPolicy::try_new(
            config.health.refresh_interval,
            config.health.probe_budget,
            config.health.failure_threshold,
        )
        .map_err(|err| service_config::ValidationError::new("health", err.to_string()))?,
    );
    let control = serving.readiness.insert(readiness.control());

    let state = crate::AppState {
        readiness: control.reader(),
        // template:begin inbound-webhooks:bootstrap-webhooks-route-state
        webhooks: webhook_state,
        // template:end inbound-webhooks:bootstrap-webhooks-route-state
        // template:begin webhooks:bootstrap-outbound-webhooks-state
        outbound_webhooks: infra_webhooks::outbound::Producer::new(
            config.outbound_webhook_endpoints.iter().cloned(),
        ),
        // template:end webhooks:bootstrap-outbound-webhooks-state
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
            // template:end grpc:bootstrap-grpc-prepare-start
            // template:begin grpc-authn:bootstrap-grpc-bearer-argument
            bearer.as_ref(),
            // template:end grpc-authn:bootstrap-grpc-bearer-argument
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
    let auth = Authentication::Public;
    // template:begin authn:bootstrap-http-bearer-authentication
    let auth = bearer.map_or(auth, |verifier| Authentication::Bearer(Box::new(verifier)));
    // template:end authn:bootstrap-http-bearer-authentication
    let app = http_application(
        state,
        auth,
        config,
        // template:begin http-idempotency:bootstrap-http-idempotency-resources
        dependencies.postgres.as_ref(),
        background,
        // template:end http-idempotency:bootstrap-http-idempotency-resources
    )
    .await?;
    let http = config.http.listener();
    let app_listener = BoundListener::bind(http.addr)?;
    let diagnostics_listener = match config.observability.metrics.addr {
        Some(addr) => Some(BoundListener::bind(addr)?),
        None => None,
    };
    // template:begin grpc:bootstrap-grpc-bind-before-ready
    let grpc_listener = match grpc_prepared {
        Some(grpc) => Some((BoundListener::bind(grpc.addr)?, grpc)),
        None => None,
    };
    // template:end grpc:bootstrap-grpc-bind-before-ready
    let mut prepared = readiness
        .prepare()
        .await
        .map_err(BootstrapError::Admission)?;

    let server_options = Http1Options {
        header_read_timeout: http.header_read_timeout,
        max_read_buffer_bytes: usize::try_from(http.max_header_bytes.as_u64())
            .unwrap_or(usize::MAX),
        connections: ConnectionLimits {
            max_connections: http.max_connections,
            max_connection_age: http.max_connection_age,
        },
    };
    #[cfg(feature = "hotpath")]
    hotpath::tokio_runtime!();
    let app_listener = serving
        .app_listener
        .insert(app.serve(app_listener, server_options));
    background.watch_listener("http_accept", app_listener);
    tracing::info!(addr = %app_listener.local_addr(), "http listener bound");

    if let Some(listener) = diagnostics_listener {
        let reader = control.reader();
        let diagnostics = diagnostics_router(metrics.clone(), move || reader.is_ready())
            .merge(infra_http::liveness_router());
        let server = serving
            .diagnostics
            .insert(listener.serve(diagnostics, server_options));
        background.watch_listener("diagnostics_accept", server);
        tracing::info!(addr = %server.local_addr(), "diagnostics listener bound");
    }

    // template:begin grpc:bootstrap-grpc-bind
    if let Some((listener, grpc)) = grpc_listener {
        let server = grpc.app.serve(listener, grpc.options, grpc.tls);
        let listener = serving.grpc_listener.insert(server);
        background.watch_listener("grpc_accept", listener);
        tracing::info!(addr = %listener.local_addr(), "grpc listener bound");
    }
    // template:end grpc:bootstrap-grpc-bind

    prepared
        .ensure_fresh()
        .await
        .map_err(BootstrapError::Admission)?;
    Ok(prepared)
}

#[allow(
    clippy::unused_async,
    reason = "only the idempotency profile checks the store before serving"
)]
async fn http_application(
    state: crate::AppState,
    auth: Authentication,
    config: &Config,
    // template:begin http-idempotency:bootstrap-http-idempotency-inputs
    pool: Option<&infra_postgres::PgPool>,
    background: &mut Background,
    // template:end http-idempotency:bootstrap-http-idempotency-inputs
) -> Result<infra_http::Application, BootstrapError> {
    let contract = crate::api::contract()?;
    let http = config.http.listener();
    let options = HardenOptions {
        max_body_bytes: usize::try_from(http.max_body_bytes.as_u64()).unwrap_or(usize::MAX),
        request_timeout: http.request_timeout,
        max_in_flight: http.max_in_flight,
        admission_wait: http.admission_wait,
        log_health_probes: http.access_log_health_probes,
        draining: Some(state.readiness.clone()),
    };
    // template:begin http-idempotency:bootstrap-http-idempotency-activation
    if let Some(mounted) = idempotency::prepare(contract.idempotent_operations(), config, pool)? {
        let app =
            contract.build_with_idempotency_store(state, auth, &options, mounted.store.clone())?;
        mounted.activate(background).await?;
        return Ok(app);
    }
    // template:end http-idempotency:bootstrap-http-idempotency-activation
    Ok(contract.build(state, auth, &options)?)
}

#[cfg(test)]
fn test_config(environment: &[(&str, &str)]) -> Config {
    service_config::fixtures::service(environment, BUILD_INFO, shutdown::TAIL).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_blocking_startup_operation_leaves_the_stop_waiter_runnable() {
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, held) = std::sync::mpsc::channel();
        let admission = blocking_startup("test", move || {
            entered.send(()).unwrap();
            held.recv_timeout(Duration::from_secs(5))
                .expect("stop waiter releases startup");
            Ok::<(), BootstrapError>(())
        });
        tokio::select! {
            result = admission => panic!("startup finished before the stop waiter: {result:?}"),
            result = started => result.unwrap(),
        }
        release.send(()).unwrap();
    }

    fn shared_metrics() -> Metrics {
        static METRICS: std::sync::OnceLock<Metrics> = std::sync::OnceLock::new();
        METRICS
            .get_or_init(|| telemetry::install_metrics().unwrap())
            .clone()
    }

    #[tokio::test(start_paused = true)]
    async fn diagnostics_scrapes_report_readiness_expiry_without_another_check() {
        use axum::body::{Body, to_bytes};
        use axum::http::Request;
        use tower::ServiceExt as _;

        let metrics = shared_metrics();
        let policy = RefreshPolicy::try_new(
            Duration::from_secs(1),
            Duration::from_millis(100),
            std::num::NonZeroU32::MIN,
        )
        .unwrap();
        let readiness = Readiness::new(Vec::new(), policy);
        let control = readiness.control();
        let reader = control.reader();
        let diagnostics = diagnostics_router(metrics, move || reader.is_ready());
        let scrape = async |sample: &str| {
            let request = Request::get("/metrics").body(Body::empty()).unwrap();
            let response = diagnostics.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body = std::str::from_utf8(&body).unwrap();
            assert_eq!(
                body.lines()
                    .filter(|line| line.starts_with("readiness_ready "))
                    .collect::<Vec<_>>(),
                [sample]
            );
        };

        scrape("readiness_ready 0").await;
        let prepared = readiness.prepare().await.unwrap();
        scrape("readiness_ready 0").await;
        let refresher = prepared.activate().unwrap();
        tokio::time::advance(policy.stale_after()).await;
        scrape("readiness_ready 1").await;
        tokio::time::advance(Duration::from_nanos(1)).await;
        scrape("readiness_ready 0").await;
        let cancel = CancellationToken::new();
        let refreshing = tokio::spawn(refresher.refresh_until(cancel.clone()));
        tokio::time::sleep(Duration::from_secs(1)).await;
        scrape("readiness_ready 1").await;
        control.start_drain();
        scrape("readiness_ready 0").await;
        cancel.cancel();
        refreshing.await.unwrap();
    }

    #[tokio::test]
    async fn a_pending_background_failure_keeps_the_serving_http_listener_unready() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

        let config = test_config(&[
            ("APP__HTTP__ADDR", "127.0.0.1:0"),
            ("APP__OBSERVABILITY__METRICS__ADDR", ""),
        ]);
        let mut background = Background::new(CancellationToken::new());
        let mut serving = Serving::default();
        let prepared = start(
            &config,
            // template:begin grpc:bootstrap-unready-test-grpc-registration
            None,
            // template:end grpc:bootstrap-unready-test-grpc-registration
            &shared_metrics(),
            &mut background,
            &mut Dependencies::default(),
            &mut serving,
        )
        .await
        .unwrap();
        let address = serving.app_listener.as_ref().unwrap().local_addr();
        let status = tokio::time::timeout(Duration::from_secs(2), async {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream
                .write_all(b"GET /health/ready HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await
                .unwrap();
            let mut status = String::new();
            BufReader::new(stream).read_line(&mut status).await.unwrap();
            status
        })
        .await
        .unwrap();
        assert!(status.starts_with("HTTP/1.1 503 "), "{status}");

        background.spawn("startup_task", async {});
        tokio::time::timeout(
            Duration::from_secs(2),
            background_failure(&mut background.observer()),
        )
        .await
        .unwrap();
        std::assert_matches!(
            finish_startup(
                Ok(Some(prepared)),
                &mut Signals::install().unwrap(),
                &mut background,
                &mut serving
            ),
            Err(BootstrapError::Background {
                name: "startup_task",
                panicked: false
            })
        );
        assert!(!serving.admitted);
        std::assert_matches!(
            serving.readiness.as_ref().unwrap().reader().verdict(),
            Err(health::NotReady::NotEvaluated)
        );
        background.cancel();
    }

    #[tokio::test]
    async fn a_later_bind_failure_never_publishes_readiness() {
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = test_config(&[
            ("APP__HTTP__ADDR", "127.0.0.1:0"),
            (
                "APP__OBSERVABILITY__METRICS__ADDR",
                &occupied.local_addr().unwrap().to_string(),
            ),
            ("APP__HTTP__READINESS_PROPAGATION_DELAY", "20s"),
            ("APP__HTTP__REQUEST_TIMEOUT", "3s"),
        ]);
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
            Err(BootstrapError::Server(
                infra_http_transport::ServerError::Bind { .. }
            ))
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
        let config = test_config(&[
            ("APP__APP__ENV", "local"),
            ("APP__HTTP__ADDR", "127.0.0.1:0"),
            ("APP__HTTP__READINESS_PROPAGATION_DELAY", "0s"),
            ("APP__GRPC__ENABLED", "true"),
            ("APP__GRPC__ADDR", "127.0.0.1:0"),
            ("APP__GRPC__SECURITY", "plaintext"),
            ("APP__OBSERVABILITY__METRICS__ADDR", ""),
            ("APP__SERVICE_AUTH__MODE", "jwt"),
            ("APP__SERVICE_AUTH__NAME", "catalog"),
            ("APP__SERVICE_AUTH__JWT_TYPE", "example-svc+jwt"),
            (
                "APP__SERVICE_AUTH__TRUST",
                &registry(&[("orders", &[&key])]),
            ),
        ]);
        let metrics = shared_metrics();
        let mut background = Background::new(CancellationToken::new());
        let mut dependencies = Dependencies::default();
        let mut serving = Serving::default();
        let prepared = start(
            &config,
            Some(service_auth_registration::register),
            &metrics,
            &mut background,
            &mut dependencies,
            &mut serving,
        )
        .await
        .unwrap();
        let mut signals = Signals::install().unwrap();
        finish_startup(
            Ok(Some(prepared)),
            &mut signals,
            &mut background,
            &mut serving,
        )
        .unwrap();
        let address = serving.grpc_listener.as_ref().unwrap().local_addr();
        let resource = infra_outbound_grpc::Client::try_new(
            &format!("http://{address}"),
            infra_outbound_grpc::ClientSecurity::Plaintext,
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
