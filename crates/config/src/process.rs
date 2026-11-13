//! Process failure output for every binary, including those that load no
//! configuration.

use std::io::{self, Write};
use std::process::ExitCode;

/// Attempt to write `message` to stderr and return [`ExitCode::FAILURE`]
/// even if the write fails.
///
/// Use this before tracing is installed or after it has shut down.
#[expect(
    clippy::disallowed_methods,
    reason = "finite process failure output before tracing or after runtime shutdown"
)]
pub fn process_failure(message: impl std::fmt::Display) -> ExitCode {
    let _ = writeln!(io::stderr().lock(), "{message}");
    ExitCode::FAILURE
}
