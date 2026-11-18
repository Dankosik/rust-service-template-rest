use std::net::SocketAddr;
use std::time::Duration;

use infra_grpc::{Authentication as GrpcAuthentication, ServerTlsMaterial};
use infra_http_transport::{ConnectionLimits, Http2Options, TlsOptions};
use service_config::{GrpcConfig, GrpcListenerSecurity, ValidationError};

use super::background::{Background, BackgroundRegistration};
// template:begin service-auth:bootstrap-grpc-service-auth-import
use super::service_auth::PreparedServiceAuth;
// template:end service-auth:bootstrap-grpc-service-auth-import
use super::BootstrapError;

pub type GrpcRegistration = fn(
    &mut infra_grpc::Services,
    &crate::AppState,
    &mut BackgroundRegistration<'_>,
) -> Result<(), infra_grpc::Error>;

pub(super) struct Prepared {
    pub(super) addr: SocketAddr,
    pub(super) app: infra_grpc::Application,
    pub(super) tls: Option<TlsOptions>,
    pub(super) options: Http2Options,
}

pub(super) fn prepare(
    grpc: &GrpcConfig,
    registration: Option<GrpcRegistration>,
    state: &crate::AppState,
    // template:begin grpc-authn:bootstrap-grpc-bearer-parameter
    bearer: Option<&infra_bearerauthn::Verifier>,
    // template:end grpc-authn:bootstrap-grpc-bearer-parameter
    // template:begin service-auth:bootstrap-grpc-service-auth-parameter
    service_auth: &PreparedServiceAuth,
    // template:end service-auth:bootstrap-grpc-service-auth-parameter
    background: &mut Background,
) -> Result<Prepared, BootstrapError> {
    let authentication = GrpcAuthentication::None;
    // template:begin grpc-authn:bootstrap-grpc-bearer
    let authentication = bearer
        .cloned()
        .map_or(authentication, GrpcAuthentication::Bearer);
    // template:end grpc-authn:bootstrap-grpc-bearer
    // template:begin service-auth:bootstrap-grpc-service-tokens
    let authentication = service_auth
        .verifier
        .clone()
        .map_or(authentication, GrpcAuthentication::ServiceTokens);
    // template:end service-auth:bootstrap-grpc-service-tokens
    // template:begin grpc-authn:bootstrap-grpc-verifier
    if matches!(authentication, GrpcAuthentication::None) {
        return Err(ValidationError::new(
            "grpc.enabled",
            infra_grpc::Error::UnauthenticatedBusinessCalls.to_string(),
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
    let app = infra_grpc::router(
        services,
        state.readiness.clone(),
        authentication,
        limits,
        grpc.trust_trace_context,
    )?;
    Ok(Prepared {
        addr: grpc.addr,
        app,
        tls: tls(&grpc.security)?,
        options: infra_grpc::server_options(ConnectionLimits {
            max_connections: grpc.max_connections,
            max_connection_age: grpc.max_connection_age,
        }),
    })
}

fn limits(grpc: &GrpcConfig) -> infra_grpc::Limits {
    infra_grpc::Limits {
        request_timeout: grpc.request_timeout,
        max_in_flight: grpc.max_in_flight,
        max_admission_wait: grpc.admission_wait,
    }
}

fn tls(security: &GrpcListenerSecurity) -> Result<Option<TlsOptions>, ValidationError> {
    let GrpcListenerSecurity::Tls(material) = security else {
        return Ok(None);
    };
    let material = ServerTlsMaterial {
        certificate_pem: &material.certificate,
        private_key_pem: &material.private_key,
        client_ca_pem: material.client_ca.as_deref(),
    };
    infra_grpc::server_tls_config(material)
        .map(|config| {
            Some(TlsOptions {
                config,
                handshake_timeout: Duration::from_secs(5),
            })
        })
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
