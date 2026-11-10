//! Liveness and readiness probe handlers.
//!
//! Liveness is process-only: if the handler runs, the process is alive.
//! Readiness is the cached verdict owned by the `health` crate; the handler
//! never touches a dependency and exposes no dependency detail.
//!
//! Probe bodies stay `text/plain` (`ok` / `not ready`) on purpose: kube and
//! other platform probes match that contract. They do not use the Problem
//! envelope the rest of the hardened router answers with.
//!
//! The `utoipa::path` attributes are the contract of these operations; the
//! generated document is committed as `api/openapi/service.yaml` and the
//! service crate's tests refuse a stale copy. Every operation declares its
//! `x-security-decision` and its OpenAPI `security`; an empty `security()`
//! renders `security: []`, the explicit public override the linter and the
//! contract tests require. Beside its own answers, every operation declares
//! [`TransportProblemResponses`]: the OpenAPI group for shared transport
//! Problems (`400`, `413`, `500`), which come from the chain, not from these
//! handlers; that type records what stays out of the group.

use axum::extract::State;
use axum::http::StatusCode;
use health::ReadinessReader;

use crate::problem::responses::TransportProblemResponses;

/// Route templates of the probes, served and documented from these constants;
/// see [`crate::router::HEALTH_PROBE_ROUTES`] for their other effects.
pub(crate) const LIVE_PATH: &str = "/health/live";
pub(crate) const READY_PATH: &str = "/health/ready";

/// Probe bodies: what the handler sends and what the contract shows as the
/// example, from one constant each.
const LIVE_BODY: &str = "ok";
const READY_BODY: &str = "ok";
const NOT_READY_BODY: &str = "not ready";

#[utoipa::path(
    get,
    path = LIVE_PATH,
    tag = "system",
    operation_id = "healthLive",
    summary = "Liveness probe",
    security(),
    extensions(("x-security-decision" = json!({
        "exposure": "public",
        "rationale": "process-only platform liveness endpoint with no dependency details"
    }))),
    responses(
        (status = 200, description = "ok", content_type = "text/plain", body = String, example = json!(LIVE_BODY)),
        TransportProblemResponses,
    )
)]
#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub(crate) async fn live() -> (StatusCode, &'static str) {
    (StatusCode::OK, LIVE_BODY)
}

#[utoipa::path(
    get,
    path = READY_PATH,
    tag = "system",
    operation_id = "healthReady",
    summary = "Readiness probe",
    security(),
    extensions(("x-security-decision" = json!({
        "exposure": "public",
        "rationale": "platform readiness endpoint exposing only generic ready state"
    }))),
    responses(
        (status = 200, description = "ready", content_type = "text/plain", body = String, example = json!(READY_BODY)),
        (status = 503, description = "not ready", content_type = "text/plain", body = String, example = json!(NOT_READY_BODY)),
        TransportProblemResponses,
    )
)]
#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub(crate) async fn ready(State(readiness): State<ReadinessReader>) -> (StatusCode, &'static str) {
    match readiness.verdict() {
        Ok(()) => (StatusCode::OK, READY_BODY),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, NOT_READY_BODY),
    }
}
