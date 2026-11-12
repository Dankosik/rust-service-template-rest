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
use infra_webhooks::protocol::{MAX_BODY_BYTES, MAX_MESSAGE_ID_BYTES};
use utoipa::{IntoResponses, OpenApi, ToResponse};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::context::RequestDeadline;
use crate::extract::Path;
use crate::problem::responses::{InternalServerError, ProblemComponents, RequestEntityTooLarge};
use crate::problem::{Code, Problem, body_too_large, sanitized_internal_error};

/// Webhook ingress results. Label `outcome` is `accepted`, `duplicate`,
/// `rejected`, `unavailable`, or `unknown_endpoint`. A configured endpoint
/// adds its operator-chosen ID as `endpoint`, and a rejection adds a static
/// `reason`: the receiver's, or `body_too_large` / `body_unreadable` when the
/// body could not be read within its limit. A requested ID that matches no configured
/// endpoint is caller-controlled and never becomes a label.
pub(crate) const WEBHOOK_INGRESS_OUTCOMES_METRIC: &str = "webhook_ingress_outcomes_total";

/// Route state supplied by the composition root.
///
/// The route is retained when the inbound profile is selected even if no
/// endpoint is configured.  That inert state deliberately answers unknown
/// endpoint before a body is read or signature work starts.
#[derive(Clone)]
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

/// webhook endpoint is unknown
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct NotFound(pub Problem);

/// webhook body, message identity, or signature evidence is unacceptable
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct WebhookRejected(pub Problem);

/// durable webhook receipt storage is unavailable
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct ServiceUnavailable(pub Problem);

/// The non-success webhook admission outcomes.  The route itself owns
/// the signature requirement; OpenAPI's empty security array means only
/// that bearer authentication is not the operation's authentication.
#[derive(Debug, IntoResponses)]
pub enum WebhookProblemResponses {
    /// 400: the webhook was rejected.
    #[response(status = 400)]
    Rejected(#[ref_response] WebhookRejected),
    /// 404: no such webhook endpoint.
    #[response(status = 404)]
    NotFound(#[ref_response] NotFound),
    /// 413: the body exceeds the size limit.
    #[response(status = 413)]
    RequestEntityTooLarge(#[ref_response] RequestEntityTooLarge),
    /// 500: unexpected internal failure.
    #[response(status = 500)]
    InternalServerError(#[ref_response] InternalServerError),
    /// 503: durable receipt storage is unavailable.
    #[response(status = 503)]
    Unavailable(#[ref_response] ServiceUnavailable),
}

/// The webhook response family's components, registered by [`router`].
#[derive(OpenApi)]
#[openapi(components(responses(NotFound, WebhookRejected, ServiceUnavailable)))]
struct WebhookComponents;

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
    let mut document = ProblemComponents::openapi();
    document.merge(WebhookComponents::openapi());
    OpenApiRouter::with_openapi(document).routes(routes!(receive))
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
        ("webhook-id" = String, Header, description = format!("required Standard Webhooks message ID, 1–{MAX_MESSAGE_ID_BYTES} bytes without a dot")),
        ("webhook-timestamp" = String, Header, description = "required Standard Webhooks timestamp"),
        ("webhook-signature" = String, Header, description = "required Standard Webhooks signature candidates")
    ),
    request_body(content(("*/*")), description = format!("Original signed binary body, at most {} KiB. No JSON decoding or payload schema is required.", MAX_BODY_BYTES / 1024)),
    security(),
    responses(
        (status = 204, description = "webhook receipt is durable"),
        WebhookProblemResponses,
    )
)]
#[cfg_attr(feature = "hotpath", hotpath::measure)]
async fn receive(
    Path(endpoint_id): Path<String>,
    State(state): State<WebhookState>,
    request: Request,
) -> Response {
    let Some(receiver) = state
        .receiver
        .as_ref()
        .filter(|receiver| receiver.has_endpoint(&endpoint_id))
    else {
        return unknown_endpoint();
    };
    let (parts, body) = request.into_parts();
    let Some(deadline) = parts.extensions.get::<RequestDeadline>() else {
        tracing::error!(
            endpoint = %endpoint_id,
            failure = "deadline_missing",
            "webhook_wiring_failed"
        );
        return sanitized_internal_error();
    };
    let body = match Limited::new(body, MAX_BODY_BYTES).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) if caused_by_length_limit(&*error) => {
            EndpointOutcome::Rejected("body_too_large").record(&endpoint_id);
            return body_too_large().into_response();
        }
        Err(_) => {
            EndpointOutcome::Rejected("body_unreadable").record(&endpoint_id);
            return Problem::new(Code::WebhookRejected).into_response();
        }
    };
    let result = match deadline.attempt_end() {
        None => Err(ReceiveError::Unavailable),
        Some(attempt_end) => {
            let receipt = receiver.receive_bytes(
                &endpoint_id,
                &parts.headers,
                body,
                SystemTime::now(),
                attempt_end,
            );
            tokio::time::timeout_at(attempt_end, receipt)
                .await
                .unwrap_or(Err(ReceiveError::Unavailable))
        }
    };
    match result {
        Ok(outcome) => {
            let outcome = match outcome {
                ReceiptOutcome::Accepted => EndpointOutcome::Accepted,
                ReceiptOutcome::Duplicate => EndpointOutcome::Duplicate,
            };
            outcome.record(&endpoint_id);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(ReceiveError::UnknownEndpoint) => unknown_endpoint(),
        Err(ReceiveError::Rejected(rejection)) => {
            EndpointOutcome::Rejected(rejection.reason()).record(&endpoint_id);
            Problem::new(Code::WebhookRejected).into_response()
        }
        Err(ReceiveError::Unavailable) => {
            EndpointOutcome::Unavailable.record(&endpoint_id);
            Problem::new(Code::ServiceUnavailable).into_response()
        }
    }
}

fn unknown_endpoint() -> Response {
    metrics::counter!(WEBHOOK_INGRESS_OUTCOMES_METRIC, "outcome" => "unknown_endpoint")
        .increment(1);
    Problem::new(Code::NotFound).into_response()
}

/// The outcome of a delivery to a configured endpoint.
enum EndpointOutcome {
    Accepted,
    Duplicate,
    /// Carries the static `reason` label.
    Rejected(&'static str),
    Unavailable,
}

impl EndpointOutcome {
    const fn label(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Duplicate => "duplicate",
            Self::Rejected(_) => "rejected",
            Self::Unavailable => "unavailable",
        }
    }

    fn record(self, endpoint_id: &str) {
        let endpoint = endpoint_id.to_owned();
        let outcome = self.label();
        let counter = if let Self::Rejected(reason) = self {
            metrics::counter!(
                WEBHOOK_INGRESS_OUTCOMES_METRIC,
                "endpoint" => endpoint,
                "outcome" => outcome,
                "reason" => reason
            )
        } else {
            metrics::counter!(
                WEBHOOK_INGRESS_OUTCOMES_METRIC,
                "endpoint" => endpoint,
                "outcome" => outcome
            )
        };
        counter.increment(1);
    }
}

/// Whether a body read failed at a length limit. axum boxes the transport
/// limit's error into [`axum::Error`] when a request has no `Content-Length`,
/// so the limit can sit anywhere in the source chain. The inbound-webhooks
/// and http-idempotency profiles each keep a copy: a shared one in
/// `problem.rs` is dead code in a projection that drops both.
fn caused_by_length_limit(error: &(dyn std::error::Error + 'static)) -> bool {
    shared_rules::error_chain::causes(error)
        .any(<dyn std::error::Error>::is::<http_body_util::LengthLimitError>)
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
            EndpointOutcome::Accepted.record("partner");
            EndpointOutcome::Rejected("invalid_signature").record("partner");
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
