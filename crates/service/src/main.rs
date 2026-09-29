//! Thin process entrypoint. All composition lives in the service library.

// jemalloc: about 12% less CPU per request than glibc malloc under mixed load
// for about 5 MB more resident memory; see docs/backend-library-selection.md.
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> std::process::ExitCode {
    service::run(std::env::args_os())
}
