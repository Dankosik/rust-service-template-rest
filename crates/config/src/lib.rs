mod app;
mod health;
mod http;
mod listener_limits;
// template:begin grpc:config-module
mod grpc;
// template:end grpc:config-module
mod log;
mod observability;
mod runtime;
// template:begin messaging:config-module
mod messaging;
// template:end messaging:config-module
// template:begin cache:config-module
mod cache;
// template:end cache:config-module
// template:begin object-storage:config-module
mod object_storage;
// template:end object-storage:config-module
// template:begin authn:config-module
mod authn;
// template:end authn:config-module
// template:begin service-auth:config-service-auth-module
mod service_auth;
// template:end service-auth:config-service-auth-module
// template:begin postgres:config-module
mod postgres;
// template:end postgres:config-module
// template:begin http-idempotency:config-http-idempotency-module
mod http_idempotency;
// template:end http-idempotency:config-http-idempotency-module
// template:begin inbound-webhooks:config-inbound-webhooks-module
mod inbound_webhooks;
// template:end inbound-webhooks:config-inbound-webhooks-module
// template:begin jobs:config-jobs-module
mod jobs;
// template:end jobs:config-jobs-module
// template:begin webhooks:config-webhooks-module
mod webhooks;
// template:end webhooks:config-webhooks-module
// template:begin worker:worker-config
mod worker;
// template:end worker:worker-config
mod temporal;
mod temporal_worker;

mod cli;
mod de;
mod load;
mod secret_policy;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod validate;

use serde::Deserialize;

use validate::admit_with;
// template:begin worker:worker-input-import
use worker::WorkerInput;
// template:end worker:worker-input-import

pub use app::{AppConfig, BuildInfo};
pub use cli::LoadOptions;
pub use health::HealthConfig;
pub use http::{HttpConfig, HttpListenerConfig, ShutdownTail};
// template:begin worker:config-worker-export
pub use http::WorkerHttpConfig;
pub use load::load_worker;
pub use worker::WorkerConfig;
// template:end worker:config-worker-export
// template:begin grpc:config-export
pub use grpc::{GrpcConfig, GrpcListenerSecurity, GrpcTlsMaterial};
// template:end grpc:config-export
// template:begin inbound-webhooks:config-inbound-webhooks-export
pub use inbound_webhooks::{InboundWebhookEndpointConfig, InboundWebhookKey};
// template:end inbound-webhooks:config-inbound-webhooks-export
pub use load::{Error, load};
pub use log::{LogConfig, LogFormat};
pub use observability::{
    MetricsConfig, ObservabilityConfig, OtelConfig, OtelExporterConfig, TracesSampler,
};
pub use runtime::RuntimeConfig;
// template:begin messaging:config-export
pub use messaging::{
    AdmittedMessaging, AdmittedMessagingConsumer, MessagingConfig, MessagingCredentials,
    MessagingTransportSecurity,
};
// template:end messaging:config-export
// template:begin cache:config-export
pub use cache::{CacheClientCertificate, CacheConfig};
// template:end cache:config-export
// template:begin object-storage:config-export
pub use object_storage::{
    ObjectStorageAccessKey, ObjectStorageAmazonCredentials, ObjectStorageConfig,
    ObjectStorageProvider,
};
// template:end object-storage:config-export
// template:begin authn:config-export
pub use authn::{Audiences, AuthnConfig};
// template:end authn:config-export
// template:begin oidc-jwt:config-jwt-input-exports
pub use authn::{JwtAlgorithm, TokenProfile};
// template:end oidc-jwt:config-jwt-input-exports
// template:begin oidc-introspection:config-introspection-exports
pub use authn::IntrospectionCacheConfig;
// template:end oidc-introspection:config-introspection-exports
// template:begin service-auth:config-service-auth-export
pub use service_auth::{ServiceAuthConfig, ServiceSigningKey};
// template:end service-auth:config-service-auth-export
// template:begin postgres:config-export
pub use load::{MigrationLoadError, load_migration};
pub use postgres::{
    MigrationConfig, MigrationPostgresConfig, PostgresConfig, PostgresSessionBudgets,
};
// template:end postgres:config-export
// template:begin http-idempotency:config-http-idempotency-export
pub use http_idempotency::HttpIdempotencyConfig;
// template:end http-idempotency:config-http-idempotency-export
// template:begin jobs:config-jobs-export
pub use jobs::{JobsConfig, JobsOperatorConfig};
pub use load::{JobsOperatorLoadError, load_jobs_operator};
pub use postgres::OperatorPostgresConfig;
// template:end jobs:config-jobs-export
// template:begin webhooks:config-webhooks-export
pub use webhooks::{WebhookEndpointConfig, WebhooksConfig};
// template:end webhooks:config-webhooks-export
pub use load::load_temporal_worker;
pub use temporal::{TemporalClientConfig, TemporalTlsRotation};
pub use temporal_worker::{
    TemporalDeploymentConfig, TemporalQueueConfig, TemporalTunerConfig, TemporalVersioningBehavior,
    TemporalWorkerConfig, TemporalWorkerRuntimeConfig,
};
pub use validate::{ValidationError, ValidationErrors};

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct ConfigInput {
    app: app::AppInput,
    http: http::HttpInput,
    // template:begin grpc:config-input-field
    grpc: grpc::GrpcInput,
    // template:end grpc:config-input-field
    // template:begin inbound-webhooks:config-inbound-webhooks-input-field
    inbound_webhooks: inbound_webhooks::InboundWebhooksInput,
    // template:end inbound-webhooks:config-inbound-webhooks-input-field
    // template:begin webhooks:config-outbound-webhooks-input-field
    webhooks: webhooks::ServiceWebhooksInput,
    // template:end webhooks:config-outbound-webhooks-input-field
    health: HealthConfig,
    log: LogConfig,
    runtime: RuntimeConfig,
    observability: observability::ObservabilityInput,
    // template:begin cache:config-input-field
    cache: cache::CacheInput,
    // template:end cache:config-input-field
    // template:begin object-storage:config-input-field
    object_storage: object_storage::ObjectStorageInput,
    // template:end object-storage:config-input-field
    // template:begin authn:config-input-field
    authn: authn::AuthnInput,
    // template:end authn:config-input-field
    // template:begin service-auth:config-service-auth-input-field
    service_auth: service_auth::ServiceAuthInput,
    // template:end service-auth:config-service-auth-input-field
    // template:begin postgres:config-input-field
    postgres: postgres::PostgresInput,
    // template:end postgres:config-input-field
    // template:begin http-idempotency:config-http-idempotency-input-field
    http_idempotency: HttpIdempotencyConfig,
    // template:end http-idempotency:config-http-idempotency-input-field
    temporal: temporal::TemporalInput,
}

impl ConfigInput {
    pub(crate) fn admit(
        self,
        build: BuildInfo,
        shutdown_tail: ShutdownTail,
    ) -> Result<Config, ValidationErrors> {
        admit_with(|v| {
            let app = self.app.admit(build, v);
            #[allow(
                unused_variables,
                reason = "only the gRPC profile consumes the HTTP drain budget"
            )]
            let (http, drain_budget) = self.http.admit(shutdown_tail, v);
            // template:begin service-auth:config-grpc-enabled
            let grpc_enabled = self.grpc.enabled;
            // template:end service-auth:config-grpc-enabled
            // template:begin grpc:config-admit
            let grpc = self.grpc.admit(drain_budget, v);
            // template:end grpc:config-admit
            // template:begin inbound-webhooks:config-inbound-webhooks-validate
            let inbound_webhooks = self.inbound_webhooks.admit(self.postgres.enabled, v);
            // template:end inbound-webhooks:config-inbound-webhooks-validate
            // template:begin webhooks:config-outbound-webhooks-validate
            let outbound_webhook_endpoints = self.webhooks.admit(v);
            // template:end webhooks:config-outbound-webhooks-validate
            self.health.validate(v);
            self.log.validate(v);
            self.runtime.validate(v);
            let observability = self.observability.admit(v);
            // template:begin cache:config-admit
            let cache = self.cache.admit(app.is_local_development(), v);
            // template:end cache:config-admit
            // template:begin object-storage:config-admit
            let object_storage = self.object_storage.admit(app.is_local_development(), v);
            // template:end object-storage:config-admit
            // template:begin authn:config-admit
            let authn = self.authn.admit(v);
            // template:end authn:config-admit
            // template:begin service-auth:config-service-auth-admit
            let service_auth = self
                .service_auth
                .admit(app.is_local_development(), grpc_enabled, v);
            if let (Ok(Some(listener)), Ok(ServiceAuthConfig::Jwt { .. })) = (&grpc, &service_auth)
                && matches!(listener.security, GrpcListenerSecurity::Plaintext)
            {
                v.reject(ValidationError::new(
                    "grpc.security",
                    "must be tls when the listener verifies service JWTs",
                ));
            }
            // template:end service-auth:config-service-auth-admit
            // template:begin postgres:config-admit
            let postgres = self.postgres.admit(v);
            // template:end postgres:config-admit
            // template:begin http-idempotency:config-http-idempotency-validate
            self.http_idempotency.validate(v);
            // template:end http-idempotency:config-http-idempotency-validate
            let temporal = self.temporal.admit(
                app.is_local_development(),
                &observability.otel.service_name,
                &app.version,
                v,
            );
            Ok(Config {
                app,
                http: http?,
                // template:begin grpc:config-admitted-field
                grpc: grpc?,
                // template:end grpc:config-admitted-field
                // template:begin inbound-webhooks:config-inbound-webhooks-admitted-field
                inbound_webhooks,
                // template:end inbound-webhooks:config-inbound-webhooks-admitted-field
                // template:begin webhooks:config-outbound-webhooks-admitted-field
                outbound_webhook_endpoints,
                // template:end webhooks:config-outbound-webhooks-admitted-field
                health: self.health,
                log: self.log,
                runtime: self.runtime,
                observability,
                // template:begin cache:config-admitted-field
                cache: cache?,
                // template:end cache:config-admitted-field
                // template:begin object-storage:config-admitted-field
                object_storage: object_storage?,
                // template:end object-storage:config-admitted-field
                // template:begin authn:config-admitted-field
                authn: authn?,
                // template:end authn:config-admitted-field
                // template:begin service-auth:config-service-auth-admitted-field
                service_auth: service_auth?,
                // template:end service-auth:config-service-auth-admitted-field
                // template:begin postgres:config-admitted-field
                postgres: postgres?,
                // template:end postgres:config-admitted-field
                // template:begin http-idempotency:config-http-idempotency-admitted-field
                http_idempotency: self.http_idempotency,
                // template:end http-idempotency:config-http-idempotency-admitted-field
                temporal: temporal?,
            })
        })
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    app: AppConfig,
    http: HttpConfig,
    // template:begin grpc:config-field
    grpc: Option<GrpcConfig>,
    // template:end grpc:config-field
    // template:begin inbound-webhooks:config-inbound-webhooks-field
    inbound_webhooks: std::collections::BTreeMap<String, InboundWebhookEndpointConfig>,
    // template:end inbound-webhooks:config-inbound-webhooks-field
    // template:begin webhooks:config-outbound-webhooks-field
    outbound_webhook_endpoints: Vec<String>,
    // template:end webhooks:config-outbound-webhooks-field
    health: HealthConfig,
    log: LogConfig,
    runtime: RuntimeConfig,
    observability: ObservabilityConfig,
    // template:begin cache:config-field
    cache: Option<CacheConfig>,
    // template:end cache:config-field
    // template:begin object-storage:config-field
    object_storage: Option<ObjectStorageConfig>,
    // template:end object-storage:config-field
    // template:begin authn:config-field
    authn: AuthnConfig,
    // template:end authn:config-field
    // template:begin service-auth:config-service-auth-field
    service_auth: ServiceAuthConfig,
    // template:end service-auth:config-service-auth-field
    // template:begin postgres:config-field
    postgres: Option<PostgresConfig>,
    // template:end postgres:config-field
    // template:begin http-idempotency:config-http-idempotency-field
    http_idempotency: HttpIdempotencyConfig,
    // template:end http-idempotency:config-http-idempotency-field
    temporal: Option<TemporalClientConfig>,
}

impl Config {
    #[must_use]
    pub const fn app(&self) -> &AppConfig {
        &self.app
    }

    #[must_use]
    pub const fn http(&self) -> &HttpConfig {
        &self.http
    }

    // template:begin grpc:config-accessor
    #[must_use]
    pub const fn grpc(&self) -> Option<&GrpcConfig> {
        self.grpc.as_ref()
    }
    // template:end grpc:config-accessor

    // template:begin inbound-webhooks:config-inbound-webhooks-accessor
    #[must_use]
    pub const fn inbound_webhooks(
        &self,
    ) -> &std::collections::BTreeMap<String, InboundWebhookEndpointConfig> {
        &self.inbound_webhooks
    }
    // template:end inbound-webhooks:config-inbound-webhooks-accessor

    // template:begin webhooks:config-outbound-webhooks-accessor
    #[must_use]
    pub fn outbound_webhook_endpoints(&self) -> &[String] {
        &self.outbound_webhook_endpoints
    }
    // template:end webhooks:config-outbound-webhooks-accessor

    #[must_use]
    pub const fn health(&self) -> &HealthConfig {
        &self.health
    }

    #[must_use]
    pub const fn log(&self) -> &LogConfig {
        &self.log
    }

    #[must_use]
    pub const fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }

    #[must_use]
    pub const fn observability(&self) -> &ObservabilityConfig {
        &self.observability
    }

    // template:begin cache:config-accessor
    #[must_use]
    pub const fn cache(&self) -> Option<&CacheConfig> {
        self.cache.as_ref()
    }
    // template:end cache:config-accessor

    // template:begin object-storage:config-accessor
    #[must_use]
    pub const fn object_storage(&self) -> Option<&ObjectStorageConfig> {
        self.object_storage.as_ref()
    }
    // template:end object-storage:config-accessor

    // template:begin authn:config-accessor
    #[must_use]
    pub const fn authn(&self) -> &AuthnConfig {
        &self.authn
    }
    // template:end authn:config-accessor

    // template:begin service-auth:config-service-auth-accessor
    #[must_use]
    pub const fn service_auth(&self) -> &ServiceAuthConfig {
        &self.service_auth
    }
    // template:end service-auth:config-service-auth-accessor

    // template:begin postgres:config-accessor
    #[must_use]
    pub const fn postgres(&self) -> Option<&PostgresConfig> {
        self.postgres.as_ref()
    }
    // template:end postgres:config-accessor

    // template:begin http-idempotency:config-http-idempotency-accessor
    #[must_use]
    pub const fn http_idempotency(&self) -> &HttpIdempotencyConfig {
        &self.http_idempotency
    }
    // template:end http-idempotency:config-http-idempotency-accessor

    #[must_use]
    pub const fn temporal(&self) -> Option<&TemporalClientConfig> {
        self.temporal.as_ref()
    }

    #[must_use]
    pub fn finite_listener_connection_caps(&self) -> u64 {
        finite_http_connection_caps(self.http.listener(), &self.observability.metrics)
            // template:begin grpc:config-finite-listener-connection-caps
            + self
                .grpc
                .as_ref()
                .and_then(|grpc| grpc.max_connections)
                .map_or(0, |cap| u64::from(cap.get()))
        // template:end grpc:config-finite-listener-connection-caps
    }
}

fn finite_http_connection_caps(http: &HttpListenerConfig, metrics: &MetricsConfig) -> u64 {
    http.max_connections.map_or(0, |cap| {
        u64::from(cap.get()) * (1 + u64::from(metrics.addr.is_some()))
    })
}
