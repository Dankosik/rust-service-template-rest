//! Liveness and readiness probe handlers.
//!
//! Liveness is process-only: if the handler runs, the process is alive.
//! Readiness is the cached verdict owned by the `health` crate; the handler
//! never touches a dependency and exposes no dependency detail.
//!
//! The `utoipa::path` attributes are the contract of these operations; the
//! generated document is committed as `api/openapi/service.yaml` and the
//! service crate's tests refuse a stale copy. Every operation declares its
//! `x-security-decision` and its OpenAPI `security`; an empty `security()`
//! renders `security: []`, the explicit public override the linter and the
//! contract tests require.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use health::ReadinessReader;
use utoipa::IntoResponses;

use crate::problem::{BadRequest, InternalServerError, RequestEntityTooLarge};

#[utoipa::path(
    get,
    path = "/health/live",
    tag = "system",
    operation_id = "healthLive",
    summary = "Liveness probe",
    security(),
    extensions(("x-security-decision" = json!({
        "exposure": "public",
        "rationale": "process-only platform liveness endpoint with no dependency details"
    }))),
    responses(
        (status = 200, description = "ok", content_type = "text/plain", body = String, example = json!("ok")),
        (status = 400, response = BadRequest),
        (status = 413, response = RequestEntityTooLarge),
        (status = 500, response = InternalServerError),
    )
)]
pub(crate) async fn live() -> (StatusCode, &'static str) {
    (StatusCode::OK, "ok")
}

/// The readiness verdict as the contract states it: one variant per status
/// the handler can answer, so the document and the handler cannot disagree
/// about which statuses exist. `IntoResponses` documents; `IntoResponse`
/// below is the runtime rendering.
#[derive(Debug, IntoResponses)]
pub(crate) enum HealthReadyResponse {
    /// ready
    #[response(status = 200, content_type = "text/plain", example = json!("ok"))]
    Ready(&'static str),
    /// not ready
    #[response(status = 503, content_type = "text/plain", example = json!("not ready"))]
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
    path = "/health/ready",
    tag = "system",
    operation_id = "healthReady",
    summary = "Readiness probe",
    security(),
    extensions(("x-security-decision" = json!({
        "exposure": "public",
        "rationale": "platform readiness endpoint exposing only generic ready state"
    }))),
    responses(
        HealthReadyResponse,
        (status = 400, response = BadRequest),
        (status = 413, response = RequestEntityTooLarge),
        (status = 500, response = InternalServerError),
    )
)]
pub(crate) async fn ready(State(readiness): State<ReadinessReader>) -> HealthReadyResponse {
    match readiness.verdict() {
        Ok(()) => HealthReadyResponse::Ready("ok"),
        Err(_) => HealthReadyResponse::NotReady("not ready"),
    }
}
