//! Tokio runtime worker count.

use std::num::NonZeroUsize;

use serde::Deserialize;

use crate::ValidationError;

const MAX_WORKER_THREADS: usize = 512;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct RuntimeConfig {
    /// Unset uses cgroup-aware available parallelism, falling back to one.
    /// Explicit counts must be between 1 and 512.
    /// `APP__RUNTIME__WORKER_THREADS` is the override channel;
    /// `TOKIO_WORKER_THREADS` is not read.
    #[serde(deserialize_with = "crate::de::optional_unsigned_count")]
    pub worker_threads: Option<NonZeroUsize>,
}

impl RuntimeConfig {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if let Some(count) = self.worker_threads
            && count.get() > MAX_WORKER_THREADS
        {
            return Err(ValidationError::new(
                "runtime.worker_threads",
                format!("must be at most {MAX_WORKER_THREADS}, got {count}"),
            ));
        }
        Ok(())
    }

    /// The configured worker count, or the process's available parallelism.
    #[must_use]
    pub fn effective_worker_threads(&self) -> usize {
        self.worker_threads
            .unwrap_or_else(|| std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN))
            .get()
    }
}
