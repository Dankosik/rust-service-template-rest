//! Print the service's OpenAPI document. `make openapi-generate` redirects
//! it into `api/openapi/service.yaml`.

use std::io::Write;
use std::process::ExitCode;

use service_config::process_failure;

fn main() -> ExitCode {
    let rendered = match service::api::render() {
        Ok(rendered) => rendered,
        Err(err) => return process_failure(&format!("render OpenAPI document: {err}")),
    };
    match std::io::stdout().write_all(rendered.as_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => process_failure(&format!("write OpenAPI document: {err}")),
    }
}
