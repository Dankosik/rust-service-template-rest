use std::process::ExitCode;

use infra_grpc::Services;

#[path = "support/echo.rs"]
mod echo;

fn register(
    services: &mut Services,
    _state: &service::AppState,
    _background: &mut service::BackgroundRegistration<'_>,
) -> Result<(), service::RegistrationError> {
    echo::register(services, |services, path| {
        // template:begin service-auth:grpc-example-caller-rules
        services.allow_callers(path, &["example-caller"])?;
        // template:end service-auth:grpc-example-caller-rules
        let _ = (services, path);
        Ok(())
    })
    .map_err(Into::into)
}

pub(crate) fn main() -> ExitCode {
    service::run_with_grpc(std::env::args_os(), register)
}
