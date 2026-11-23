use std::process::ExitCode;

use infra_grpc::Services;

#[path = "support/echo.rs"]
mod echo;

fn register(
    services: &mut Services,
    _state: &service::AppState,
    _background: &mut service::BackgroundRegistration<'_>,
) -> Result<(), service::RegistrationError> {
    echo::register(services, |echo, method| {
        // template:begin service-auth:grpc-example-caller-rules
        echo.allow_callers(method, &["example-caller"])?;
        // template:end service-auth:grpc-example-caller-rules
        let _ = (echo, method);
        Ok(())
    })
    .map_err(Into::into)
}

pub(crate) fn main() -> ExitCode {
    service::run_with_grpc(std::env::args_os(), register)
}
