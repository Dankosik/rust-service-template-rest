use std::process::ExitCode;

#[path = "support/echo.rs"]
mod echo;

fn main() -> ExitCode {
    service::run_with_grpc(std::env::args_os(), |services, _, _| {
        echo::register(services, |_, _| Ok(())).map_err(Into::into)
    })
}
