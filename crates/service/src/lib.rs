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
pub fn run_with_grpc<I>(args: I, registration: GrpcRegistration) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    bootstrap::run(args, Some(registration))
}
// template:end grpc:service-run-with-grpc

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_returns_control_to_its_caller_for_invalid_arguments_and_help() {
        for (argument, code) in [("--unknown", 2), ("--help", 0)] {
            assert_eq!(
                run(["service", argument].map(OsString::from)),
                ExitCode::from(code)
            );
        }
    }
}
