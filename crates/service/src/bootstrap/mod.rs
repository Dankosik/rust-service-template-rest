mod background;
// template:begin outbound-auth:bootstrap-outbound-resource-tests
#[cfg(test)]
pub(crate) mod outbound_tests;
// template:end outbound-auth:bootstrap-outbound-resource-tests
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
    LoggerGuard, LoggingFormat, LoggingOptions, Metrics, TracerProviderHandle, diagnostics_router,
    install_panic_hook, install_subscriber, install_tracer_provider, runtime_metrics,
};
use service_config::{BuildInfo, Config, LoadOptions, LogFormat};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use self::background::{Background, BackgroundFailure, wait_for_failure};
pub use self::background::{BackgroundFailureReporter, BackgroundRegistration};
use self::shutdown::{Dependencies, Outcome, Serving};
use self::signals::Signals;
use crate::{RegistrationError, ServiceRegistration};

pub(crate) const BUILD_INFO: BuildInfo = BuildInfo::from_package_version(env!("CARGO_PKG_VERSION"));

const EXIT_DEGRADED_SHUTDOWN: u8 = 3;

const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

const METRICS_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(10);

const HTTP_ACCEPT_TASK: &str = "http_accept";
const DIAGNOSTICS_ACCEPT_TASK: &str = "diagnostics_accept";
// template:begin grpc:bootstrap-grpc-accept-task
const GRPC_ACCEPT_TASK: &str = "grpc_accept";
// template:end grpc:bootstrap-grpc-accept-task

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
    Cache(#[from] infra_cache::StartError),
    // template:end cache:service-bootstrap-cache-errors
    // template:begin object-storage:service-bootstrap-object-storage-errors
    #[error("object storage configuration: {0}")]
    ObjectStorage(#[from] infra_object_storage::ConfigError),
    // template:end object-storage:service-bootstrap-object-storage-errors
    #[error("startup admission: {0}")]
    Admission(#[from] health::NotReady),
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
    #[error("HTTP resource preparation failed")]
    HttpResource(#[source] RegistrationError),
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
    #[error(transparent)]
    InboundWebhookKeyReuse(#[from] infra_webhooks::protocol::SharedEndpointKey),
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
    #[error("gRPC service registration: {0}")]
    GrpcRegistration(#[source] RegistrationError),
    // template:end grpc:bootstrap-grpc-error
    #[error(transparent)]
    Background(#[from] BackgroundFailure),
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
fn fail_to_stderr(message: impl std::fmt::Display) -> ExitCode {
    let _ = writeln!(std::io::stderr().lock(), "{message}");
    ExitCode::FAILURE
}

pub(super) fn run_registered<I>(args: I, registration: ServiceRegistration) -> ExitCode
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
        Err(err) => return fail_to_stderr(&err),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.runtime.effective_worker_threads())
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return fail_to_stderr(format!("build tokio runtime: {err}")),
    };

    let mut exit = RuntimeExit::default();
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(serve(config, registration, &mut exit))
    }))
    .unwrap_or(Err(BootstrapError::Panicked));
    let remaining = exit.deadline.map_or(RUNTIME_SHUTDOWN_TIMEOUT, |deadline| {
        RUNTIME_SHUTDOWN_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()))
    });
    runtime.shutdown_timeout(remaining);
    drop(exit.signals);

    match outcome {
        Ok(Outcome::Graceful) => ExitCode::SUCCESS,
        Ok(Outcome::Degraded) => ExitCode::from(EXIT_DEGRADED_SHUTDOWN),
        Err(err) => {
            if exit.logger_installed {
                ExitCode::FAILURE
            } else {
                fail_to_stderr(&err)
            }
        }
    }
}

#[derive(Default)]
struct RuntimeExit {
    logger_installed: bool,
    signals: Option<Signals>,
    deadline: Option<Instant>,
}

#[derive(Default)]
struct TelemetryHandles {
    tracer_provider: Option<TracerProviderHandle>,
    logger: Option<LoggerGuard>,
}

#[allow(
    clippy::too_many_lines,
    reason = "retained lifecycle state stays beside its single unwind and teardown boundary"
)]
async fn serve(
    config: Config,
    registration: ServiceRegistration,
    exit: &mut RuntimeExit,
) -> Result<Outcome, BootstrapError> {
    let signals = exit
        .signals
        .insert(Signals::install().map_err(BootstrapError::Signals)?);
    let config = Box::new(config);
    let http_config = config.http.clone();
    let mut background = Background::new(CancellationToken::new());
    let mut background_failures = background.failure_observer();
    let mut dependencies = Dependencies::default();
    let mut serving = Serving::default();
    let mut telemetry = TelemetryHandles::default();

    let failure = AssertUnwindSafe(async {
        let startup = async {
            let metrics = install_telemetry(
                &config,
                &mut background,
                &mut telemetry,
                &mut exit.logger_installed,
            )
            .await?;
            raise_open_file_limit(&config);
            start(
                &config,
                registration,
                &metrics,
                &mut background,
                &mut dependencies,
                &mut serving,
            )
            .await
        };
        let startup_result = tokio::select! {
            biased;
            stopped = signals.wait() => stopped.map(|()| None).map_err(BootstrapError::from),
            failed = wait_for_failure(&mut background_failures) => Err(failed.into()),
            started = startup => started.map(Some),
        };
        drop(config);
        finish_startup(startup_result, signals, &mut background, &mut serving)?;
        if !serving.admitted {
            return Ok(());
        }
        tokio::select! {
            biased;
            stopped = signals.wait() => stopped.map_err(BootstrapError::from),
            failed = wait_for_failure(&mut background_failures) => Err(failed.into()),
        }
    })
    .catch_unwind()
    .await
    .unwrap_or(Err(BootstrapError::Panicked))
    .err()
    .or_else(|| pending_failure(&background, &serving));
    let deadline = *exit
        .deadline
        .insert(signals.first_stop().unwrap_or_else(Instant::now) + http_config.grace_period);
    if let Some(error) = &failure
        && telemetry.logger.is_some()
    {
        tracing::error!(error = %error, "service failed");
    }
    let outcome = shutdown::run(shutdown::Plan {
        http_config: &http_config,
        signals,
        deadline,
        serving,
        background,
        dependencies,
        tracer_provider: telemetry.tracer_provider,
        logger: telemetry.logger,
    })
    .await;
    match failure {
        Some(error) => Err(error),
        None => Ok(outcome),
    }
}

async fn install_telemetry(
    config: &Config,
    background: &mut Background,
    handles: &mut TelemetryHandles,
    logger_installed: &mut bool,
) -> Result<Metrics, BootstrapError> {
    let provider = handles
        .tracer_provider
        .insert(install_tracer_provider(&telemetry::tracing_options(config)).await?);
    handles.logger = Some(install_subscriber(&LoggingOptions {
        level: &config.log.level,
        format: match config.log.format {
            LogFormat::Json => LoggingFormat::Json,
            LogFormat::Text => LoggingFormat::Text,
        },
        tracer_provider: Some(provider),
    })?);
    *logger_installed = true;
    install_panic_hook();
    let metrics = telemetry::install_metrics()?;
    metrics.record_trace_exporter(provider.exporter_state());
    telemetry::log_startup_summary(config, provider.exporter_state());
    spawn_metrics_tasks(&metrics, background);
    Ok(metrics)
}

fn spawn_metrics_tasks(metrics: &Metrics, background: &mut Background) {
    background.spawn_with_token("metrics_upkeep", |stop| metrics.clone().upkeep(stop));
    background.spawn_with_token("runtime_progress", |stop| {
        metrics.clone().runtime_progress(stop)
    });
    background.spawn_with_token("runtime_metrics", |stop| {
        runtime_metrics(METRICS_MAINTENANCE_INTERVAL, stop)
    });
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
        && !signals.take_pending()?
    {
        let refresher = prepared.activate()?;
        serving.admitted = true;
        background.spawn_with_token("readiness", |stop| refresher.refresh_until(stop));
        tracing::info!("service_ready");
    }
    Ok(())
}

fn pending_failure(background: &Background, serving: &Serving) -> Option<BootstrapError> {
    if let Some(failure) = background.failure() {
        return Some(failure.into());
    }
    for (name, server) in [
        (HTTP_ACCEPT_TASK, serving.http_server.as_ref()),
        (DIAGNOSTICS_ACCEPT_TASK, serving.diagnostics_server.as_ref()),
        // template:begin grpc:bootstrap-pending-grpc
        (GRPC_ACCEPT_TASK, serving.grpc_server.as_ref()),
        // template:end grpc:bootstrap-pending-grpc
    ] {
        if let Some(server) = server
            && let Some(failure) = server.accept_failure().now_or_never()
        {
            return Some(
                BackgroundFailure {
                    name,
                    panicked: failure == infra_http_transport::AcceptFailure::Panicked,
                }
                .into(),
            );
        }
    }
    None
}

#[allow(
    clippy::too_many_lines,
    reason = "startup is one ordered sequence; each step names the profile that owns it"
)]
async fn start(
    config: &Config,
    registration: ServiceRegistration,
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
    if let Some(postgres) = &config.postgres {
        let pool = postgres::open(postgres, config, background, &mut dependencies.postgres).await?;
        probes.push(Box::new(infra_postgres::PostgresProbe::new(pool.clone())));
        migrate::verify_history(pool).await?;
    }
    // template:end postgres:bootstrap-postgres-startup
    // template:begin cache:service-bootstrap-cache-startup
    if let Some(settings) = &config.cache {
        dependencies.cache = Some(cache::open(settings).await?);
    }
    // template:end cache:service-bootstrap-cache-startup
    // template:begin object-storage:service-bootstrap-object-storage-startup
    dependencies.object_storage = config
        .object_storage
        .as_ref()
        .map(object_storage::open)
        .transpose()?;
    // template:end object-storage:service-bootstrap-object-storage-startup
    // template:begin inbound-webhooks:bootstrap-webhooks-prepare
    let webhook_state = inbound_webhooks::prepare(config, dependencies.postgres.as_ref())?;
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
    let readiness_control = serving.readiness.insert(readiness.control());

    let state = crate::AppState {
        readiness: readiness_control.reader(),
        // template:begin inbound-webhooks:bootstrap-webhooks-route-state
        webhooks: webhook_state,
        // template:end inbound-webhooks:bootstrap-webhooks-route-state
        // template:begin webhooks:bootstrap-outbound-webhooks-state
        outbound_webhooks: infra_webhooks::outbound::Producer::new(
            config.outbound_webhook_endpoints.iter().cloned(),
        ),
        // template:end webhooks:bootstrap-outbound-webhooks-state
        // template:begin service-auth:bootstrap-service-auth-route-state
        service_tokens: service_auth.tokens,
        // template:end service-auth:bootstrap-service-auth-route-state
    };

    // template:begin grpc:bootstrap-grpc-prepare-start
    let grpc_prepared = if let Some(grpc_config) = &config.grpc {
        Some(grpc::prepare(
            grpc_config,
            registration.grpc,
            &state,
            // template:end grpc:bootstrap-grpc-prepare-start
            // template:begin grpc-authn:bootstrap-grpc-bearer-argument
            bearer.as_ref(),
            // template:end grpc-authn:bootstrap-grpc-bearer-argument
            // template:begin service-auth:bootstrap-grpc-service-auth-argument
            service_auth.verifier,
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
    let app = match registration.http_resource {
        Some(attach) => attach(app, config, &mut BackgroundRegistration { background })
            .map_err(BootstrapError::HttpResource)?,
        None => app,
    };
    let listeners = BoundListeners::bind(
        config,
        // template:begin grpc:bootstrap-grpc-bind-argument
        grpc_prepared,
        // template:end grpc:bootstrap-grpc-bind-argument
    )?;
    let mut prepared_readiness = readiness.prepare().await?;

    listeners.serve(
        app,
        metrics,
        readiness_control.reader(),
        background,
        serving,
    )?;

    prepared_readiness.ensure_fresh().await?;
    Ok(prepared_readiness)
}

struct BoundListeners {
    http: BoundListener,
    http_options: Http1Options,
    diagnostics: Option<BoundListener>,
    // template:begin grpc:bootstrap-grpc-bound-field
    grpc: Option<(BoundListener, grpc::Prepared)>,
    // template:end grpc:bootstrap-grpc-bound-field
}

fn http1_options(http: &service_config::HttpListenerConfig) -> Http1Options {
    Http1Options {
        header_read_timeout: http.header_read_timeout,
        max_read_buffer_bytes: usize::try_from(http.max_header_bytes.as_u64())
            .unwrap_or(usize::MAX),
        connections: ConnectionLimits {
            max_connections: http.max_connections,
            max_connection_age: http.max_connection_age,
        },
    }
}

impl BoundListeners {
    fn bind(
        config: &Config,
        // template:begin grpc:bootstrap-grpc-bind-input
        grpc_prepared: Option<grpc::Prepared>,
        // template:end grpc:bootstrap-grpc-bind-input
    ) -> Result<Self, BootstrapError> {
        let http = config.http.listener();
        let http_listener = BoundListener::bind(http.addr)?;
        let diagnostics = config
            .observability
            .metrics
            .addr
            .map(BoundListener::bind)
            .transpose()?;
        // template:begin grpc:bootstrap-grpc-bind-before-ready
        let grpc = grpc_prepared
            .map(|grpc| BoundListener::bind(grpc.addr).map(|listener| (listener, grpc)))
            .transpose()?;
        // template:end grpc:bootstrap-grpc-bind-before-ready
        Ok(Self {
            http: http_listener,
            http_options: http1_options(http),
            diagnostics,
            // template:begin grpc:bootstrap-grpc-bound-value
            grpc,
            // template:end grpc:bootstrap-grpc-bound-value
        })
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "only the gRPC profile can fail when starting a bound listener"
    )]
    fn serve(
        self,
        app: infra_http::Application,
        metrics: &Metrics,
        readiness: health::ReadinessReader,
        background: &mut Background,
        serving: &mut Serving,
    ) -> Result<(), BootstrapError> {
        #[cfg(feature = "hotpath")]
        hotpath::tokio_runtime!();
        let http_server = serving
            .http_server
            .insert(app.serve(self.http, self.http_options));
        background.watch_server(HTTP_ACCEPT_TASK, http_server);
        tracing::info!(addr = %http_server.local_addr(), "http listener bound");

        if let Some(listener) = self.diagnostics {
            let diagnostics = diagnostics_router(metrics.clone(), move || readiness.is_ready())
                .merge(infra_http::liveness_router());
            let diagnostics_server = serving
                .diagnostics_server
                .insert(listener.serve(diagnostics, self.http_options));
            background.watch_server(DIAGNOSTICS_ACCEPT_TASK, diagnostics_server);
            tracing::info!(
                addr = %diagnostics_server.local_addr(),
                "diagnostics listener bound"
            );
        }

        // template:begin grpc:bootstrap-grpc-bind
        if let Some((listener, grpc)) = self.grpc {
            let grpc_server =
                serving
                    .grpc_server
                    .insert(grpc.app.serve(listener, grpc.options, grpc.tls)?);
            background.watch_server(GRPC_ACCEPT_TASK, grpc_server);
            tracing::info!(addr = %grpc_server.local_addr(), "grpc listener bound");
        }
        // template:end grpc:bootstrap-grpc-bind

        Ok(())
    }
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
        trust_trace_context: http.trust_trace_context,
        draining: Some(state.readiness.clone()),
    };
    // template:begin http-idempotency:bootstrap-http-idempotency-activation
    if let Some(idempotency) = idempotency::prepare(contract.idempotent_operations(), config, pool)?
    {
        let app = contract.build_with_idempotency_store(
            state,
            auth,
            &options,
            idempotency.store.clone(),
        )?;
        idempotency.activate(background).await?;
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

    pub(super) fn shared_metrics() -> Metrics {
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
                    .filter(|line| {
                        line.starts_with("# HELP readiness_ready ")
                            || line.starts_with("# TYPE readiness_ready ")
                            || line.starts_with("readiness_ready ")
                    })
                    .collect::<Vec<_>>(),
                [
                    "# HELP readiness_ready 1 if ready at scrape time, 0 otherwise.",
                    "# TYPE readiness_ready gauge",
                    sample,
                ]
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
            crate::ServiceRegistration::default(),
            &shared_metrics(),
            &mut background,
            &mut Dependencies::default(),
            &mut serving,
        )
        .await
        .unwrap();
        let address = serving.http_server.as_ref().unwrap().local_addr();
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
            wait_for_failure(&mut background.failure_observer()),
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
            Err(BootstrapError::Background(BackgroundFailure {
                name: "startup_task",
                panicked: false
            }))
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
            crate::ServiceRegistration::default(),
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
        assert!(serving.http_server.is_none());
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
        use std::time::Duration;

        use grpc_contracts::example::v1::{
            BidiStreamRequest, BidiStreamResponse, ClientStreamRequest, ClientStreamResponse,
            ServerStreamRequest, ServerStreamResponse, UnaryRequest, UnaryResponse,
            echo_service_server::{EchoService, EchoServiceServer},
        };
        use infra_outbound_grpc::{Client, ClientSecurity};
        use tonic::{Request, Response, Status};
        use tonic_reflection::pb::v1::{
            ServerReflectionRequest, server_reflection_client::ServerReflectionClient,
            server_reflection_request::MessageRequest, server_reflection_response::MessageResponse,
        };

        type Stream<T> =
            Pin<Box<dyn tonic::codegen::tokio_stream::Stream<Item = Result<T, Status>> + Send>>;

        #[derive(Clone, Debug)]
        struct Echo(ServerReflectionClient<Client>);

        #[tonic::async_trait]
        impl EchoService for Echo {
            type ServerStreamStream = Stream<ServerStreamResponse>;
            type BidiStreamStream = Stream<BidiStreamResponse>;

            async fn unary(
                &self,
                request: Request<UnaryRequest>,
            ) -> Result<Response<UnaryResponse>, Status> {
                let caller = infra_grpc::require_caller(&request)?;
                let query = ServerReflectionRequest {
                    message_request: Some(MessageRequest::ListServices(String::new())),
                    ..Default::default()
                };
                let mut directory = self.0.clone();
                let mut replies = directory
                    .server_reflection_info(futures_util::stream::iter([query]))
                    .await?
                    .into_inner();
                let Some(MessageResponse::ListServicesResponse(list)) = replies
                    .message()
                    .await?
                    .and_then(|reply| reply.message_response)
                else {
                    return Err(Status::unavailable("directory did not list services"));
                };
                let service = list
                    .service
                    .into_iter()
                    .find(|service| service.name == caller.name().as_str())
                    .ok_or_else(|| Status::not_found("caller missing from directory"))?;
                Ok(Response::new(UnaryResponse {
                    message: service.name,
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

        pub(super) fn directory() -> infra_http_transport::Server {
            let directory = tonic_reflection::server::Builder::configure()
                .with_service_name("orders")
                .build_v1()
                .unwrap();
            super::BoundListener::bind("127.0.0.1:0".parse().unwrap())
                .unwrap()
                .serve_h2(
                    tonic::service::Routes::new(directory).into_axum_router(),
                    infra_grpc::server_options(super::ConnectionLimits {
                        max_connections: None,
                        max_connection_age: None,
                    }),
                    None,
                )
        }

        pub(super) fn registration(origin: String) -> super::ServiceRegistration {
            super::ServiceRegistration::default().with_grpc(move |services, _, _| {
                let directory =
                    Client::try_new(&origin, ClientSecurity::Plaintext, Duration::from_secs(5))?;
                services.set_listener_layer(tower::util::MapResponseLayer::new(
                    |mut response: axum::response::Response| {
                        response
                            .headers_mut()
                            .insert("x-listener", "grpc".parse().unwrap());
                        response
                    },
                ));
                services.describe(grpc_contracts::FILE_DESCRIPTOR_SET)?;
                services.add(EchoServiceServer::new(Echo(ServerReflectionClient::new(
                    directory,
                ))))?;
                services.add_reflection()?;
                for method in ["Unary", "ClientStream", "ServerStream", "BidiStream"] {
                    services
                        .allow_callers(&format!("/example.v1.EchoService/{method}"), &["orders"])?;
                }
                for path in infra_grpc::REFLECTION_PATHS {
                    services.allow_callers(path, &["orders"])?;
                }
                Ok(())
            })
        }

        pub(super) async fn exercise_listener(
            resource: Client,
            signed: infra_outbound_grpc::ServiceTokenClient,
        ) {
            use axum::http::{Request as HttpRequest, StatusCode};
            use futures_util::StreamExt as _;
            use grpc_contracts::example::v1::echo_service_client::EchoServiceClient;
            use tonic_reflection::pb::v1alpha;
            use tower::ServiceExt as _;

            let rejected = HttpRequest::post("/grpc.health.v1.Health/Check")
                .header("content-type", "text/plain")
                .body(tonic::body::Body::empty())
                .unwrap();
            let response = resource.clone().oneshot(rejected).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
            assert!(!response.headers().contains_key("x-listener"));
            let unknown = HttpRequest::post("/unknown.Service/Call")
                .header("content-type", "application/grpc")
                .body(tonic::body::Body::empty())
                .unwrap();
            let forbidden = signed.clone().oneshot(unknown).await.unwrap();
            assert_eq!(forbidden.headers()["grpc-status"], "7");
            assert_eq!(forbidden.headers()["x-listener"], "grpc");
            let empty_protobuf_frame = axum::body::Body::from(&[0_u8; 5][..]);
            let check = HttpRequest::post("/grpc.health.v1.Health/Check")
                .header("content-type", "application/grpc")
                .body(tonic::body::Body::new(empty_protobuf_frame))
                .unwrap();
            let health = resource.oneshot(check).await.unwrap();
            assert_eq!(health.headers()["x-listener"], "grpc");
            assert_eq!(health.status(), StatusCode::OK);
            let body = axum::body::to_bytes(axum::body::Body::new(health.into_body()), 64)
                .await
                .unwrap();
            assert!(!body.is_empty());
            let alpha =
                v1alpha::server_reflection_client::ServerReflectionClient::new(signed.clone())
                    .server_reflection_info(futures_util::stream::empty::<
                        v1alpha::ServerReflectionRequest,
                    >())
                    .await
                    .unwrap();
            assert_eq!(alpha.metadata().get("x-listener").unwrap(), "grpc");
            assert!(alpha.into_inner().message().await.unwrap().is_none());
            let queries = futures_util::stream::iter([ServerReflectionRequest {
                message_request: Some(MessageRequest::ListServices(String::new())),
                ..Default::default()
            }])
            .chain(futures_util::stream::pending());
            let open = ServerReflectionClient::new(signed.clone())
                .server_reflection_info(queries)
                .await
                .unwrap();
            assert_eq!(open.metadata().get("x-listener").unwrap(), "grpc");
            let mut stream = open.into_inner();
            std::assert_matches!(
                stream.message().await.unwrap().and_then(|reply| reply.message_response),
                Some(MessageResponse::ListServicesResponse(list)) if list.service.iter().any(|s| s.name == "example.v1.EchoService"));
            let status = EchoServiceClient::new(signed)
                .unary(UnaryRequest::default())
                .await
                .unwrap_err();
            assert_eq!(status.code(), tonic::Code::ResourceExhausted);
            drop(stream);
        }

        pub(super) async fn assert_registration_failure(
            config: &super::Config,
            metrics: &super::Metrics,
        ) {
            let mut serving = super::Serving::default();
            let failure = super::start(
                config,
                registration("https://directory.invalid".to_owned()),
                metrics,
                &mut super::Background::new(super::CancellationToken::new()),
                &mut super::Dependencies::default(),
                &mut serving,
            )
            .await
            .unwrap_err();
            let source = std::error::Error::source(&failure).unwrap();
            std::assert_matches!(
                source.downcast_ref::<infra_outbound_grpc::ClientError>(),
                Some(infra_outbound_grpc::ClientError::DestinationSecurityMismatch)
            );
            assert!(serving.grpc_server.is_none() && serving.http_server.is_none());
        }
    }

    fn grpc_test_config(
        certificate: &tls_test_support::TlsMaterial,
        key: &infra_serviceauth::test_support::TestKey,
    ) -> Config {
        test_config(&[
            ("APP__APP__ENV", "local"),
            ("APP__HTTP__ADDR", "127.0.0.1:0"),
            ("APP__HTTP__READINESS_PROPAGATION_DELAY", "0s"),
            ("APP__GRPC__ENABLED", "true"),
            ("APP__GRPC__ADDR", "127.0.0.1:0"),
            ("APP__GRPC__SECURITY", "tls"),
            ("APP__GRPC__MAX_IN_FLIGHT", "1"),
            ("APP__GRPC__ADMISSION_WAIT", "0s"),
            ("APP__GRPC__CERTIFICATE", &certificate.cert_pem),
            ("APP__GRPC__PRIVATE_KEY", &certificate.key_pem),
            ("APP__OBSERVABILITY__METRICS__ADDR", ""),
            ("APP__SERVICE_AUTH__MODE", "jwt"),
            ("APP__SERVICE_AUTH__NAME", "catalog"),
            ("APP__SERVICE_AUTH__JWT_TYPE", "example-svc+jwt"),
            (
                "APP__SERVICE_AUTH__TRUST",
                &infra_serviceauth::test_support::registry(&[("orders", &[key])]),
            ),
        ])
    }

    #[tokio::test]
    async fn registered_grpc_middleware_forwards_native_calls_through_listener_policies() {
        use grpc_contracts::example::v1::{UnaryRequest, echo_service_client::EchoServiceClient};
        use infra_serviceauth::test_support::TestKey;

        let key = TestKey::new("orders/1");
        let certificate = tls_test_support::TlsMaterial::new("127.0.0.1");
        let config = grpc_test_config(&certificate, &key);
        let metrics = shared_metrics();
        let mut background = Background::new(CancellationToken::new());
        let mut dependencies = Dependencies::default();
        let mut serving = Serving::default();
        service_auth_registration::assert_registration_failure(&config, &metrics).await;
        let directory = service_auth_registration::directory();
        let prepared = start(
            &config,
            service_auth_registration::registration(format!("http://{}", directory.local_addr())),
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
        let address = serving.grpc_server.as_ref().unwrap().local_addr();
        let origin = format!("https://{address}");
        let resource = infra_outbound_grpc::Client::try_new(
            &origin,
            infra_outbound_grpc::ClientSecurity::Tls(infra_outbound_grpc::ClientTlsMaterial {
                ca_certificate_pem: Some(certificate.root_pem),
                identity: None,
            }),
            Duration::from_secs(5),
        )
        .unwrap();
        let status = EchoServiceClient::new(resource.clone())
            .unary(UnaryRequest::default())
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
        let signing = infra_serviceauth::TokenSource::try_new(
            key.signing_options("orders", "example-svc+jwt"),
        )
        .unwrap();
        let signed = resource
            .with_service_tokens(
                signing
                    .receiver("catalog", origin.parse().unwrap())
                    .unwrap(),
            )
            .unwrap();
        let admitted = EchoServiceClient::new(signed.clone())
            .unary(UnaryRequest::default())
            .await
            .unwrap();
        assert_eq!(admitted.metadata().get("x-listener").unwrap(), "grpc");
        assert_eq!(admitted.into_inner().message, "orders");
        service_auth_registration::exercise_listener(resource, signed).await;
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
        assert_eq!(
            directory.drain(Duration::from_secs(5)).await.unwrap(),
            infra_http_transport::Drained::Complete
        );
    }
    // template:end service-auth:bootstrap-service-auth-test
}
