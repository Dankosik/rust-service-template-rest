//! Thin process entrypoint. All composition lives in `bootstrap`.

use std::process::ExitCode;

mod bootstrap;

fn main() -> ExitCode {
    match bootstrap::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // The subscriber may not be installed yet when startup fails, so
            // report on stderr directly; `ExitCode` still runs destructors.
            #[allow(clippy::print_stderr)]
            {
                eprintln!("{err}");
            }
            ExitCode::FAILURE
        }
    }
}
