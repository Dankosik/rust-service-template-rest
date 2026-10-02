//! Application-owned generated-service registration for native gRPC.
//!
//! The transport owns listener policy. This module only gives a derived
//! service one registration hook and converts admitted limits and TLS
//! material.

use std::sync::Arc;

use infra_grpc::ServerTlsMaterial;
use rustls::ServerConfig;
use service_config::{Config, GrpcSecurity, ValidationError};

/// Registers this service's generated native gRPC adapters. Bootstrap calls
/// it once, after it opened the dependencies, with the state the HTTP routes
/// also receive: a generated server takes what it needs from that state when
/// it is constructed.
pub type GrpcRegistration =
    fn(&mut infra_grpc::Services, &crate::AppState) -> Result<(), infra_grpc::Error>;

/// Build the immutable service registry that the transport prepares.
///
/// The default service supplies no business registration; examples and derived
/// services opt in through [`crate::run_with_grpc`].
pub(crate) fn services(
    registration: Option<GrpcRegistration>,
    state: &crate::AppState,
) -> Result<infra_grpc::Services, infra_grpc::Error> {
    let mut services = infra_grpc::Services::new();
    if let Some(registration) = registration {
        registration(&mut services, state)?;
    }
    Ok(services)
}

/// The admitted `grpc` limits in the transport's form.
pub(crate) fn limits(config: &Config) -> infra_grpc::Limits {
    infra_grpc::Limits {
        request_timeout: config.grpc.request_timeout,
        max_in_flight: config.grpc.in_flight_cap(),
        max_connections: config.grpc.connection_cap(),
        max_connection_age: config.grpc.connection_age(),
    }
}

/// Convert admitted TLS material into a listener config.
///
/// Configuration validates presence and source policy before this runs. The
/// conversion does not read paths or perform network I/O, and borrows the
/// private key without copying it. Plaintext has no server config.
///
/// # Errors
///
/// Returns the configuration key whose value is missing, or the TLS keys
/// when the material cannot form a server config.
pub(crate) fn tls(config: &Config) -> Result<Option<Arc<ServerConfig>>, ValidationError> {
    let grpc = &config.grpc;
    let tls_required = |key| ValidationError::new(key, "is required when grpc.security is tls");
    match grpc.security.ok_or_else(|| {
        ValidationError::new("grpc.security", "is required when grpc.enabled is true")
    })? {
        GrpcSecurity::Plaintext => Ok(None),
        GrpcSecurity::Tls => {
            let material = ServerTlsMaterial {
                certificate_pem: grpc
                    .certificate
                    .as_deref()
                    .ok_or_else(|| tls_required("grpc.certificate"))?,
                private_key_pem: grpc
                    .private_key
                    .as_ref()
                    .ok_or_else(|| tls_required("grpc.private_key"))?,
                client_ca_pem: grpc.client_ca.as_deref(),
            };
            let server_config = infra_grpc::server_tls_config(material).map_err(|error| {
                let key = match error {
                    infra_grpc::Error::InvalidPrivateKey => "grpc.private_key",
                    infra_grpc::Error::InvalidCaCertificate => "grpc.client_ca",
                    _ => "grpc.certificate",
                };
                ValidationError::new(key, error.to_string())
            })?;
            Ok(Some(server_config))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unusable_tls_material_names_its_configuration_key() {
        let mut config = Config::default();
        config.grpc.security = Some(GrpcSecurity::Tls);
        config.grpc.certificate = Some("not a certificate".to_owned());
        config.grpc.private_key = Some("not a key".into());
        let err = tls(&config).expect_err("garbage PEM must not form a server config");
        assert_eq!(err.key, "grpc.certificate");
    }
}
