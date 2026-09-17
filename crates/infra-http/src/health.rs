//! Liveness and readiness probes.
//!
//! Liveness is process-only: if the handler runs, the process is alive.
//! Readiness is a shared flag owned by the composition root, which flips it
//! on once startup admission completes and off as the first step of drain.
//! Both probes answer `text/plain` and expose no dependency detail.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::State;
use axum::http::StatusCode;

/// Shared readiness flag. Cloning shares the same underlying state.
#[derive(Clone, Debug, Default)]
pub struct Readiness(Arc<AtomicBool>);

impl Readiness {
    /// A new flag that starts as not ready.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish the current readiness state to every clone.
    pub fn set_ready(&self, ready: bool) {
        self.0.store(ready, Ordering::Release);
    }

    /// Whether the service currently accepts traffic.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub(crate) async fn live() -> (StatusCode, &'static str) {
    (StatusCode::OK, "ok")
}

pub(crate) async fn ready(State(readiness): State<Readiness>) -> (StatusCode, &'static str) {
    if readiness.is_ready() {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_starts_not_ready() {
        let readiness = Readiness::new();
        assert!(!readiness.is_ready());
    }

    #[test]
    fn readiness_is_shared_between_clones() {
        let readiness = Readiness::new();
        let observer = readiness.clone();
        readiness.set_ready(true);
        assert!(observer.is_ready());
        readiness.set_ready(false);
        assert!(!observer.is_ready());
    }
}
