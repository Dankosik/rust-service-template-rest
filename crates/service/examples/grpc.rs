use std::process::ExitCode;

use infra_grpc::Services;

#[path = "support/echo.rs"]
mod echo;

fn register(
    services: &mut Services,
    _state: &service::AppState,
    _background: &mut service::BackgroundRegistration<'_>,
) -> Result<(), infra_grpc::Error> {
    echo::register(services)?;
    // template:begin service-auth:grpc-example-caller-rules
    for method in ["Unary", "ClientStream", "ServerStream", "BidiStream"] {
        services.allow_callers(
            &format!("/example.v1.EchoService/{method}"),
            &["example-caller"],
        )?;
    }
    // template:end service-auth:grpc-example-caller-rules
    Ok(())
}

pub(crate) fn main() -> ExitCode {
    service::run_with_grpc(std::env::args_os(), register)
}
