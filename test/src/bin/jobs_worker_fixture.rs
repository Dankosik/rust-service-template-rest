//! The test-only jobs worker: the shipped entry point with the `Probe` kind
//! registered. Built only with the `integration` feature and never shipped.

use std::process::ExitCode;

fn main() -> ExitCode {
    jobs_worker::run(std::env::args_os(), Some(integration_tests::jobs::REGISTER))
}
