use std::sync::Arc;

use infra_grpc::{Authentication as GrpcAuthentication, ServerTlsMaterial};
use infra_http_transport::ServerOptions;
use rustls::ServerConfig;
use std::net::SocketAddr;

use service_config::{GrpcConfig, GrpcListenerSecurity, ValidationError};

use super::background::{Background, BackgroundRegistration};
// template:begin service-auth:bootstrap-grpc-service-auth-import
use super::service_auth::PreparedServiceAuth;
// template:end service-auth:bootstrap-grpc-service-auth-import
use super::BootstrapError;
use infra_http::Authentication;

pub type GrpcRegistration = fn(
    &mut infra_grpc::Services,
    &crate::AppState,
    &mut BackgroundRegistration<'_>,
) -> Result<(), infra_grpc::Error>;

pub(super) struct Prepared {
    pub(super) addr: SocketAddr,
    pub(super) router: axum::Router,
    pub(super) tls: Option<Arc<ServerConfig>>,
    pub(super) options: ServerOptions,
}

pub(super) fn prepare(
    grpc: &GrpcConfig,
    registration: Option<GrpcRegistration>,
    state: &crate::AppState,
    auth: &Authentication,
    // template:begin service-auth:bootstrap-grpc-service-auth-parameter
    service_auth: &PreparedServiceAuth,
    // template:end service-auth:bootstrap-grpc-service-auth-parameter
    background: &mut Background,
) -> Result<Prepared, BootstrapError> {
    let authentication = match (
        auth,
        // template:begin service-auth:bootstrap-grpc-service-verifier
        service_auth.verifier.as_ref(),
        // template:end service-auth:bootstrap-grpc-service-verifier
    ) {
        // template:begin service-auth:bootstrap-grpc-service-tokens
        (_, Some(verifier)) => GrpcAuthentication::ServiceTokens(verifier.clone()),
        // template:end service-auth:bootstrap-grpc-service-tokens
        // template:begin grpc-authn:bootstrap-grpc-bearer
        (Authentication::Bearer(verifier), ..) => GrpcAuthentication::Bearer((**verifier).clone()),
        // template:end grpc-authn:bootstrap-grpc-bearer
        (Authentication::Public, ..) => GrpcAuthentication::None,
    };
    // template:begin grpc-authn:bootstrap-grpc-verifier
    if matches!(authentication, GrpcAuthentication::None) {
        return Err(ValidationError::new(
            "grpc.enabled",
            "requires authn.mode = oidc-jwt or oidc-introspection",
        )
        .into());
    }
    // template:end grpc-authn:bootstrap-grpc-verifier
    let limits = limits(grpc);
    let mut services = infra_grpc::Services::new();
    if let Some(registration) = registration {
        registration(
            &mut services,
            state,
            &mut BackgroundRegistration { background },
        )?;
    }
    let router = infra_grpc::router(services, state.readiness.clone(), authentication, limits)?;
    Ok(Prepared {
        addr: grpc.addr,
        router,
        tls: tls(&grpc.security)?,
        options: infra_grpc::server_options(limits),
    })
}

fn limits(grpc: &GrpcConfig) -> infra_grpc::Limits {
    infra_grpc::Limits {
        request_timeout: grpc.request_timeout,
        max_in_flight: grpc.in_flight_cap(),
        max_admission_wait: grpc.admission_wait_cap(),
        max_connections: grpc.connection_cap(),
        max_connection_age: grpc.connection_age_cap(),
    }
}

fn tls(security: &GrpcListenerSecurity) -> Result<Option<Arc<ServerConfig>>, ValidationError> {
    let GrpcListenerSecurity::Tls(material) = security else {
        return Ok(None);
    };
    let material = ServerTlsMaterial {
        certificate_pem: &material.certificate,
        private_key_pem: &material.private_key,
        client_ca_pem: material.client_ca.as_deref(),
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
    use service_config::GrpcTlsMaterial;

    use super::*;

    #[test]
    fn unusable_tls_material_names_its_configuration_key() {
        let security = GrpcListenerSecurity::Tls(GrpcTlsMaterial {
            certificate: "not a certificate".to_owned(),
            private_key: "not a key".into(),
            client_ca: None,
        });
        let err = tls(&security).expect_err("garbage PEM must not form a server config");
        assert_eq!(err.key, "grpc.certificate");
    }
}
