//! Application-owned generated-service registration for native gRPC.
//!
//! The transport owns listener policy. This module only gives a derived
//! service one registration hook and converts admitted TLS material.

use std::sync::Arc;

use infra_grpc::ServerTlsMaterial;
use rustls::ServerConfig;
use service_config::{Config, GrpcSecurity};

use crate::bootstrap::BootstrapError;

/// Registers this service's generated native gRPC adapters.
pub type GrpcRegistration = fn(&mut infra_grpc::Services) -> Result<(), infra_grpc::Error>;

/// Build the immutable service registry that the transport prepares.
///
/// The default service supplies no business registration; examples and derived
/// services opt in through [`crate::run_with_grpc`].
pub(crate) fn services(
    registration: Option<GrpcRegistration>,
) -> Result<infra_grpc::Services, infra_grpc::Error> {
    let mut services = infra_grpc::Services::new();
    if let Some(registration) = registration {
        registration(&mut services)?;
    }
    Ok(services)
}

/// Convert admitted TLS material into a listener config.
///
/// Configuration validates presence and source policy before this runs. The
/// conversion does not read paths or perform network I/O, and borrows the
/// private key without copying it. Plaintext has no server config.
///
/// # Errors
///
/// Returns [`BootstrapError::GrpcInvalid`] when security or TLS material is
/// unset, and the transport error naming unusable TLS material.
pub(crate) fn tls(config: &Config) -> Result<Option<Arc<ServerConfig>>, BootstrapError> {
    let grpc = &config.grpc;
    let missing = BootstrapError::GrpcInvalid {
        reason: "grpc.security, and for tls grpc.certificate and grpc.private_key, must be set",
    };
    match grpc.security {
        None => Err(missing),
        Some(GrpcSecurity::Plaintext) => Ok(None),
        Some(GrpcSecurity::Tls) => {
            let (Some(certificate_pem), Some(private_key_pem)) =
                (grpc.certificate.as_deref(), grpc.private_key.as_ref())
            else {
                return Err(missing);
            };
            let material = ServerTlsMaterial {
                certificate_pem,
                private_key_pem,
                client_ca_pem: grpc.client_ca.as_deref(),
            };
            Ok(Some(infra_grpc::server_tls_config(material)?))
        }
    }
}
