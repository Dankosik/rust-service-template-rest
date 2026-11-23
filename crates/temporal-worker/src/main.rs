use std::process::ExitCode;

#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[allow(
    clippy::unnecessary_wraps,
    reason = "derived services register Temporal workflows and activities here"
)]
fn register(
    _registration: &mut temporal_worker::Registration<'_>,
) -> Result<(), temporal_worker::BuildError> {
    Ok(())
}

fn main() -> ExitCode {
    temporal_worker::run(std::env::args_os(), register)
}
