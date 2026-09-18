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
//! [`TransportProblemResponses`]: the `400`, `413`, and `500` problems the
//! transport can answer with on any route (`413` and `500` come from the
//! hardened chain, not from these handlers).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use health::ReadinessReader;
use utoipa::IntoResponses;

use crate::problem::responses::TransportProblemResponses;

/// Route templates of the probes: the served path, the documented path, and
/// the access log's skip list read these constants.
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
pub(crate) async fn live() -> (StatusCode, &'static str) {
    (StatusCode::OK, LIVE_BODY)
}

/// The readiness verdict as the contract states it: one variant per answer
/// the handler itself produces, so the document and the handler cannot
/// disagree about those statuses (the shared problem responses belong to the
/// transport). `IntoResponses` documents; `IntoResponse` below is the
/// runtime rendering. The payload carries the body's schema; its value is
/// always the matching constant.
#[derive(Debug, IntoResponses)]
pub(crate) enum HealthReadyResponse {
    /// ready
    #[response(status = 200, content_type = "text/plain", example = json!(READY_BODY))]
    Ready(&'static str),
    /// not ready
    #[response(status = 503, content_type = "text/plain", example = json!(NOT_READY_BODY))]
    NotReady(&'static str),
}

impl IntoResponse for HealthReadyResponse {
    fn into_response(self) -> Response {
        match self {
            Self::Ready(body) => (StatusCode::OK, body).into_response(),
            Self::NotReady(body) => (StatusCode::SERVICE_UNAVAILABLE, body).into_response(),
        }
    }
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
    responses(HealthReadyResponse, TransportProblemResponses)
)]
pub(crate) async fn ready(State(readiness): State<ReadinessReader>) -> HealthReadyResponse {
    match readiness.verdict() {
        Ok(()) => HealthReadyResponse::Ready(READY_BODY),
        Err(_) => HealthReadyResponse::NotReady(NOT_READY_BODY),
    }
}
