//! Composition root: configuration, telemetry, readiness, transports, and
//! process lifecycle.
//!
//! Startup order: flags → config → signal handlers → tracer provider →
//! subscriber → metrics recorder → background tasks → dependency pools →
//! API contract → readiness admission → HTTP listeners → ready. Shutdown
//! order lives in [`shutdown`]. Handlers and feature code never own this
//! sequence.

mod shutdown;

use std::error::Error;
use std::ffi::OsString;
use std::process::ExitCode;
use std::time::Duration;

use health::{Probe, Readiness, RefreshPolicy};
use infra_http::{HTTP_REQUESTS_DURATION_SECONDS, HardenOptions, Server, ServerOptions};
// template:begin inbound-webhooks:bootstrap-webhooks-imports
use infra_http::webhooks::WebhookState;
use infra_webhooks::inbound::{Consumers, Receiver};
use infra_webhooks::protocol::{KeyRing, SigningKey};
// template:end inbound-webhooks:bootstrap-webhooks-imports
// template:begin authn:bootstrap-authn-imports
use infra_bearerauthn::Verifier;
// template:end authn:bootstrap-authn-imports
// template:begin http-idempotency:bootstrap-http-idempotency-imports
use infra_http::idempotency::{Activation, Composer};
use infra_idempotency_store::Store;
// template:end http-idempotency:bootstrap-http-idempotency-imports
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
use service_config::AuthnConfig;
// template:end authn:bootstrap-authn-config-import
// template:begin oidc-jwt:bootstrap-auth-jwt-algorithm-import
use service_config::{JwtAlgorithm, TokenProfile};
// template:end oidc-jwt:bootstrap-auth-jwt-algorithm-import
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
    #[error("authentication preparation failed for {mode} {key}: {source}")]
    AuthenticationPreparation {
        mode: &'static str,
        key: &'static str,
        #[source]
        source: infra_bearerauthn::PreparationError,
    },
    #[error("authentication preparation input is unavailable for {mode}: {key}")]
    AuthenticationInput {
        mode: &'static str,
        key: &'static str,
    },
    // template:end authn:bootstrap-authn-errors
    #[error(transparent)]
    HttpContract(#[from] infra_http::FinalizeError),
    #[error("http contract composition: {0}")]
    HttpComposition(#[source] Box<dyn Error + Send + Sync>),
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
    #[error(
        "configuration is invalid: postgres.enabled must be true when inbound webhook endpoints are configured"
    )]
    InboundWebhooksPostgresRequired,
    #[error("inbound webhook endpoint {endpoint} references unavailable key {key}")]
    InboundWebhookKeyReference { endpoint: String, key: String },
    #[error("inbound webhook endpoint {endpoint} key {key} is invalid: {source}")]
    InboundWebhookKey {
        endpoint: String,
        key: String,
        #[source]
        source: infra_webhooks::protocol::ProtocolError,
    },
    #[error("inbound webhook endpoint {endpoint} has no consumer binding")]
    InboundWebhookConsumerMissing { endpoint: String },
    // template:end inbound-webhooks:bootstrap-webhooks-errors
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
        #[allow(
            unused_variables,
            reason = "the no-auth projection uses this fallback; retained authentication shadows it"
        )]
        let auth = PreparedAuth::None;
        // template:begin authn:bootstrap-authn-prepare
        let auth = prepare_auth(&config, &tracker, &cancel).await?;
        // template:end authn:bootstrap-authn-prepare
        // template:begin postgres:bootstrap-postgres-startup
        let (probes, pool) = prepare_postgres(probes, &config, &tracker, &cancel).await?;
        postgres_pool = pool;
        if let Some(pool) = &postgres_pool {
            migrate::verify_history(pool).await?;
        }
        // template:end postgres:bootstrap-postgres-startup
        // template:begin http-idempotency:bootstrap-http-idempotency-composer
        let composer = prepare_http_idempotency(&config, postgres_pool.as_ref());
        // template:end http-idempotency:bootstrap-http-idempotency-composer
        // template:begin inbound-webhooks:bootstrap-webhooks-prepare
        let webhook_state = prepare_inbound_webhooks(&config, postgres_pool.as_ref())?;
        // template:end inbound-webhooks:bootstrap-webhooks-prepare

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
            auth,
            // template:begin postgres:bootstrap-prepared-pool
            postgres_pool: postgres_pool.clone(),
            // template:end postgres:bootstrap-prepared-pool
            // template:begin http-idempotency:bootstrap-http-idempotency-prepared-value
            composer,
            // template:end http-idempotency:bootstrap-http-idempotency-prepared-value
            // template:begin inbound-webhooks:bootstrap-webhooks-prepared-value
            webhook_state,
            // template:end inbound-webhooks:bootstrap-webhooks-prepared-value
        })
        .await
    }
    .await;
    // Bind, admission, or connect failure: cancel and join tracked tasks,
    // then close any opened pool. `Server` only cancels accept. Dropping
    // `TracerProviderHandle` is not last-ref: the global SDK clone remains
    // until process teardown.
    if outcome.is_err() {
        shutdown::cancel_and_join_background_tasks(&cancel, &tracker).await;
        // template:begin postgres:bootstrap-startup-pool-close
        if let Some(pool) = postgres_pool.as_ref() {
            shutdown::close_opened_postgres(pool).await;
        }
        // template:end postgres:bootstrap-startup-pool-close
    }
    outcome
}

// template:begin inbound-webhooks:bootstrap-webhooks-constructor
/// Build a receiver from the immutable startup snapshot.  An empty configured
/// endpoint map is a retained, inert route; an active endpoint cannot reach
/// listener admission without PostgreSQL and every referenced key.
fn prepare_inbound_webhooks(
    config: &Config,
    postgres_pool: Option<&PgPool>,
) -> Result<WebhookState, BootstrapError> {
    if config.inbound_webhooks.endpoints.is_empty() {
        return Ok(WebhookState::inert());
    }
    let pool = postgres_pool.ok_or(BootstrapError::InboundWebhooksPostgresRequired)?;
    // The template has no domain consumer.  A derived service adds its real
    // adapter to this same registry constructor in both roots; serving an
    // endpoint without that binding could durably accept work it cannot own.
    let consumers = Consumers::new();
    for endpoint_id in config.inbound_webhooks.endpoints.keys() {
        if !consumers.contains(endpoint_id) {
            return Err(BootstrapError::InboundWebhookConsumerMissing {
                endpoint: endpoint_id.clone(),
            });
        }
    }
    let bindings = config
        .inbound_webhooks
        .endpoints
        .iter()
        .map(|(endpoint_id, endpoint)| {
            let active = config
                .inbound_webhooks
                .secrets
                .get(&endpoint.active_key)
                .ok_or_else(|| BootstrapError::InboundWebhookKeyReference {
                    endpoint: endpoint_id.clone(),
                    key: endpoint.active_key.clone(),
                })?;
            let previous = endpoint
                .previous_key
                .as_ref()
                .map(|key| {
                    config.inbound_webhooks.secrets.get(key).ok_or_else(|| {
                        BootstrapError::InboundWebhookKeyReference {
                            endpoint: endpoint_id.clone(),
                            key: key.clone(),
                        }
                    })
                })
                .transpose()?;
            let active = SigningKey::from_encoded(active.expose_secret()).map_err(|source| {
                BootstrapError::InboundWebhookKey {
                    endpoint: endpoint_id.clone(),
                    key: endpoint.active_key.clone(),
                    source,
                }
            })?;
            let previous = endpoint
                .previous_key
                .as_ref()
                .zip(previous)
                .map(|(key, value)| {
                    SigningKey::from_encoded(value.expose_secret()).map_err(|source| {
                        BootstrapError::InboundWebhookKey {
                            endpoint: endpoint_id.clone(),
                            key: key.clone(),
                            source,
                        }
                    })
                })
                .transpose()?;
            let key = KeyRing::new(active, previous);
            Ok((endpoint_id.clone(), key))
        })
        .collect::<Result<Vec<_>, BootstrapError>>()?;
    Ok(WebhookState::active(Receiver::new(pool.clone(), bindings)))
}
// template:end inbound-webhooks:bootstrap-webhooks-constructor

// template:begin authn:bootstrap-prepare-auth-prefix
#[allow(
    unused_variables,
    clippy::unused_async,
    reason = "JWT discovery and refresh use the future and task ownership; introspection-only output preserves this preparation signature without provider I/O"
)]
async fn prepare_auth(
    config: &Config,
    tracker: &TaskTracker,
    cancel: &CancellationToken,
) -> Result<PreparedAuth, BootstrapError> {
    match &config.authn {
        // template:end authn:bootstrap-prepare-auth-prefix
        // template:begin oidc-jwt:bootstrap-prepare-auth-jwt
        AuthnConfig::OidcJwt {
            issuer,
            audience,
            token_profile,
            algorithms,
        } => {
            let issuer = provider_url("oidc-jwt", "authn.issuer", issuer)?;
            let (verifier, refresh) = infra_bearerauthn::prepare_jwt(
                infra_bearerauthn::JwtOptions {
                    issuer,
                    audiences: audience.as_slice().to_vec(),
                    token_profile: match token_profile {
                        TokenProfile::ResourceServer => {
                            infra_bearerauthn::TokenProfile::ResourceServer
                        }
                        TokenProfile::Rfc9068 => infra_bearerauthn::TokenProfile::Rfc9068,
                    },
                    algorithms: algorithms.iter().copied().map(jwt_algorithm).collect(),
                },
                cancel.child_token(),
            )
            .await
            .map_err(|source| BootstrapError::AuthenticationPreparation {
                mode: "oidc-jwt",
                key: "authn.issuer",
                source,
            })?;
            tracker.spawn(refresh);
            Ok(PreparedAuth::Enabled(Box::new(verifier)))
        }
        // template:end oidc-jwt:bootstrap-prepare-auth-jwt
        // template:begin oidc-introspection:bootstrap-prepare-auth-introspection
        AuthnConfig::OidcIntrospection {
            issuer,
            audience,
            introspection_endpoint,
            introspection_client_id,
            introspection_client_secret,
            provider_concurrency,
            cache_enabled,
            cache_capacity,
            cache_ttl,
        } => {
            let issuer = provider_url("oidc-introspection", "authn.issuer", issuer)?;
            let endpoint = infra_bearerauthn::ProviderUrl::parse_endpoint(introspection_endpoint)
                .map_err(|source| BootstrapError::AuthenticationPreparation {
                mode: "oidc-introspection",
                key: "authn.introspection_endpoint",
                source,
            })?;
            let client_secret =
                introspection_client_secret
                    .clone()
                    .ok_or(BootstrapError::AuthenticationInput {
                        mode: "oidc-introspection",
                        key: "authn.introspection_client_secret",
                    })?;
            let provider_concurrency = usize::try_from(provider_concurrency.get())
                .ok()
                .and_then(std::num::NonZeroUsize::new)
                .ok_or(BootstrapError::AuthenticationInput {
                    mode: "oidc-introspection",
                    key: "authn.provider_concurrency",
                })?;
            let cache =
                infra_bearerauthn::IntrospectionCacheOptions::new(*cache_capacity, *cache_ttl)
                    .map_err(|source| BootstrapError::AuthenticationPreparation {
                        mode: "oidc-introspection",
                        key: "authn.cache_capacity/authn.cache_ttl",
                        source,
                    })?;
            infra_bearerauthn::prepare_introspection(infra_bearerauthn::IntrospectionOptions {
                issuer,
                audiences: audience.as_slice().to_vec(),
                endpoint,
                client_id: introspection_client_id.clone(),
                client_secret,
                provider_concurrency,
                cache: cache_enabled.then_some(cache),
            })
            .map(|verifier| PreparedAuth::Enabled(Box::new(verifier)))
            .map_err(|source| BootstrapError::AuthenticationPreparation {
                mode: "oidc-introspection",
                key: "authn.introspection_endpoint",
                source,
            })
        }
        // template:end oidc-introspection:bootstrap-prepare-auth-introspection
        // template:begin authn:bootstrap-prepare-auth-suffix
        AuthnConfig::None {} => Ok(PreparedAuth::None),
    }
}
// template:end authn:bootstrap-prepare-auth-suffix

// template:begin authn:bootstrap-auth-provider-url
fn provider_url(
    mode: &'static str,
    key: &'static str,
    value: &str,
) -> Result<infra_bearerauthn::ProviderUrl, BootstrapError> {
    infra_bearerauthn::ProviderUrl::parse(value)
        .map_err(|source| BootstrapError::AuthenticationPreparation { mode, key, source })
}
// template:end authn:bootstrap-auth-provider-url

// template:begin oidc-jwt:bootstrap-auth-jwt-algorithm-converter
const fn jwt_algorithm(algorithm: JwtAlgorithm) -> infra_bearerauthn::JwtAlgorithm {
    match algorithm {
        JwtAlgorithm::Rs256 => infra_bearerauthn::JwtAlgorithm::Rs256,
        JwtAlgorithm::Es256 => infra_bearerauthn::JwtAlgorithm::Es256,
        JwtAlgorithm::Ps256 => infra_bearerauthn::JwtAlgorithm::Ps256,
        JwtAlgorithm::EdDsa => infra_bearerauthn::JwtAlgorithm::EdDsa,
    }
}
// template:end oidc-jwt:bootstrap-auth-jwt-algorithm-converter

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
            default_isolation: infra_postgres::Isolation::ServerDefault,
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

// template:begin http-idempotency:bootstrap-http-idempotency-functions
/// The composer through which idempotent operations join the contract. Its
/// store uses the pool only when a retention is set as well; otherwise the
/// store is inert, and activation refuses the missing value before any
/// store call if an idempotent operation is served.
fn prepare_http_idempotency(config: &Config, postgres_pool: Option<&PgPool>) -> Composer {
    let store = match (postgres_pool, config.http_idempotency.retention) {
        (Some(pool), Some(retention)) => Store::new(pool.clone(), retention),
        _ => Store::inert(),
    };
    Composer::new(store)
}

/// Start the composed boundary when it serves at least one. An inactive
/// boundary makes no query, starts no task, and requires no value.
async fn activate_http_idempotency(
    composer: Composer,
    config: &Config,
    tracker: &TaskTracker,
    cancel: &CancellationToken,
) -> Result<(), BootstrapError> {
    match composer.finish() {
        Activation::Inactive => Ok(()),
        Activation::Active {
            store, operations, ..
        } => start_http_idempotency(store, operations, config, tracker, cancel).await,
    }
}

/// Start an active boundary before readiness admission. It needs
/// `postgres.enabled`, a set `http_idempotency.retention`, and the store's
/// schema on a writable session; the cleanup task then joins the tracker.
async fn start_http_idempotency(
    store: Store,
    operations: std::num::NonZeroUsize,
    config: &Config,
    tracker: &TaskTracker,
    cancel: &CancellationToken,
) -> Result<(), BootstrapError> {
    let retention = config
        .http_idempotency
        .required_retention(&config.postgres)?;
    store.check_startup().await?;
    tracker.spawn(store.run_cleanup(cancel.child_token()));
    tracing::info!(
        http_idempotency.operations = operations.get(),
        http_idempotency.retention = ?retention,
        "http_idempotency_active"
    );
    Ok(())
}
// template:end http-idempotency:bootstrap-http-idempotency-functions

/// Runtime pieces built before listeners bind: admission, then serve.
///
/// `cancel`, `tracker`, and `postgres_pool` are shared clones: `serve` still
/// owns `cancel_and_join_background_tasks` on the error path. `tracer_provider` and
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
    auth: PreparedAuth,
    // template:begin postgres:bootstrap-prepared-field
    postgres_pool: Option<PgPool>,
    // template:end postgres:bootstrap-prepared-field
    // template:begin http-idempotency:bootstrap-http-idempotency-prepared-field
    /// A unique move: activation consumes it after route assembly.
    composer: Composer,
    // template:end http-idempotency:bootstrap-http-idempotency-prepared-field
    // template:begin inbound-webhooks:bootstrap-webhooks-prepared-field
    webhook_state: WebhookState,
    // template:end inbound-webhooks:bootstrap-webhooks-prepared-field
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
        auth,
        // template:begin postgres:bootstrap-destructure-pool
        postgres_pool,
        // template:end postgres:bootstrap-destructure-pool
        // template:begin http-idempotency:bootstrap-http-idempotency-destructure
        mut composer,
        // template:end http-idempotency:bootstrap-http-idempotency-destructure
        // template:begin inbound-webhooks:bootstrap-webhooks-destructure
        webhook_state,
        // template:end inbound-webhooks:bootstrap-webhooks-destructure
    } = prepared;
    // The routes and the committed OpenAPI document are the two halves of
    // one contract. Assembly is pure, so it runs before readiness admission.
    let contract = service::api::contract(
        // template:begin http-idempotency:bootstrap-http-idempotency-contract-composer
        &mut composer,
        // template:end http-idempotency:bootstrap-http-idempotency-contract-composer
    )
    .map_err(BootstrapError::HttpComposition)?;
    let routes = match auth {
        PreparedAuth::None => infra_http::finalize_public(contract)?,
        // template:begin authn:bootstrap-authn-finalize-enabled
        PreparedAuth::Enabled(verifier) => infra_http::authn::finalize(contract, *verifier)?,
        // template:end authn:bootstrap-authn-finalize-enabled
    };
    // template:begin http-idempotency:bootstrap-http-idempotency-activation
    activate_http_idempotency(composer, config, &tracker, &cancel).await?;
    // template:end http-idempotency:bootstrap-http-idempotency-activation
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
    // template:begin inbound-webhooks:bootstrap-webhooks-route-state
    let routes = infra_http::webhooks::with_webhook_state(routes, webhook_state);
    // template:end inbound-webhooks:bootstrap-webhooks-route-state
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

enum PreparedAuth {
    None,
    // template:begin authn:bootstrap-prepared-auth-enabled
    Enabled(Box<Verifier>),
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

    // template:begin oidc-introspection:bootstrap-introspection-cache-tests
    #[tokio::test]
    async fn introspection_preparation_validates_cache_options_even_when_disabled() {
        for (capacity, ttl) in [(0, "30s"), (1025, "30s"), (256, "0s"), (256, "301s")] {
            let config = Config {
                authn: serde_json::from_value(serde_json::json!({
                "mode": "oidc-introspection",
                "issuer": "https://issuer.example.test",
                "audience": "api",
                "introspection_endpoint": "https://issuer.example.test/introspect?tenant=private",
                "introspection_client_id": "service",
                "introspection_client_secret": "fixture-secret",
                "cache_enabled": false,
                "cache_capacity": capacity,
                "cache_ttl": ttl,
            }))
                .expect("raw typed configuration"),
                ..Config::default()
            };
            let tracker = TaskTracker::new();
            let result = prepare_auth(&config, &tracker, &CancellationToken::new()).await;
            assert!(matches!(
                result,
                Err(BootstrapError::AuthenticationPreparation {
                    key: "authn.cache_capacity/authn.cache_ttl",
                    ..
                })
            ));
            assert!(tracker.is_empty());
        }
    }
    // template:end oidc-introspection:bootstrap-introspection-cache-tests

    // template:begin http-idempotency:bootstrap-http-idempotency-tests
    /// Start an active boundary of one operation over `store`, on a fresh
    /// tracker the caller can inspect for a spawned task.
    async fn start_one_operation(
        store: Store,
        config: &Config,
    ) -> (Result<(), BootstrapError>, TaskTracker) {
        let tracker = TaskTracker::new();
        let started = start_http_idempotency(
            store,
            std::num::NonZeroUsize::MIN,
            config,
            &tracker,
            &CancellationToken::new(),
        )
        .await;
        (started, tracker)
    }

    #[tokio::test]
    async fn an_inactive_boundary_touches_no_store_and_spawns_no_task() {
        // A contract without an idempotent operation: only the family's
        // components, which every retained document carries.
        let composer = Composer::inert();
        let tracker = TaskTracker::new();
        // An active boundary would refuse this configuration at its first
        // check: `postgres.enabled` is false and no retention is set.
        activate_http_idempotency(
            composer,
            &Config::default(),
            &tracker,
            &CancellationToken::new(),
        )
        .await
        .expect("an inactive boundary requires no value");
        assert!(tracker.is_empty());
    }

    #[tokio::test]
    async fn an_active_boundary_refuses_disabled_postgres_naming_the_key() {
        let (started, tracker) = start_one_operation(Store::inert(), &Config::default()).await;
        assert!(
            matches!(&started, Err(BootstrapError::Config(invalid)) if invalid.key == "postgres.enabled"),
            "{started:?}"
        );
        assert!(tracker.is_empty());
    }

    #[tokio::test]
    async fn an_active_boundary_refuses_an_unset_retention_naming_the_key() {
        let mut config = Config::default();
        config.postgres.enabled = true;
        let (started, tracker) = start_one_operation(Store::inert(), &config).await;
        assert!(
            matches!(&started, Err(BootstrapError::Config(invalid)) if invalid.key == "http_idempotency.retention"),
            "{started:?}"
        );
        assert!(tracker.is_empty());
    }

    #[tokio::test]
    async fn an_active_boundary_refuses_an_unavailable_store_at_the_startup_check() {
        let mut config = Config::default();
        config.postgres.enabled = true;
        config.http_idempotency.retention = Some(Duration::from_secs(3600));
        let (started, tracker) = start_one_operation(Store::inert(), &config).await;
        assert!(
            matches!(
                &started,
                Err(BootstrapError::HttpIdempotencyStartup(
                    infra_idempotency_store::StartupError::Unavailable
                ))
            ),
            "{started:?}"
        );
        assert!(tracker.is_empty());
    }
    // template:end http-idempotency:bootstrap-http-idempotency-tests
}
