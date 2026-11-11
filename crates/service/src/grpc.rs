//! Application-owned generated-service registration for native gRPC.
//!
//! The transport owns listener policy. This module only gives a derived
//! service one registration hook and converts admitted limits and TLS
//! material.

use std::sync::Arc;

use infra_grpc::ServerTlsMaterial;
use rustls::ServerConfig;
use service_config::{GrpcConfig, GrpcListenerSecurity, ValidationError};

/// Registers this service's generated native gRPC adapters. Bootstrap calls
/// it once, only when `grpc.enabled` is true, after it opened the
/// dependencies, with the state the HTTP routes also receive: a generated
/// server takes what it needs from that state when it is constructed. The
/// borrowed background capability registers a feature manager in the same
/// production lifecycle; registration must return promptly.
pub type GrpcRegistration = fn(
    &mut infra_grpc::Services,
    &crate::AppState,
    &mut crate::BackgroundRegistration<'_>,
) -> Result<(), infra_grpc::Error>;

/// Build the immutable service registry that the transport prepares.
///
/// The default service supplies no business registration; examples and derived
/// services opt in through [`crate::run_with_grpc`].
pub(crate) fn services(
    registration: Option<GrpcRegistration>,
    state: &crate::AppState,
    background: &mut crate::BackgroundRegistration<'_>,
) -> Result<infra_grpc::Services, infra_grpc::Error> {
    let mut services = infra_grpc::Services::new();
    if let Some(registration) = registration {
        registration(&mut services, state, background)?;
    }
    Ok(services)
}

pub(crate) fn limits(grpc: &GrpcConfig) -> infra_grpc::Limits {
    infra_grpc::Limits {
        request_timeout: grpc.request_timeout,
        max_in_flight: grpc.in_flight_cap(),
        max_admission_wait: grpc.admission_wait,
        max_connections: grpc.connection_cap(),
        max_connection_age: grpc.connection_age(),
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
pub(crate) fn tls(grpc: &GrpcConfig) -> Result<Option<Arc<ServerConfig>>, ValidationError> {
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
