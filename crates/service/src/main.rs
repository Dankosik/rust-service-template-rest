//! Thin process entrypoint. All composition lives in the service library.

fn main() -> std::process::ExitCode {
    service::run(std::env::args_os())
}
