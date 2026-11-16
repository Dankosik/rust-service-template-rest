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

/// Called once, only when `grpc.enabled` is true, after dependencies opened,
/// with the state the HTTP routes also receive. Must return promptly.
pub type GrpcRegistration = fn(
    &mut infra_grpc::Services,
    &crate::AppState,
    &mut BackgroundRegistration<'_>,
) -> Result<(), infra_grpc::Error>;

pub(super) struct Prepared {
    pub(super) router: axum::Router,
    pub(super) tls: Option<Arc<ServerConfig>>,
    pub(super) options: ServerOptions,
}

/// Runs before readiness admission, so a refused registration or verifier binds nothing.
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

/// Does not read paths or perform network I/O.
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
