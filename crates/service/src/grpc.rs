//! Application-owned generated-service registration for native gRPC.
//!
//! The transport owns listener policy. This module only gives a derived
//! service one registration hook for its generated, governed adapters.

use infra_grpc::Services;

/// Registers this service's generated native gRPC adapters.
pub type GrpcRegistration = fn(&mut Services) -> Result<(), infra_grpc::Error>;

/// Build the immutable service registry that the transport prepares.
///
/// The default service supplies no business registration; examples and derived
/// services opt in through [`crate::run_with_grpc`].
pub(crate) fn services(registration: Option<GrpcRegistration>) -> Result<Services, infra_grpc::Error> {
    let mut services = Services::new();
    if let Some(registration) = registration {
        registration(&mut services)?;
    }
    Ok(services)
}
