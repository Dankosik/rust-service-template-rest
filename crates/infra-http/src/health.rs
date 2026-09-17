//! Liveness and readiness probe handlers.
//!
//! Liveness is process-only: if the handler runs, the process is alive.
//! Readiness is the cached verdict owned by the `health` crate; the handler
//! never touches a dependency and exposes no dependency detail.

use axum::extract::State;
use axum::http::StatusCode;
use health::ReadinessReader;

pub(crate) async fn live() -> (StatusCode, &'static str) {
    (StatusCode::OK, "ok")
}

pub(crate) async fn ready(State(readiness): State<ReadinessReader>) -> (StatusCode, &'static str) {
    match readiness.verdict() {
        Ok(()) => (StatusCode::OK, "ok"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "not ready"),
    }
}
