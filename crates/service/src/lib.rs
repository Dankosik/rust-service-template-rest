pub mod api;

mod bootstrap;
pub use bootstrap::{BackgroundFailureReporter, BackgroundRegistration};
mod state;
pub use state::AppState;

use std::ffi::OsString;
use std::process::ExitCode;

pub type HttpResource<C> = Box<
    dyn FnOnce(
        &service_config::Config,
        &mut BackgroundRegistration<'_>,
    ) -> Result<C, Box<dyn std::error::Error + Send + Sync>>,
>;

// template:begin grpc:service-registration-callbacks
pub type GrpcRegistration = fn(
    &mut infra_grpc::Services,
    &AppState,
    &mut BackgroundRegistration<'_>,
) -> Result<(), infra_grpc::Error>;

type GrpcResourceRegistration = Box<
    dyn FnOnce(
        &mut infra_grpc::Services,
        &AppState,
        &mut BackgroundRegistration<'_>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>,
>;
// template:end grpc:service-registration-callbacks

#[derive(derive_more::Debug)]
#[debug("ServiceRegistration {{ .. }}")]
pub struct ServiceRegistration<C = ()> {
    pub http_resource: Option<HttpResource<C>>,
    // template:begin grpc:service-grpc-registration-field
    pub grpc: Option<GrpcRegistration>,
    // template:end grpc:service-grpc-registration-field
}

impl<C> ServiceRegistration<C> {
    pub fn for_http_resource(
        prepare: impl FnOnce(
            &service_config::Config,
            &mut BackgroundRegistration<'_>,
        ) -> Result<C, Box<dyn std::error::Error + Send + Sync>>
        + 'static,
    ) -> Self {
        Self {
            http_resource: Some(Box::new(prepare)),
            // template:begin grpc:service-http-resource-grpc-default
            grpc: None,
            // template:end grpc:service-http-resource-grpc-default
        }
    }
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

struct StartupRegistration<C> {
    http_resource: Option<HttpResource<C>>,
    // template:begin grpc:bootstrap-registration-callback
    grpc: Option<GrpcResourceRegistration>,
    // template:end grpc:bootstrap-registration-callback
}

impl<C> From<ServiceRegistration<C>> for StartupRegistration<C> {
    fn from(registration: ServiceRegistration<C>) -> Self {
        Self {
            http_resource: registration.http_resource,
            // template:begin grpc:bootstrap-adapt-legacy-registration
            grpc: registration
                .grpc
                .map(|register| -> GrpcResourceRegistration {
                    Box::new(move |services, state, background| {
                        register(services, state, background).map_err(Into::into)
                    })
                }),
            // template:end grpc:bootstrap-adapt-legacy-registration
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

// template:begin grpc:bootstrap-run-with-grpc-resource
pub fn run_with_grpc_resource<I>(
    args: I,
    register: impl FnOnce(
        &mut infra_grpc::Services,
        &AppState,
        &mut BackgroundRegistration<'_>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    + 'static,
) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    bootstrap::run_registered(
        args,
        StartupRegistration::<()> {
            http_resource: None,
            grpc: Some(Box::new(register)),
        },
    )
}
// template:end grpc:bootstrap-run-with-grpc-resource

pub fn run_with<I>(
    args: I,
    registration: ServiceRegistration<impl Clone + Send + Sync + 'static>,
) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    bootstrap::run_registered(args, registration.into())
}

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
