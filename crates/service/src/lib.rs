pub mod api;

mod bootstrap;
pub use bootstrap::{BackgroundFailureReporter, BackgroundRegistration};
mod state;
pub use state::AppState;
// template:begin grpc:service-registration-module
pub use bootstrap::GrpcRegistration;
// template:end grpc:service-registration-module

use std::ffi::OsString;
use std::process::ExitCode;

/// `args` is the whole command line, program name first.
// template:begin grpc:service-run-grpc-doc
///
/// No application gRPC service is registered: a selected gRPC listener
/// exposes only its transport-owned health surface until a derived service
/// calls [`run_with_grpc`].
// template:end grpc:service-run-grpc-doc
pub fn run<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    bootstrap::run(
        args,
        // template:begin grpc:service-run-registration-argument
        None,
        // template:end grpc:service-run-registration-argument
    )
}

// template:begin grpc:service-run-with-grpc
/// The registration hook runs only when `grpc.enabled` is true.
pub fn run_with_grpc<I>(args: I, registration: GrpcRegistration) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    bootstrap::run(args, Some(registration))
}
// template:end grpc:service-run-with-grpc
