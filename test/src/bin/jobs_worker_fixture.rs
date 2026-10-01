//! The test-only jobs worker: the shipped entry point with the `Probe` kind
//! registered. Built only with the `integration` feature and never shipped.

use std::process::ExitCode;

fn main() -> ExitCode {
    jobs_worker::run(std::env::args_os(), integration_tests::jobs::register)
}
