use crate::{BuildInfo, Config, Error, LoadOptions, ShutdownTail};

pub fn service(
    environment: &[(&str, &str)],
    build: BuildInfo,
    tail: ShutdownTail,
) -> Result<Config, Error> {
    crate::load::load_with_tail_from(
        &LoadOptions::default(),
        build,
        Some(tail),
        environment
            .iter()
            .map(|&(key, value)| (key.into(), value.into())),
    )
}

// template:begin worker:config-worker-fixture
pub fn worker(
    environment: &[(&str, &str)],
    build: BuildInfo,
    tail: ShutdownTail,
) -> Result<crate::WorkerConfig, Error> {
    crate::load::load_worker_with_tail_from(
        &LoadOptions::default(),
        build,
        Some(tail),
        environment
            .iter()
            .map(|&(key, value)| (key.into(), value.into())),
    )
}
// template:end worker:config-worker-fixture
