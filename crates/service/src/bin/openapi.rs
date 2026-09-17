//! Print the service's OpenAPI document. `make openapi-generate` redirects
//! it into `api/openapi/service.yaml`.

use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let rendered = match service::api::render() {
        Ok(rendered) => rendered,
        Err(err) => return failure(&format!("render OpenAPI document: {err}")),
    };
    match std::io::stdout().write_all(rendered.as_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => failure(&format!("write OpenAPI document: {err}")),
    }
}

fn failure(message: &str) -> ExitCode {
    #[allow(clippy::print_stderr)] // The process's only failure channel.
    {
        eprintln!("{message}");
    }
    ExitCode::FAILURE
}
