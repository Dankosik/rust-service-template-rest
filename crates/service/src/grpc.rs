//! Application-owned generated-service registration for native gRPC.
//!
//! The transport owns listener policy. This module only gives a derived
//! service one registration hook and converts admitted TLS material.

use std::sync::Arc;

use infra_grpc::ServerTlsMaterial;
use rustls::ServerConfig;
use secrecy::ExposeSecret as _;
use service_config::{Config, GrpcSecurity};

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
/// conversion does not read paths or perform network I/O. Plaintext has no
/// server config.
///
/// # Errors
///
/// Returns [`infra_grpc::Error::InvalidConfiguration`] when security is unset
/// or the TLS material cannot be used.
pub(crate) fn tls(config: &Config) -> Result<Option<Arc<ServerConfig>>, infra_grpc::Error> {
    let grpc = &config.grpc;
    match grpc
        .security
        .ok_or(infra_grpc::Error::InvalidConfiguration)?
    {
        GrpcSecurity::Plaintext => Ok(None),
        GrpcSecurity::Tls => {
            let material = ServerTlsMaterial {
                certificate_pem: grpc
                    .certificate
                    .as_deref()
                    .ok_or(infra_grpc::Error::InvalidConfiguration)?
                    .as_bytes()
                    .to_vec(),
                private_key_pem: grpc
                    .private_key
                    .as_ref()
                    .ok_or(infra_grpc::Error::InvalidConfiguration)?
                    .expose_secret()
                    .as_bytes()
                    .to_vec(),
                client_ca_pem: grpc
                    .client_ca
                    .as_deref()
                    .map(|value| value.as_bytes().to_vec()),
            };
            Ok(Some(infra_grpc::server_tls_config(&material)?))
        }
    }
}
