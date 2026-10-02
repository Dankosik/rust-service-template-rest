//! The public Standard Webhooks ingress route.
//!
//! This module owns only transport admission: it bounds raw bytes, carries the
//! generated operation contract, and translates the provider's closed receive
//! outcomes into the service Problem catalog.  Signature verification and the
//! receipt transaction remain in `infra-webhooks`.

use std::fmt;
use std::time::SystemTime;

use axum::extract::{FromRef, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use http_body_util::{BodyExt, Limited};
use infra_webhooks::inbound::{ReceiptOutcome, ReceiveError, Receiver};
use infra_webhooks::protocol::MAX_BODY_BYTES;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::extract::Path;
use crate::harden::{RequestDeadline, postgres_attempt_end};
use crate::problem::responses::WebhookProblemResponses;
use crate::problem::{Code, Problem, sanitized_internal_error};

/// Webhook ingress results. Label `outcome` is `accepted`, `duplicate`,
/// `rejected`, `unavailable`, or `unknown_endpoint`. A configured endpoint
/// adds its operator-chosen ID as `endpoint`, and a rejection adds the
/// verifier's static `reason`. A requested ID that matches no configured
/// endpoint is caller-controlled and never becomes a label.
pub const WEBHOOK_INGRESS_OUTCOMES_METRIC: &str = "webhook_ingress_outcomes_total";

/// Route state supplied by the composition root.
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

/// The public webhook route with its annotated contract. The state is any
/// type that hands out a [`WebhookState`]: the composition root keeps the
/// receiver in its application state, and a route mounted without one does
/// not compile.
#[must_use]
pub fn router<S>() -> OpenApiRouter<S>
where
    WebhookState: FromRef<S>,
    S: Clone + Send + Sync + 'static,
{
    metrics::describe_counter!(
        WEBHOOK_INGRESS_OUTCOMES_METRIC,
        metrics::Unit::Count,
        "Inbound webhook deliveries by admission outcome."
    );
    OpenApiRouter::with_openapi(crate::problem::responses::ProblemComponents::openapi())
        .routes(routes!(receive))
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
    State(state): State<WebhookState>,
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
            record_rejection(&endpoint_id, "body_too_large");
            return Problem::new(Code::RequestEntityTooLarge).into_response();
        }
        Err(_) => {
            record_rejection(&endpoint_id, "body_unreadable");
            return Problem::new(Code::WebhookRejected).into_response();
        }
    };
    let Some(deadline) = parts.extensions.get::<RequestDeadline>() else {
        tracing::error!(failure = "deadline_missing", "webhook_wiring_failed");
        return sanitized_internal_error();
    };
    let result = if let Some(attempt_end) = postgres_attempt_end(deadline.at()) {
        tokio::time::timeout_at(
            attempt_end,
            receiver.receive(&endpoint_id, &parts.headers, &body, SystemTime::now()),
        )
        .await
        .unwrap_or(Err(ReceiveError::Unavailable))
    } else {
        Err(ReceiveError::Unavailable)
    };
    match result {
        Ok(outcome) => {
            let outcome = match outcome {
                ReceiptOutcome::Accepted => "accepted",
                ReceiptOutcome::Duplicate => "duplicate",
            };
            record_outcome(&endpoint_id, outcome);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(ReceiveError::UnknownEndpoint) => unknown_endpoint(),
        Err(ReceiveError::Rejected(rejection)) => {
            record_rejection(&endpoint_id, rejection.reason());
            Problem::new(Code::WebhookRejected).into_response()
        }
        Err(ReceiveError::Unavailable) => {
            record_outcome(&endpoint_id, "unavailable");
            Problem::new(Code::ServiceUnavailable).into_response()
        }
    }
}

fn unknown_endpoint() -> Response {
    metrics::counter!(WEBHOOK_INGRESS_OUTCOMES_METRIC, "outcome" => "unknown_endpoint")
        .increment(1);
    Problem::new(Code::NotFound).into_response()
}

fn record_outcome(endpoint_id: &str, outcome: &'static str) {
    metrics::counter!(
        WEBHOOK_INGRESS_OUTCOMES_METRIC,
        "endpoint" => endpoint_id.to_owned(),
        "outcome" => outcome
    )
    .increment(1);
}

fn record_rejection(endpoint_id: &str, reason: &'static str) {
    metrics::counter!(
        WEBHOOK_INGRESS_OUTCOMES_METRIC,
        "endpoint" => endpoint_id.to_owned(),
        "outcome" => "rejected",
        "reason" => reason
    )
    .increment(1);
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::header::CONTENT_TYPE;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    /// Collects the key of every counter the route registers.
    #[derive(Default)]
    struct Keys(std::sync::Mutex<Vec<metrics::Key>>);

    impl metrics::Recorder for Keys {
        fn describe_counter(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }

        fn describe_gauge(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }

        fn describe_histogram(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }

        fn register_counter(
            &self,
            key: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Counter {
            self.0.lock().expect("keys").push(key.clone());
            metrics::Counter::noop()
        }

        fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
            metrics::Gauge::noop()
        }

        fn register_histogram(
            &self,
            _: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Histogram {
            metrics::Histogram::noop()
        }
    }

    #[test]
    fn outcomes_label_a_configured_endpoint_and_never_an_unknown_one() {
        let keys = Keys::default();
        metrics::with_local_recorder(&keys, || {
            record_outcome("partner", "accepted");
            record_rejection("partner", "invalid_signature");
            let _ = unknown_endpoint();
        });
        let labels: Vec<Vec<(String, String)>> = keys
            .0
            .lock()
            .expect("keys")
            .iter()
            .map(|key| {
                assert_eq!(key.name(), WEBHOOK_INGRESS_OUTCOMES_METRIC);
                key.labels()
                    .map(|label| (label.key().to_owned(), label.value().to_owned()))
                    .collect()
            })
            .collect();
        let pair = |key: &str, value: &str| (key.to_owned(), value.to_owned());
        assert_eq!(
            labels,
            [
                vec![pair("endpoint", "partner"), pair("outcome", "accepted")],
                vec![
                    pair("endpoint", "partner"),
                    pair("outcome", "rejected"),
                    pair("reason", "invalid_signature"),
                ],
                vec![pair("outcome", "unknown_endpoint")],
            ]
        );
    }

    #[tokio::test]
    async fn inert_receiver_returns_a_problem_before_reading_or_authenticating_the_body() {
        let app = crate::finalize_public(router())
            .expect("the webhook operation is explicitly public")
            .with_state(WebhookState::inert());
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
        let app = crate::finalize_public(router())
            .expect("the webhook operation is explicitly public")
            .with_state(WebhookState::inert());
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
}
