//! Composition root: configuration, telemetry, readiness, transports, and
//! process lifecycle.
//!
//! Startup order: flags → config → signal handlers → tracer provider →
//! subscriber → metrics recorder → background tasks → dependencies → API
//! contract → readiness admission → listeners → ready. Every exit after the
//! signal handlers, including a failed or stopped startup, runs the one
//! teardown in [`shutdown`]. Handlers and feature code never own this
//! sequence.

mod shutdown;

use std::error::Error;
use std::ffi::OsString;
use std::process::ExitCode;
use std::time::Duration;

use health::{Probe, Readiness, RefreshPolicy};
use infra_http::{
    HTTP_REQUESTS_DURATION_BUCKETS, HTTP_REQUESTS_DURATION_SECONDS, HardenOptions, Server,
    ServerOptions,
};
// template:begin cache:service-bootstrap-cache-imports
use infra_cache::{Cache, CacheError, CacheOptions};
// template:end cache:service-bootstrap-cache-imports
// template:begin object-storage:service-bootstrap-object-storage-imports
use infra_object_storage::{CredentialSource, ObjectStorage, ObjectStorageOptions, Provider};
use service_config::{ObjectStorageCredentials, ObjectStorageProvider};
// template:end object-storage:service-bootstrap-object-storage-imports
// template:begin inbound-webhooks:bootstrap-webhooks-imports
use infra_http::webhooks::WebhookState;
use infra_webhooks::inbound::Receiver;
use infra_webhooks::protocol::{KeyRing, SigningKey};
use service_config::InboundWebhooksConfig;
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
    diagnostics_router, install_subscriber, install_tracer_provider, runtime_metrics,
};
// template:begin integration:bootstrap-postgres-secret-import
use secrecy::ExposeSecret;
// template:end integration:bootstrap-postgres-secret-import
use service_config::{
    AppConfig, BuildInfo, Config, LoadOptions, LogFormat, TracesSampler, process_failure,
};
// template:begin authn:bootstrap-authn-config-import
use service_config::AuthnConfig;
// template:end authn:bootstrap-authn-config-import
// template:begin oidc-jwt:bootstrap-auth-jwt-algorithm-import
use service_config::{JwtAlgorithm, TokenProfile};
// template:end oidc-jwt:bootstrap-auth-jwt-algorithm-import
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

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
// template:begin cache:service-bootstrap-cache-startup-budget
const CACHE_STARTUP_CHECK: Duration = Duration::from_secs(1);
// template:end cache:service-bootstrap-cache-startup-budget

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
    Cache(#[from] CacheError),
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
    // template:begin grpc:bootstrap-grpc-error
    #[error(transparent)]
    Grpc(#[from] infra_grpc::Error),
    // template:end grpc:bootstrap-grpc-error
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
            tracing::error!(error = %err, "startup failed");
            process_failure(&err.to_string())
        }
    }
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
    let metrics = Metrics::install(&[
        (
            HTTP_REQUESTS_DURATION_SECONDS,
            HTTP_REQUESTS_DURATION_BUCKETS,
        ),
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
    ])?;
    metrics.record_trace_exporter_initialized(matches!(
        tracer_provider.exporter_state,
        ExporterState::Initialized { .. }
    ));
    log_startup_summary(&config, &tracer_provider.exporter_state);

    let cancel = CancellationToken::new();
    let tracker = TaskTracker::new();
    tracker.spawn(metrics.clone().upkeep(cancel.child_token()));
    tracker.spawn(runtime_metrics(
        METRICS_MAINTENANCE_INTERVAL,
        cancel.child_token(),
    ));

    let mut dependencies = Dependencies::default();
    let started = Box::pin(serve_until_stopped(
        &config,
        // template:begin grpc:bootstrap-grpc-start-registration-argument
        grpc_registration,
        // template:end grpc:bootstrap-grpc-start-registration-argument
        &metrics,
        &mut signals,
        &cancel,
        &tracker,
        &mut dependencies,
    ))
    .await;
    let (serving, failure) = match started {
        Ok(serving) => (serving, None),
        Err(err) => (None, Some(err)),
    };
    // Dropping `TracerProviderHandle` is not last-ref: the global SDK clone
    // remains until process teardown, so a failed startup flushes too.
    let outcome = shutdown::run(shutdown::Plan {
        http_config: &config.http,
        signals: &mut signals,
        serving,
        cancel,
        tracker,
        dependencies,
        tracer_provider,
    })
    .await;
    match failure {
        Some(err) => Err(err),
        None => Ok(outcome),
    }
}

/// Open dependencies, compose and admit the routes, bind the listeners, and
/// serve until a stop signal. Returns the listeners for teardown, or `None`
/// when the signal arrived before admission. What startup opens lands in
/// `dependencies` or `tracker`, so teardown releases it on every path.
#[allow(
    clippy::too_many_lines,
    reason = "startup is one ordered sequence; each step names the profile that owns it"
)]
async fn serve_until_stopped(
    config: &Config,
    // template:begin grpc:bootstrap-grpc-start-registration-parameter
    grpc_registration: Option<crate::GrpcRegistration>,
    // template:end grpc:bootstrap-grpc-start-registration-parameter
    metrics: &Metrics,
    signals: &mut Signals,
    cancel: &CancellationToken,
    tracker: &TaskTracker,
    #[allow(unused_variables, reason = "dependency-free profiles open nothing")]
    dependencies: &mut Dependencies,
) -> Result<Option<Serving>, BootstrapError> {
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
    let auth = prepare_auth(config, tracker, cancel).await?;
    // template:end authn:bootstrap-authn-prepare
    // template:begin postgres:bootstrap-postgres-startup
    dependencies.postgres = open_postgres(config, tracker, cancel).await?;
    if let Some(pool) = &dependencies.postgres {
        probes.push(Box::new(PostgresProbe::new(pool.clone())));
        migrate::verify_history(pool).await?;
    }
    // template:end postgres:bootstrap-postgres-startup
    // template:begin cache:service-bootstrap-cache-startup
    dependencies.cache = open_cache(config).await?;
    // template:end cache:service-bootstrap-cache-startup
    // template:begin object-storage:service-bootstrap-object-storage-startup
    dependencies.object_storage = open_object_storage(config)?;
    // template:end object-storage:service-bootstrap-object-storage-startup
    // template:begin http-idempotency:bootstrap-http-idempotency-composer
    let mut composer = prepare_http_idempotency(config, dependencies.postgres.as_ref());
    // template:end http-idempotency:bootstrap-http-idempotency-composer
    // template:begin inbound-webhooks:bootstrap-webhooks-prepare
    let webhook_state =
        prepare_inbound_webhooks(config, dependencies.postgres.as_ref(), tracker, cancel)?;
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
    )
    .map_err(BootstrapError::HttpComposition)?;
    let routes = match auth {
        PreparedAuth::None => infra_http::finalize_public(contract)?,
        // template:begin authn:bootstrap-authn-finalize-enabled
        PreparedAuth::Enabled(verifier) => infra_http::authn::finalize(contract, *verifier)?,
        // template:end authn:bootstrap-authn-finalize-enabled
    };
    // template:begin http-idempotency:bootstrap-http-idempotency-activation
    activate_http_idempotency(composer, config, tracker, cancel).await?;
    // template:end http-idempotency:bootstrap-http-idempotency-activation
    // Admission runs even without probes so the first probe after bind
    // answers from an evaluation.
    readiness.refresh().await;
    readiness
        .reader()
        .verdict()
        .map_err(BootstrapError::Admission)?;
    tracker.spawn({
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

    tracing::info!("service_ready");
    signals.wait().await;
    Ok(Some(Serving {
        readiness,
        app_listener,
        diagnostics,
        // template:begin grpc:bootstrap-serving-grpc
        grpc_listener,
        // template:end grpc:bootstrap-serving-grpc
    }))
}

// template:begin inbound-webhooks:bootstrap-webhooks-constructor
/// Build a receiver from the immutable startup snapshot. An empty endpoint
/// map is a retained, inert route; an active endpoint cannot reach listener
/// admission without PostgreSQL, a bound consumer, and every referenced key.
fn prepare_inbound_webhooks(
    config: &Config,
    postgres_pool: Option<&PgPool>,
    tracker: &TaskTracker,
    cancel: &CancellationToken,
) -> Result<WebhookState, BootstrapError> {
    let webhooks = &config.inbound_webhooks;
    if webhooks.endpoints.is_empty() {
        return Ok(WebhookState::inert());
    }
    // Configuration validation already requires `postgres.enabled` here.
    let pool = postgres_pool.ok_or_else(|| {
        service_config::ValidationError::new(
            "postgres.enabled",
            "must be true when inbound webhook endpoints are configured",
        )
    })?;
    let consumers = webhook_consumers::consumers();
    consumers
        .require(webhooks.endpoints.keys().map(String::as_str))
        .map_err(|missing| BootstrapError::InboundWebhookConsumerMissing {
            endpoint: missing.endpoint,
        })?;
    let mut bindings = Vec::with_capacity(webhooks.endpoints.len());
    for (endpoint_id, endpoint) in &webhooks.endpoints {
        let active = signing_key(webhooks, endpoint_id, &endpoint.active_key)?;
        let previous = endpoint
            .previous_key
            .as_deref()
            .map(|key| signing_key(webhooks, endpoint_id, key))
            .transpose()?;
        bindings.push((endpoint_id.clone(), KeyRing::new(active, previous)));
    }
    let receiver = Receiver::new(pool.clone(), bindings);
    tracker.spawn(receiver.clone().run_cleanup(cancel.child_token()));
    Ok(WebhookState::active(receiver))
}

/// Decode the secret that one endpoint's key reference names. The worker
/// shares the endpoint section but holds no secrets, so this check is ours.
fn signing_key(
    webhooks: &InboundWebhooksConfig,
    endpoint: &str,
    key: &str,
) -> Result<SigningKey, BootstrapError> {
    let secret =
        webhooks
            .secrets
            .get(key)
            .ok_or_else(|| BootstrapError::InboundWebhookKeyReference {
                endpoint: endpoint.to_owned(),
                key: key.to_owned(),
            })?;
    SigningKey::from_encoded(secret.expose_secret()).map_err(|source| {
        BootstrapError::InboundWebhookKey {
            endpoint: endpoint.to_owned(),
            key: key.to_owned(),
            source,
        }
    })
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
            let issuer = issuer_url("oidc-jwt", "authn.issuer", issuer)?;
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
            let issuer = issuer_url("oidc-introspection", "authn.issuer", issuer)?;
            let endpoint = infra_bearerauthn::EndpointUrl::parse(introspection_endpoint).map_err(
                |source| BootstrapError::AuthenticationPreparation {
                    mode: "oidc-introspection",
                    key: "authn.introspection_endpoint",
                    source,
                },
            )?;
            // Configuration validation requires the secret in this mode.
            let client_secret = introspection_client_secret.clone().ok_or_else(|| {
                service_config::ValidationError::new(
                    "authn.introspection_client_secret",
                    "is required when authn.mode = oidc-introspection",
                )
            })?;
            let provider_concurrency = std::num::NonZeroUsize::try_from(*provider_concurrency)
                .map_err(|_| {
                    service_config::ValidationError::new(
                        "authn.provider_concurrency",
                        "does not fit this platform",
                    )
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
fn issuer_url(
    mode: &'static str,
    key: &'static str,
    value: &str,
) -> Result<infra_bearerauthn::IssuerUrl, BootstrapError> {
    infra_bearerauthn::IssuerUrl::parse(value)
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
/// Open the pool when the profile is enabled. An unreachable database fails
/// startup here rather than serving a readiness that never passes.
async fn open_postgres(
    config: &Config,
    tracker: &TaskTracker,
    cancel: &CancellationToken,
) -> Result<Option<PgPool>, BootstrapError> {
    if !config.postgres.enabled {
        return Ok(None);
    }
    let dsn = Dsn::admit(config.postgres.required_dsn()?.expose_secret())?;
    let pool = infra_postgres::connect(
        &dsn,
        &PoolOptions {
            max_connections: config.postgres.max_connections,
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
    tracker.spawn(infra_postgres::record_metrics_periodically(
        pool.clone(),
        METRICS_MAINTENANCE_INTERVAL,
        cancel.child_token(),
    ));
    Ok(Some(pool))
}
// template:end postgres:bootstrap-open-postgres

// template:begin cache:service-bootstrap-cache-functions
/// Connect the optional cache. An outage at startup is logged, not fatal:
/// the cache is not a readiness probe, and callers fall back to the source
/// of truth.
async fn open_cache(config: &Config) -> Result<Option<Cache>, BootstrapError> {
    let Some(dsn) = &config.cache.dsn else {
        return Ok(None);
    };
    let cache = Cache::connect(CacheOptions {
        dsn: dsn.clone(),
        root_ca_path: config.cache.root_ca_path.clone(),
        allow_plaintext: config.cache.allow_plaintext,
        allow_unauthenticated: config.cache.allow_unauthenticated,
        command_timeout: config.cache.command_timeout,
    })?;
    let server = cache.server();
    match tokio::time::timeout(CACHE_STARTUP_CHECK, cache.probe().check()).await {
        Ok(Ok(())) => {
            tracing::info!(
                server.address = %server.host,
                server.port = server.port,
                cache.tls = server.tls,
                "cache_connected"
            );
        }
        Ok(Err(error)) => {
            tracing::warn!(
                server.address = %server.host,
                server.port = server.port,
                cache.tls = server.tls,
                reason = %error,
                "cache_unavailable_at_startup"
            );
        }
        Err(_) => {
            tracing::warn!(
                server.address = %server.host,
                server.port = server.port,
                cache.tls = server.tls,
                reason = "startup check timed out",
                "cache_unavailable_at_startup"
            );
        }
    }
    Ok(Some(cache))
}
// template:end cache:service-bootstrap-cache-functions

// template:begin object-storage:service-bootstrap-object-storage-functions
/// Build the optional object storage client. Nothing is sent: the bucket is
/// not a readiness dependency, and a service that cannot serve without it
/// pushes `storage.probe()` into the readiness probes instead.
fn open_object_storage(config: &Config) -> Result<Option<ObjectStorage>, BootstrapError> {
    let settings = &config.object_storage;
    let provider = match settings.provider {
        ObjectStorageProvider::None => return Ok(None),
        ObjectStorageProvider::AmazonS3 => Provider::AmazonS3 {
            region: settings.region.clone(),
            expected_bucket_owner: settings.expected_bucket_owner.clone(),
        },
        ObjectStorageProvider::CloudflareR2 => Provider::CloudflareR2 {
            endpoint: settings.endpoint.clone(),
        },
        ObjectStorageProvider::Railway => Provider::Railway {
            endpoint: settings.endpoint.clone(),
            region: settings.region.clone(),
        },
        ObjectStorageProvider::S3Compatible => Provider::S3Compatible {
            endpoint: settings.endpoint.clone(),
            region: settings.region.clone(),
            path_style: settings.path_style,
        },
        ObjectStorageProvider::Local => Provider::Local {
            endpoint: settings.endpoint.clone(),
            region: settings.region.clone(),
        },
    };
    let (credentials, credentials_label) = match settings.credentials {
        ObjectStorageCredentials::AccessKey => {
            let Some(secret_access_key) = settings.secret_access_key.clone() else {
                return Err(infra_object_storage::ConfigError::SecretAccessKey.into());
            };
            let credentials = CredentialSource::AccessKey {
                access_key_id: settings.access_key_id.clone(),
                secret_access_key,
            };
            (credentials, "access_key")
        }
        ObjectStorageCredentials::WorkloadIdentity => {
            (CredentialSource::WorkloadIdentity, "workload_identity")
        }
    };
    let storage = ObjectStorage::new(ObjectStorageOptions {
        provider,
        bucket: settings.bucket.clone(),
        credentials,
        max_object_bytes: settings.max_object_bytes.as_u64(),
        max_concurrency: usize::try_from(settings.max_concurrency).unwrap_or(usize::MAX),
        operation_timeout: settings.operation_timeout,
    })?;
    tracing::info!(
        object_storage.provider = storage.provider(),
        object_storage.credentials = credentials_label,
        object_storage.max_object_bytes = settings.max_object_bytes.as_u64(),
        object_storage.max_concurrency = settings.max_concurrency,
        "object_storage_configured"
    );
    Ok(Some(storage))
}
// template:end object-storage:service-bootstrap-object-storage-functions

// template:begin http-idempotency:bootstrap-http-idempotency-functions
/// The composer through which idempotent operations join the contract. It
/// has a store only when both the pool and a retention are set; otherwise
/// activation refuses the missing value if an idempotent operation is served.
fn prepare_http_idempotency(config: &Config, postgres_pool: Option<&PgPool>) -> Composer {
    match (postgres_pool, config.http_idempotency.retention) {
        (Some(pool), Some(retention)) => Composer::new(Store::new(pool.clone(), retention)),
        _ => Composer::inert(),
    }
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
    store: Option<Store>,
    operations: std::num::NonZeroUsize,
    config: &Config,
    tracker: &TaskTracker,
    cancel: &CancellationToken,
) -> Result<(), BootstrapError> {
    let retention = config
        .http_idempotency
        .required_retention(&config.postgres)?;
    let Some(store) = store else {
        return Err(infra_idempotency_store::StartupError::Unavailable.into());
    };
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

    // template:begin inbound-webhooks:bootstrap-webhooks-tests
    #[tokio::test]
    async fn inbound_startup_rejects_an_endpoint_without_an_adopter_consumer() {
        let mut config = Config::default();
        config.inbound_webhooks.endpoints.insert(
            "partner".into(),
            service_config::InboundWebhookEndpointConfig {
                active_key: "partner_v1".into(),
                previous_key: None,
            },
        );
        let pool = PgPool::connect_lazy("postgres://localhost/unused")
            .expect("lazy pool does not connect");
        let tracker = TaskTracker::new();
        let cancel = CancellationToken::new();
        let result = prepare_inbound_webhooks(&config, Some(&pool), &tracker, &cancel);
        assert!(matches!(
            result,
            Err(BootstrapError::InboundWebhookConsumerMissing { endpoint })
                if endpoint == "partner"
        ));
        pool.close().await;
    }
    // template:end inbound-webhooks:bootstrap-webhooks-tests

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
    /// Start an active boundary of one operation without a store, on a fresh
    /// tracker the caller can inspect for a spawned task.
    async fn start_one_operation(config: &Config) -> (Result<(), BootstrapError>, TaskTracker) {
        let tracker = TaskTracker::new();
        let started = start_http_idempotency(
            None,
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
        let (started, tracker) = start_one_operation(&Config::default()).await;
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
        let (started, tracker) = start_one_operation(&config).await;
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
        let (started, tracker) = start_one_operation(&config).await;
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
