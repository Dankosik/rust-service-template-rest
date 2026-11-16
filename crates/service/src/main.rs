#[cfg(not(feature = "hotpath-alloc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg_attr(
    feature = "hotpath",
    hotpath::main(allocator = tikv_jemallocator::Jemalloc, percentiles = [50, 95, 99])
)]
fn main() -> std::process::ExitCode {
    service::run(std::env::args_os())
}
