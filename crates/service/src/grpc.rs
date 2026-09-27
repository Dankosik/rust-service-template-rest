//! Application-owned generated-service registration for native gRPC.
//!
//! The transport owns listener policy. This module only gives a derived
//! service one registration hook for its generated, governed adapters.

use infra_grpc::{ServerOptions, ServerSecurity, ServerTlsMaterial, Services};
use secrecy::ExposeSecret as _;
use service_config::{Config, GrpcSecurity};

/// Registers this service's generated native gRPC adapters.
pub type GrpcRegistration = fn(&mut Services) -> Result<(), infra_grpc::Error>;

/// Build the immutable service registry that the transport prepares.
///
/// The default service supplies no business registration; examples and derived
/// services opt in through [`crate::run_with_grpc`].
pub(crate) fn services(
    registration: Option<GrpcRegistration>,
) -> Result<Services, infra_grpc::Error> {
    let mut services = Services::new();
    if let Some(registration) = registration {
        registration(&mut services)?;
    }
    Ok(services)
}

/// Convert the immutable service snapshot into transport-only listener input.
///
/// Configuration validates presence and source policy before this runs. The
/// conversion does not read paths or perform TLS/network I/O.
pub(crate) fn server_options(config: &Config) -> Result<ServerOptions, infra_grpc::Error> {
    let grpc = &config.grpc;
    let security = match grpc
        .security
        .ok_or(infra_grpc::Error::InvalidConfiguration)?
    {
        GrpcSecurity::Plaintext => ServerSecurity::Plaintext,
        GrpcSecurity::Tls => ServerSecurity::Tls(ServerTlsMaterial {
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
        }),
    };
    Ok(ServerOptions {
        security,
        effective_drain_budget: config.http.effective_drain_budget(),
    })
}
