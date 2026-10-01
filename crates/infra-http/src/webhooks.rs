//! The public Standard Webhooks ingress route.
//!
//! This module owns only transport admission: it bounds raw bytes, carries the
//! generated operation contract, and translates the provider's closed receive
//! outcomes into the service Problem catalog.  Signature verification and the
//! receipt transaction remain in `infra-webhooks`.

use std::fmt;
use std::time::SystemTime;

use axum::Router;
use axum::extract::{Extension, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use health::ReadinessReader;
use http_body_util::{BodyExt, Limited};
use infra_webhooks::inbound::{ReceiptOutcome, ReceiveError, Receiver};
use infra_webhooks::protocol::{MAX_BODY_BYTES, ProtocolError};
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::extract::Path;
use crate::problem::responses::WebhookProblemResponses;
use crate::problem::{Code, Problem};

/// Webhook admissions by `outcome` (`accepted`, `duplicate`, `rejected`,
/// `unavailable`, `unknown_endpoint`), configured `endpoint`, and rejection
/// `reason`. `endpoint` is empty for an unknown endpoint, so a caller never
/// chooses a label value; `reason` is empty unless the outcome is `rejected`.
pub const WEBHOOK_INGRESS_OUTCOMES_METRIC: &str = "webhook_ingress_outcomes_total";

/// The rejection reason for a body the transport could not read.
const BODY_READ_FAILED: &str = "body_read_failed";

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
        metrics::describe_counter!(
            WEBHOOK_INGRESS_OUTCOMES_METRIC,
            metrics::Unit::Count,
            "Inbound webhook admissions by outcome, endpoint, and rejection reason."
        );
        Self {
            receiver: Some(receiver),
        }
    }
}

/// The public webhook route with its annotated contract.  It shares the
/// service's readiness state; the webhook receiver is a route extension so
/// existing probe state and the hardened router stay unchanged.
#[must_use]
pub fn router() -> OpenApiRouter<ReadinessReader> {
    OpenApiRouter::with_openapi(crate::problem::responses::ProblemComponents::openapi())
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
        return unknown_endpoint();
    };
    if !receiver.has_endpoint(&endpoint_id) {
        return unknown_endpoint();
    }
    let (parts, body) = request.into_parts();
    let body = match Limited::new(body, MAX_BODY_BYTES).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) if error.is::<http_body_util::LengthLimitError>() => {
            return rejected(
                Code::RequestEntityTooLarge,
                &endpoint_id,
                ProtocolError::BodyTooLarge.as_str(),
            );
        }
        Err(_) => return rejected(Code::WebhookRejected, &endpoint_id, BODY_READ_FAILED),
    };
    match receiver
        .receive(&endpoint_id, &parts.headers, &body, SystemTime::now())
        .await
    {
        Ok(ReceiptOutcome::Accepted) => {
            record_outcome("accepted", &endpoint_id, "");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(ReceiptOutcome::Duplicate) => {
            record_outcome("duplicate", &endpoint_id, "");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(ReceiveError::UnknownEndpoint) => unknown_endpoint(),
        Err(ReceiveError::Rejected(reason)) => {
            rejected(Code::WebhookRejected, &endpoint_id, reason.as_str())
        }
        Err(ReceiveError::Unavailable) => {
            record_outcome("unavailable", &endpoint_id, "");
            Problem::new(Code::ServiceUnavailable).into_response()
        }
    }
}

/// The path names no configured endpoint, so it never becomes a label value.
fn unknown_endpoint() -> Response {
    record_outcome("unknown_endpoint", "", "");
    Problem::new(Code::NotFound).into_response()
}

fn rejected(code: Code, endpoint_id: &str, reason: &'static str) -> Response {
    record_outcome("rejected", endpoint_id, reason);
    Problem::new(code).into_response()
}

/// `endpoint_id` is empty or a configured endpoint ID, a set the operator bounds.
fn record_outcome(outcome: &'static str, endpoint_id: &str, reason: &'static str) {
    metrics::counter!(
        WEBHOOK_INGRESS_OUTCOMES_METRIC,
        "outcome" => outcome,
        "endpoint" => endpoint_id.to_owned(),
        "reason" => reason,
    )
    .increment(1);
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
        let readiness = Readiness::new(
            Vec::new(),
            RefreshPolicy {
                interval: std::time::Duration::from_secs(1),
                probe_budget: std::time::Duration::from_secs(1),
                failure_threshold: 1,
            },
        );
        readiness.refresh().await;
        let app = crate::finalize_public(router())
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

    #[tokio::test]
    async fn an_undecodable_endpoint_id_is_a_problem() {
        let readiness = Readiness::new(
            Vec::new(),
            RefreshPolicy {
                interval: std::time::Duration::from_secs(1),
                probe_budget: std::time::Duration::from_secs(1),
                failure_threshold: 1,
            },
        );
        let app = crate::finalize_public(router())
            .expect("the webhook operation is explicitly public")
            .with_state(readiness.reader())
            .layer(Extension(WebhookState::inert()));
        let response = app
            .oneshot(
                Request::post("/webhooks/%FF")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
        assert_eq!(problem["code"], "bad_request");
        assert_eq!(problem["invalid_params"][0]["name"], "path.endpoint_id");
    }

    #[tokio::test]
    async fn outcomes_name_the_configured_endpoint_and_reason_but_never_a_caller_path() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let _local = metrics::set_default_local_recorder(&recorder);
        // Both requests end before the receipt transaction, so nothing connects.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .expect("lazy pool does not connect");
        let keys = infra_webhooks::protocol::KeyRing::from_encoded(
            "whsec_Q0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0M=",
            None,
        )
        .expect("key");
        let readiness = Readiness::new(
            Vec::new(),
            RefreshPolicy {
                interval: std::time::Duration::from_secs(1),
                probe_budget: std::time::Duration::from_secs(1),
                failure_threshold: 1,
            },
        );
        let app = crate::finalize_public(router())
            .expect("the webhook operation is explicitly public")
            .with_state(readiness.reader())
            .layer(Extension(WebhookState::active(Receiver::new(
                pool,
                [("partner".to_owned(), keys)],
            ))));
        for (path, status) in [
            ("/webhooks/partner", StatusCode::BAD_REQUEST),
            ("/webhooks/caller-chosen", StatusCode::NOT_FOUND),
        ] {
            let response = app
                .clone()
                .oneshot(Request::post(path).body(Body::from("{}")).expect("request"))
                .await
                .expect("router response");
            assert_eq!(response.status(), status, "{path}");
        }

        let rendered = recorder.handle().render();
        let series = |outcome: &str| {
            rendered
                .lines()
                .find(|line| {
                    line.starts_with(WEBHOOK_INGRESS_OUTCOMES_METRIC)
                        && line.contains(&format!("outcome=\"{outcome}\""))
                })
                .unwrap_or_else(|| panic!("no {outcome} series in:\n{rendered}"))
        };
        let rejected = series("rejected");
        assert!(rejected.contains("endpoint=\"partner\""), "{rejected}");
        assert!(rejected.contains("reason=\"missing_header\""), "{rejected}");
        assert!(rejected.ends_with(" 1"), "{rejected}");
        assert!(series("unknown_endpoint").ends_with(" 1"));
        assert!(!rendered.contains("caller-chosen"), "{rendered}");
    }
}
