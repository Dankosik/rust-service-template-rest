use std::num::NonZeroUsize;

use serde::Deserialize;

use crate::validate::{Violations, int_range};

const MAX_WORKER_THREADS: usize = 512;

#[serde_with::serde_as]
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct RuntimeConfig {
    /// Unset uses cgroup-aware available parallelism, falling back to one.
    /// A fractional CPU quota rounds down there (1.5 CPUs give one worker),
    /// so under such a quota set the count to the quota rounded up.
    /// Explicit counts must be between 1 and 512.
    /// `APP__RUNTIME__WORKER_THREADS` is the override channel;
    /// `TOKIO_WORKER_THREADS` is not read.
    #[serde_as(as = "Option<crate::de::StrictCount>")]
    pub worker_threads: Option<NonZeroUsize>,
}

impl RuntimeConfig {
    pub(crate) fn validate(&self, v: &mut Violations) {
        if let Some(count) = self.worker_threads {
            v.check(int_range(
                "runtime.worker_threads",
                count.get(),
                1..=MAX_WORKER_THREADS,
            ));
        }
    }

    #[must_use]
    pub fn effective_worker_threads(&self) -> usize {
        self.worker_threads
            .unwrap_or_else(|| std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN))
            .get()
    }
}
