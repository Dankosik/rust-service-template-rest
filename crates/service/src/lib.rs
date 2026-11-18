pub mod api;

mod bootstrap;
pub use bootstrap::{BackgroundFailureReporter, BackgroundRegistration, run_with};
mod state;
pub use state::AppState;
// template:begin grpc:service-registration-module
pub use bootstrap::{GrpcRegistration, run_with_grpc_resource};
// template:end grpc:service-registration-module

use std::ffi::OsString;
use std::process::ExitCode;

pub type HttpResource<C> = Box<
    dyn FnOnce(
        &service_config::Config,
        &mut BackgroundRegistration<'_>,
    ) -> Result<C, Box<dyn std::error::Error + Send + Sync>>,
>;

#[derive(derive_more::Debug)]
#[debug("ServiceRegistration {{ .. }}")]
pub struct ServiceRegistration<C = ()> {
    pub http_resource: Option<HttpResource<C>>,
    // template:begin grpc:service-grpc-registration-field
    pub grpc: Option<GrpcRegistration>,
    // template:end grpc:service-grpc-registration-field
}

impl<C> Default for ServiceRegistration<C> {
    fn default() -> Self {
        Self {
            http_resource: None,
            // template:begin grpc:service-default-grpc-registration
            grpc: None,
            // template:end grpc:service-default-grpc-registration
        }
    }
}

pub fn run<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    run_with(args, ServiceRegistration::<()>::default())
}

// template:begin grpc:service-run-with-grpc
pub fn run_with_grpc<I>(args: I, registration: GrpcRegistration) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    run_with(
        args,
        ServiceRegistration {
            grpc: Some(registration),
            ..ServiceRegistration::<()>::default()
        },
    )
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
