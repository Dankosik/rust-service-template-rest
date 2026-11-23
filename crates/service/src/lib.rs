pub mod api;

mod bootstrap;
pub use bootstrap::{BackgroundFailureReporter, BackgroundRegistration};
mod state;
pub use state::AppState;

use std::ffi::OsString;
use std::process::ExitCode;

pub type RegistrationError = Box<dyn std::error::Error + Send + Sync>;

type HttpResource = Box<
    dyn FnOnce(
        infra_http::Application,
        &service_config::Config,
        &mut BackgroundRegistration<'_>,
    ) -> Result<infra_http::Application, RegistrationError>,
>;

// template:begin grpc:service-registration-callbacks
type GrpcRegistration = Box<
    dyn FnOnce(
        &mut infra_grpc::Services,
        &AppState,
        &mut BackgroundRegistration<'_>,
    ) -> Result<(), RegistrationError>,
>;
// template:end grpc:service-registration-callbacks

#[derive(Default, derive_more::Debug)]
#[debug("ServiceRegistration {{ .. }}")]
pub struct ServiceRegistration {
    http_resource: Option<HttpResource>,
    // template:begin grpc:service-grpc-registration-field
    grpc: Option<GrpcRegistration>,
    // template:end grpc:service-grpc-registration-field
}

impl ServiceRegistration {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn for_http_resource<C>(
        prepare: impl FnOnce(
            &service_config::Config,
            &mut BackgroundRegistration<'_>,
        ) -> Result<C, RegistrationError>
        + 'static,
    ) -> Self
    where
        C: Clone + Send + Sync + 'static,
    {
        Self::new().with_http_resource(prepare)
    }

    #[must_use]
    pub fn with_http_resource<C>(
        mut self,
        prepare: impl FnOnce(
            &service_config::Config,
            &mut BackgroundRegistration<'_>,
        ) -> Result<C, RegistrationError>
        + 'static,
    ) -> Self
    where
        C: Clone + Send + Sync + 'static,
    {
        self.http_resource = Some(Box::new(move |app, config, background| {
            let resource = prepare(config, background)?;
            Ok(app.with_extension(resource))
        }));
        self
    }

    // template:begin grpc:service-with-grpc
    #[must_use]
    pub fn with_grpc(
        mut self,
        register: impl FnOnce(
            &mut infra_grpc::Services,
            &AppState,
            &mut BackgroundRegistration<'_>,
        ) -> Result<(), RegistrationError>
        + 'static,
    ) -> Self {
        self.grpc = Some(Box::new(register));
        self
    }
    // template:end grpc:service-with-grpc
}

pub fn run<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    run_with(args, ServiceRegistration::new())
}

// template:begin grpc:service-run-with-grpc
pub fn run_with_grpc<I>(
    args: I,
    register: impl FnOnce(
        &mut infra_grpc::Services,
        &AppState,
        &mut BackgroundRegistration<'_>,
    ) -> Result<(), RegistrationError>
    + 'static,
) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    run_with(args, ServiceRegistration::new().with_grpc(register))
}
// template:end grpc:service-run-with-grpc

pub fn run_with<I>(args: I, registration: ServiceRegistration) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    match service_config::LoadOptions::try_parse_from(args) {
        Ok(options) => bootstrap::run_registered(&options, registration),
        Err(error) => {
            if error.print().is_ok() {
                ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(1))
            } else {
                ExitCode::FAILURE
            }
        }
    }
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

    // template:begin grpc:service-registration-compose-test
    #[test]
    fn http_resource_and_grpc_registration_compose_on_one_registration() {
        let registration =
            ServiceRegistration::for_http_resource(|_, _| Ok(())).with_grpc(|_, _, _| Ok(()));
        assert!(registration.http_resource.is_some());
        assert!(registration.grpc.is_some());
    }
    // template:end grpc:service-registration-compose-test
}
