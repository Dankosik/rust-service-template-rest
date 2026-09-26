//! The service's API contract, assembled from the routers the transport and
//! feature crates own. The `service` binary serves it (`main.rs` and
//! `bootstrap`), the `openapi` binary renders it, and the integration tests
//! hold the committed document to it.

pub mod api;

mod bootstrap;
// template:begin grpc:service-registration-module
mod grpc;
pub use grpc::GrpcRegistration;
// template:end grpc:service-registration-module

use std::ffi::OsString;
use std::process::ExitCode;

/// Run the service with no application gRPC registration.
///
/// A selected gRPC listener still exposes only its transport-owned health
/// surface until a derived service calls [`run_with_grpc`].
pub fn run<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    bootstrap::run(args, None)
}

/// Run the service with one generated gRPC registration hook.
pub fn run_with_grpc<I>(args: I, registration: GrpcRegistration) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    bootstrap::run(args, Some(registration))
}
