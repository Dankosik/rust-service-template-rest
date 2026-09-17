//! Application router.
//!
//! One route tree. New operations join it through the API contract and
//! feature-owned handlers; shared request policy lives in [`crate::harden`],
//! never here.

use axum::Router;
use axum::routing::get;
use health::ReadinessReader;

/// Route templates served without an access-log line unless enabled.
pub(crate) const HEALTH_PROBE_ROUTES: &[&str] = &["/health/live", "/health/ready"];

/// The application routes over the shared readiness reader, without the
/// hardening chain. Pass the result to [`crate::harden`].
pub fn router(readiness: ReadinessReader) -> Router {
    Router::new()
        .route(HEALTH_PROBE_ROUTES[0], get(crate::health::live))
        .route(HEALTH_PROBE_ROUTES[1], get(crate::health::ready))
        .with_state(readiness)
}
