//! Process failure output for every binary, including those that load no
//! configuration.

use std::process::ExitCode;

/// Print `message` to stderr and return [`ExitCode::FAILURE`].
///
/// Does not call `process::exit`, so destructors still run. Use this when
/// tracing is absent or not yet installed.
pub fn process_failure(message: impl std::fmt::Display) -> ExitCode {
    #[expect(
        clippy::print_stderr,
        reason = "finite process failure output before tracing or after runtime shutdown"
    )]
    {
        eprintln!("{message}");
    }
    ExitCode::FAILURE
}
