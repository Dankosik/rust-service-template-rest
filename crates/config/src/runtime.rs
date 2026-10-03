//! Tokio runtime worker count.

use std::num::NonZeroUsize;

use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct RuntimeConfig {
    /// Unset uses cgroup-aware available parallelism, falling back to one.
    /// `NonZeroUsize` rejects zero while decoding, so no validation step.
    /// `APP__RUNTIME__WORKER_THREADS` is the override channel;
    /// `TOKIO_WORKER_THREADS` is not read.
    pub worker_threads: Option<NonZeroUsize>,
}

impl RuntimeConfig {
    /// The configured worker count, or the process's available parallelism.
    #[must_use]
    pub fn effective_worker_threads(&self) -> usize {
        self.worker_threads
            .unwrap_or_else(|| std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN))
            .get()
    }
}
