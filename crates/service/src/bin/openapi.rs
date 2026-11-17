use std::io::Write;
use std::process::ExitCode;

#[expect(
    clippy::disallowed_methods,
    reason = "standalone document generator owns finite stdout output without a Tokio runtime"
)]
fn main() -> ExitCode {
    let output = service::api::render()
        .map_err(|err| format!("render OpenAPI document: {err}"))
        .and_then(|rendered| {
            std::io::stdout()
                .write_all(rendered.as_bytes())
                .map_err(|err| format!("write OpenAPI document: {err}"))
        });
    match output {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            let _ = writeln!(std::io::stderr().lock(), "{message}");
            ExitCode::FAILURE
        }
    }
}
