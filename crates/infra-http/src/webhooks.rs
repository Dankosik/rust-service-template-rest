//! The public Standard Webhooks ingress route.
//!
//! This module owns only transport admission: it bounds raw bytes, carries the
//! generated operation contract, and translates the provider's closed receive
//! outcomes into the service Problem catalog.  Signature verification and the
//! receipt transaction remain in `infra-webhooks`.

use std::error::Error as _;
use std::fmt;
use std::time::SystemTime;

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Extension, Path, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use health::ReadinessReader;
use infra_webhooks::inbound::{ReceiptOutcome, ReceiveError, Receiver};
use infra_webhooks::protocol::MAX_BODY_BYTES;
use utoipa::OpenApi;

use crate::ContractRouter;
use crate::problem::responses::WebhookProblemResponses;
use crate::problem::{Code, Problem};
use crate::routes;

/// Bounded result labels for webhook ingress telemetry.
pub const WEBHOOK_INGRESS_OUTCOMES_METRIC: &str = "webhook_ingress_outcomes_total";

/// Route state supplied by the composition root through an Axum extension.
///
/// The route is retained when the inbound profile is selected even if no
/// endpoint is configured.  That inert state deliberately answers unknown
/// endpoint before a body is read or signature work starts.
#[derive(Clone, Default)]
pub struct WebhookState {
    receiver: Option<Receiver>,
}

impl fmt::Debug for WebhookState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebhookState")
            .field("active", &self.receiver.is_some())
            .finish()
    }
}

impl WebhookState {
    /// An enabled inbound profile with no receiving endpoint bindings.
    #[must_use]
    pub const fn inert() -> Self {
        Self { receiver: None }
    }

    /// A receiver whose endpoint bindings have already passed startup
    /// admission in the composition root.
    #[must_use]
    pub fn active(receiver: Receiver) -> Self {
        Self {
            receiver: Some(receiver),
        }
    }
}

/// Tracked public webhook route.  It shares the service's readiness state;
/// the webhook receiver is a route extension so existing probe state and the
/// hardened router stay unchanged.
#[must_use]
pub fn router() -> ContractRouter<ReadinessReader> {
    ContractRouter::with_openapi(crate::problem::responses::ProblemComponents::openapi())
        .routes(routes!(receive))
}

/// Attach the composition-root receiver state after contract finalization.
/// This keeps the existing readiness state as the router's only `State`
/// value, while avoiding a parallel router or a transport-to-root dependency.
pub fn with_webhook_state(
    router: Router<ReadinessReader>,
    state: WebhookState,
) -> Router<ReadinessReader> {
    router.layer(Extension(state))
}

/// Verify and atomically retain one inbound Standard Webhooks delivery.
///
/// Bearer authentication is explicitly disabled for this operation.  It is
/// still authenticated: every configured endpoint must pass Standard Webhooks
/// signature and timestamp verification before a durable receipt is written.
#[utoipa::path(
    post,
    path = "/webhooks/{endpoint_id}",
    tag = "webhooks",
    operation_id = "receiveWebhook",
    summary = "Accept a signed Standard Webhooks delivery",
    description = "The required Standard Webhooks signature headers authenticate this public endpoint. A successful response acknowledges durable asynchronous ownership only.",
    params(
        ("endpoint_id" = String, Path, description = "operator-configured receiving endpoint ID"),
        ("webhook-id" = String, Header, description = "required Standard Webhooks message ID, 1–255 bytes without a dot"),
        ("webhook-timestamp" = String, Header, description = "required Standard Webhooks timestamp"),
        ("webhook-signature" = String, Header, description = "required Standard Webhooks signature candidates")
    ),
    request_body(content(("*/*")), description = "Original signed binary body, at most 128 KiB. No JSON decoding or payload schema is required."),
    security(),
    responses(
        (status = 204, description = "webhook receipt is durable"),
        WebhookProblemResponses,
    )
)]
async fn receive(
    Path(endpoint_id): Path<String>,
    Extension(state): Extension<WebhookState>,
    request: Request,
) -> Response {
    let Some(receiver) = state.receiver.as_ref() else {
        return outcome_problem(Code::NotFound, "unknown_endpoint");
    };
    if !receiver.has_endpoint(&endpoint_id) {
        return outcome_problem(Code::NotFound, "unknown_endpoint");
    }
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(error) => {
            let mut source = error.source();
            while let Some(cause) = source {
                if cause.is::<http_body_util::LengthLimitError>() {
                    return outcome_problem(Code::RequestEntityTooLarge, "rejected");
                }
                source = cause.source();
            }
            return outcome_problem(Code::WebhookRejected, "rejected");
        }
    };
    match receiver
        .receive(&endpoint_id, &parts.headers, &body, SystemTime::now())
        .await
    {
        Ok(ReceiptOutcome::Accepted) => outcome_no_content("accepted"),
        Ok(ReceiptOutcome::Duplicate) => outcome_no_content("duplicate"),
        Err(ReceiveError::UnknownEndpoint) => outcome_problem(Code::NotFound, "unknown_endpoint"),
        Err(ReceiveError::Rejected) => outcome_problem(Code::WebhookRejected, "rejected"),
        Err(ReceiveError::Unavailable) => outcome_problem(Code::ServiceUnavailable, "unavailable"),
    }
}

fn outcome_no_content(outcome: &'static str) -> Response {
    record_outcome(outcome);
    StatusCode::NO_CONTENT.into_response()
}

fn outcome_problem(code: Code, outcome: &'static str) -> Response {
    record_outcome(outcome);
    Problem::new(code).into_response()
}

fn record_outcome(outcome: &'static str) {
    metrics::counter!(WEBHOOK_INGRESS_OUTCOMES_METRIC, "outcome" => outcome).increment(1);
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::extract::Extension;
    use axum::http::header::CONTENT_TYPE;
    use axum::http::{Request, StatusCode};
    use health::{Readiness, RefreshPolicy};
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn inert_receiver_returns_a_problem_before_reading_or_authenticating_the_body() {
        let readiness = Readiness::new(Vec::new());
        readiness
            .refresh(RefreshPolicy {
                interval: std::time::Duration::from_secs(1),
                probe_budget: std::time::Duration::from_secs(1),
                failure_threshold: 1,
            })
            .await;
        let app = router()
            .finalize_public()
            .expect("the webhook operation is explicitly public")
            .with_state(readiness.reader())
            .layer(Extension(WebhookState::inert()));
        let response = app
            .oneshot(
                Request::post("/webhooks/unknown")
                    .body(Body::from(vec![b'x'; MAX_BODY_BYTES + 1]))
                    .expect("request"),
            )
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/problem+json")
        );
        let body = response
            .into_body()
            .collect()
            .await
            .expect("complete problem body")
            .to_bytes();
        let problem: Value = serde_json::from_slice(&body).expect("problem JSON");
        assert_eq!(problem["code"], "not_found");
    }
}
