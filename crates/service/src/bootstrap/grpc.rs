//! The native gRPC listener's startup step.
//!
//! The transport owns listener policy. This step gives a derived service one
//! registration hook, selects how the listener authenticates its callers, and
//! converts admitted limits and TLS material.

use std::sync::Arc;

use health::Readiness;
use infra_grpc::ServerTlsMaterial;
use infra_http::ServerOptions;
use rustls::ServerConfig;
use service_config::{Config, GrpcConfig, GrpcListenerSecurity, ValidationError};

use super::background::{Background, BackgroundRegistration};
// template:begin service-auth:bootstrap-grpc-service-auth-import
use super::service_auth::PreparedServiceAuth;
// template:end service-auth:bootstrap-grpc-service-auth-import
use super::{BootstrapError, PreparedAuth};

/// Registers this service's generated native gRPC adapters. Bootstrap calls
/// it once, only when `grpc.enabled` is true, after it opened the
/// dependencies, with the state the HTTP routes also receive: a generated
/// server takes what it needs from that state when it is constructed. The
/// borrowed background capability registers a feature manager in the same
/// production lifecycle; registration must return promptly.
pub type GrpcRegistration = fn(
    &mut infra_grpc::Services,
    &crate::AppState,
    &mut BackgroundRegistration<'_>,
) -> Result<(), infra_grpc::Error>;

/// The gRPC listener, ready to bind.
pub(super) struct Prepared {
    pub(super) router: axum::Router,
    pub(super) tls: Option<Arc<ServerConfig>>,
    pub(super) options: ServerOptions,
}

/// Build the listener's router from the registered services before readiness
/// admission, so a refused registration or verifier binds nothing.
///
/// The default service supplies no business registration; examples and derived
/// services opt in through [`crate::run_with_grpc`].
pub(super) fn prepare(
    config: &Config,
    registration: Option<GrpcRegistration>,
    state: &crate::AppState,
    readiness: &Readiness,
    #[allow(unused_variables, reason = "bearer authentication alone reads it")] auth: &PreparedAuth,
    // template:begin service-auth:bootstrap-grpc-service-auth-parameter
    service_auth: &PreparedServiceAuth,
    // template:end service-auth:bootstrap-grpc-service-auth-parameter
    background: &mut Background,
) -> Result<Prepared, BootstrapError> {
    #[allow(
        unused_variables,
        reason = "bearer authentication alone reads it; service tokens replace it"
    )]
    let service_tokens = false;
    // template:begin service-auth:bootstrap-grpc-service-tokens
    #[allow(
        unused_variables,
        reason = "bearer authentication alone reads it; without AUTHN nothing does"
    )]
    let service_tokens = service_auth.verifier.is_some();
    // template:end service-auth:bootstrap-grpc-service-tokens
    // template:begin grpc-authn:bootstrap-grpc-verifier
    let verifier = match auth {
        PreparedAuth::Enabled(verifier) => Some((**verifier).clone()),
        // Service tokens replace bearer authentication on this listener.
        PreparedAuth::None if service_tokens => None,
        PreparedAuth::None => {
            return Err(ValidationError::new(
                "grpc.enabled",
                "requires authn.mode = oidc-jwt or oidc-introspection",
            )
            .into());
        }
    };
    // template:end grpc-authn:bootstrap-grpc-verifier
    let limits = limits(&config.grpc);
    let mut services = infra_grpc::Services::new();
    if let Some(registration) = registration {
        registration(
            &mut services,
            state,
            &mut BackgroundRegistration { background },
        )?;
    }
    // template:begin service-auth:bootstrap-grpc-service-auth
    if let Some(verifier) = &service_auth.verifier {
        services.authenticate_callers(verifier.clone());
    }
    // template:end service-auth:bootstrap-grpc-service-auth
    let router = infra_grpc::router(
        services,
        readiness.reader(),
        // template:begin grpc-authn:bootstrap-grpc-verifier-argument
        verifier,
        // template:end grpc-authn:bootstrap-grpc-verifier-argument
        limits,
    )?;
    Ok(Prepared {
        router,
        tls: tls(&config.grpc)?,
        options: infra_grpc::server_options(limits),
    })
}

fn limits(grpc: &GrpcConfig) -> infra_grpc::Limits {
    infra_grpc::Limits {
        request_timeout: grpc.request_timeout,
        max_in_flight: grpc.in_flight_cap(),
        max_admission_wait: grpc.admission_wait,
        max_connections: grpc.connection_cap(),
        max_connection_age: grpc.connection_age_cap(),
    }
}

/// Convert admitted TLS material into a listener config.
///
/// The conversion does not read paths or perform network I/O, and borrows the
/// private key without copying it. Plaintext has no server config.
///
/// # Errors
///
/// Returns the configuration key whose value is missing, or whose material
/// cannot form a server config.
fn tls(grpc: &GrpcConfig) -> Result<Option<Arc<ServerConfig>>, ValidationError> {
    let GrpcListenerSecurity::Tls {
        certificate,
        private_key,
        client_ca,
    } = grpc.listener_security()?
    else {
        return Ok(None);
    };
    let material = ServerTlsMaterial {
        certificate_pem: certificate,
        private_key_pem: private_key,
        client_ca_pem: client_ca,
    };
    infra_grpc::server_tls_config(material)
        .map(Some)
        .map_err(|error| {
            let key = match error {
                infra_grpc::TlsMaterialError::InvalidCertificate => "grpc.certificate",
                infra_grpc::TlsMaterialError::InvalidPrivateKey => "grpc.private_key",
                infra_grpc::TlsMaterialError::InvalidCaCertificate => "grpc.client_ca",
            };
            ValidationError::new(key, error.to_string())
        })
}

#[cfg(test)]
mod tests {
    use service_config::GrpcSecurity;

    use super::*;

    #[test]
    fn unusable_tls_material_names_its_configuration_key() {
        let config = GrpcConfig {
            security: Some(GrpcSecurity::Tls),
            certificate: Some("not a certificate".to_owned()),
            private_key: Some("not a key".into()),
            ..GrpcConfig::default()
        };
        let err = tls(&config).expect_err("garbage PEM must not form a server config");
        assert_eq!(err.key, "grpc.certificate");
    }
}
