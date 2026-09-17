//! Thin process entrypoint. All composition lives in `bootstrap`.

use std::process::ExitCode;

mod bootstrap;

fn main() -> ExitCode {
    bootstrap::run(std::env::args_os().skip(1))
}
