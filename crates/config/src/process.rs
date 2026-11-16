use std::io::{self, Write};
use std::process::ExitCode;

#[expect(
    clippy::disallowed_methods,
    reason = "finite process failure output before tracing or after runtime shutdown"
)]
pub fn process_failure(message: impl std::fmt::Display) -> ExitCode {
    let _ = writeln!(io::stderr().lock(), "{message}");
    ExitCode::FAILURE
}
