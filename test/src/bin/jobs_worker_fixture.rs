use std::process::ExitCode;

fn main() -> ExitCode {
    jobs_worker::run(std::env::args_os(), integration_tests::jobs::register)
}
