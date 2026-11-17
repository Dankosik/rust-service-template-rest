use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use axum::extract::{FromRef, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use enum_map::{Enum, EnumMap};
use foldhash::HashMap;
use http_body_util::{BodyExt, Limited};
use infra_http::extract::Path;
use infra_http::problem::responses::{GatewayTimeout, InternalServerError, RequestEntityTooLarge};
use infra_http::problem::{body_too_large, caused_by_length_limit, sanitized_problem};
use infra_http::{Code, Problem, RequestContext, RequestDeadline};
use metrics::{Counter, SharedString};
use parking_lot::Mutex;
use utoipa::{IntoResponses, OpenApi, ToResponse};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::{ReceiptOutcome, ReceiveError, Receiver};
use crate::protocol::{MAX_BODY_BYTES, MAX_MESSAGE_ID_BYTES};

const WEBHOOK_INGRESS_OUTCOMES_METRIC: &str = "webhook_ingress_outcomes_total";

#[derive(Clone, derive_more::Debug)]
pub struct WebhookState {
    #[debug("{}", active.active.is_some())]
    active: Arc<ReceiverState>,
}

impl WebhookState {
    #[must_use]
    pub fn inert() -> Self {
        Self {
            active: Arc::new(ReceiverState::inert()),
        }
    }

    #[must_use]
    pub fn active(receiver: Receiver) -> Self {
        Self {
            active: Arc::new(ReceiverState::active(receiver)),
        }
    }
}

struct ReceiverState {
    active: Option<ActiveReceiver>,
    unknown_endpoint: OnceLock<Counter>,
}

struct ActiveReceiver {
    receiver: Receiver,
    endpoints: HashMap<Arc<str>, EndpointMetrics>,
}

impl ReceiverState {
    fn inert() -> Self {
        Self {
            active: None,
            unknown_endpoint: OnceLock::new(),
        }
    }

    fn active(receiver: Receiver) -> Self {
        let endpoints = receiver
            .endpoint_ids()
            .map(|endpoint_id| {
                let id: Arc<str> = Arc::from(endpoint_id);
                let metrics = EndpointMetrics::new(SharedString::from(Arc::clone(&id)));
                (id, metrics)
            })
            .collect();
        Self {
            active: Some(ActiveReceiver {
                receiver,
                endpoints,
            }),
            unknown_endpoint: OnceLock::new(),
        }
    }

    fn endpoint(&self, endpoint_id: &str) -> Option<(&Receiver, &EndpointMetrics)> {
        let active = self.active.as_ref()?;
        let metrics = active.endpoints.get(endpoint_id)?;
        Some((&active.receiver, metrics))
    }

    fn unknown_endpoint(&self) -> Response {
        self.unknown_endpoint
            .get_or_init(|| {
                metrics::counter!(
                    WEBHOOK_INGRESS_OUTCOMES_METRIC,
                    "outcome" => "unknown_endpoint"
                )
            })
            .increment(1);
        Problem::new(Code::NotFound).into_response()
    }
}

/// webhook endpoint is unknown
#[derive(Debug, ToResponse)]
#[expect(
    dead_code,
    reason = "The response body exists only for the OpenAPI derive"
)]
#[response(content_type = "application/problem+json")]
struct NotFound(Problem);

/// webhook body, message identity, or signature evidence is unacceptable
#[derive(Debug, ToResponse)]
#[expect(
    dead_code,
    reason = "The response body exists only for the OpenAPI derive"
)]
#[response(content_type = "application/problem+json")]
struct WebhookRejected(Problem);

/// durable webhook receipt storage is unavailable
#[derive(Debug, ToResponse)]
#[expect(
    dead_code,
    reason = "The response body exists only for the OpenAPI derive"
)]
#[response(content_type = "application/problem+json")]
struct ServiceUnavailable(Problem);

/// The non-success webhook admission outcomes.  The route itself owns
/// the signature requirement; OpenAPI's empty security array means only
/// that bearer authentication is not the operation's authentication.
#[derive(Debug, IntoResponses)]
#[expect(dead_code, reason = "The variants exist only for the OpenAPI derive")]
enum WebhookProblemResponses {
    #[response(status = 400)]
    Rejected(#[ref_response] WebhookRejected),
    #[response(status = 404)]
    NotFound(#[ref_response] NotFound),
    #[response(status = 413)]
    RequestEntityTooLarge(#[ref_response] RequestEntityTooLarge),
    #[response(status = 500)]
    InternalServerError(#[ref_response] InternalServerError),
    #[response(status = 503)]
    Unavailable(#[ref_response] ServiceUnavailable),
    #[response(status = 504)]
    Timeout(#[ref_response] GatewayTimeout),
}

#[derive(OpenApi)]
#[openapi(components(responses(NotFound, WebhookRejected, ServiceUnavailable, GatewayTimeout)))]
struct WebhookComponents;

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
    OpenApiRouter::with_openapi(WebhookComponents::openapi()).routes(routes!(receive))
}

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
    let state = state.active.as_ref();
    let Some((receiver, metrics)) = state.endpoint(&endpoint_id) else {
        return state.unknown_endpoint();
    };
    let (parts, body) = request.into_parts();
    let (Some(deadline), Some(context)) = (
        parts.extensions.get::<RequestDeadline>(),
        parts.extensions.get::<RequestContext>(),
    ) else {
        tracing::error!(
            endpoint = %endpoint_id,
            failure = "deadline_missing",
            "webhook_wiring_failed"
        );
        return sanitized_problem().into_response();
    };
    let body = match context
        .read_body(Limited::new(body, MAX_BODY_BYTES).collect())
        .await
    {
        Ok(collected) => collected.to_bytes(),
        Err(error) if caused_by_length_limit(&*error) => {
            metrics.record(OutcomeSlot::BodyTooLarge);
            return body_too_large().into_response();
        }
        Err(_) => {
            metrics.record(OutcomeSlot::BodyUnreadable);
            return Problem::new(Code::WebhookRejected).into_response();
        }
    };
    let Some(attempt_end) = deadline.attempt_end() else {
        return receipt_deadline_exceeded(metrics);
    };
    let receipt = receiver.receive_bytes(
        &endpoint_id,
        &parts.headers,
        body,
        SystemTime::now(),
        attempt_end,
    );
    let Ok(result) = tokio::time::timeout_at(attempt_end, receipt).await else {
        return receipt_deadline_exceeded(metrics);
    };
    match result {
        Ok(outcome) => {
            let slot = match outcome {
                ReceiptOutcome::Accepted => OutcomeSlot::Accepted,
                ReceiptOutcome::Duplicate => OutcomeSlot::Duplicate,
            };
            metrics.record(slot);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(ReceiveError::UnknownEndpoint) => state.unknown_endpoint(),
        Err(ReceiveError::Rejected(rejection)) => {
            metrics.record_rejection(rejection.reason());
            Problem::new(Code::WebhookRejected).into_response()
        }
        Err(ReceiveError::Unavailable) if tokio::time::Instant::now() >= attempt_end => {
            receipt_deadline_exceeded(metrics)
        }
        Err(ReceiveError::Unavailable) => {
            metrics.record(OutcomeSlot::Unavailable);
            Problem::new(Code::ServiceUnavailable).into_response()
        }
    }
}

fn receipt_deadline_exceeded(metrics: &EndpointMetrics) -> Response {
    metrics.record(OutcomeSlot::DeadlineExceeded);
    Problem::new(Code::GatewayTimeout)
        .detail("request budget for durable webhook receipt was exhausted")
        .into_response()
}

#[derive(Clone, Copy, Debug, Enum)]
enum OutcomeSlot {
    Accepted,
    Duplicate,
    Unavailable,
    DeadlineExceeded,
    BodyTooLarge,
    BodyUnreadable,
    MissingHeader,
    ConflictingHeader,
    InvalidMessageId,
    InvalidTimestamp,
    TimestampOutOfWindow,
    InvalidSignature,
    PayloadTooLarge,
}

impl OutcomeSlot {
    fn from_rejection(reason: &'static str) -> Option<Self> {
        match reason {
            "body_too_large" => Some(Self::BodyTooLarge),
            "body_unreadable" => Some(Self::BodyUnreadable),
            "missing_header" => Some(Self::MissingHeader),
            "conflicting_header" => Some(Self::ConflictingHeader),
            "invalid_message_id" => Some(Self::InvalidMessageId),
            "invalid_timestamp" => Some(Self::InvalidTimestamp),
            "timestamp_out_of_window" => Some(Self::TimestampOutOfWindow),
            "invalid_signature" => Some(Self::InvalidSignature),
            "payload_too_large" => Some(Self::PayloadTooLarge),
            _ => None,
        }
    }

    const fn labels(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Accepted => ("accepted", None),
            Self::Duplicate => ("duplicate", None),
            Self::Unavailable => ("unavailable", None),
            Self::DeadlineExceeded => ("deadline_exceeded", None),
            Self::BodyTooLarge => ("rejected", Some("body_too_large")),
            Self::BodyUnreadable => ("rejected", Some("body_unreadable")),
            Self::MissingHeader => ("rejected", Some("missing_header")),
            Self::ConflictingHeader => ("rejected", Some("conflicting_header")),
            Self::InvalidMessageId => ("rejected", Some("invalid_message_id")),
            Self::InvalidTimestamp => ("rejected", Some("invalid_timestamp")),
            Self::TimestampOutOfWindow => ("rejected", Some("timestamp_out_of_window")),
            Self::InvalidSignature => ("rejected", Some("invalid_signature")),
            Self::PayloadTooLarge => ("rejected", Some("payload_too_large")),
        }
    }
}

struct EndpointMetrics {
    endpoint: SharedString,
    outcomes: EnumMap<OutcomeSlot, OnceLock<Counter>>,
    custom_rejections: Mutex<HashMap<&'static str, Counter>>,
}

impl EndpointMetrics {
    fn new(endpoint: SharedString) -> Self {
        Self {
            endpoint,
            outcomes: EnumMap::default(),
            custom_rejections: Mutex::new(HashMap::default()),
        }
    }

    fn record(&self, slot: OutcomeSlot) {
        self.outcomes[slot]
            .get_or_init(|| {
                let endpoint = self.endpoint.clone();
                match slot.labels() {
                    (outcome, None) => metrics::counter!(
                        WEBHOOK_INGRESS_OUTCOMES_METRIC,
                        "endpoint" => endpoint,
                        "outcome" => outcome
                    ),
                    (outcome, Some(reason)) => metrics::counter!(
                        WEBHOOK_INGRESS_OUTCOMES_METRIC,
                        "endpoint" => endpoint,
                        "outcome" => outcome,
                        "reason" => reason
                    ),
                }
            })
            .increment(1);
    }

    fn record_rejection(&self, reason: &'static str) {
        if let Some(slot) = OutcomeSlot::from_rejection(reason) {
            self.record(slot);
            return;
        }
        let counter = self
            .custom_rejections
            .lock()
            .entry(reason)
            .or_insert_with(|| {
                metrics::counter!(
                    WEBHOOK_INGRESS_OUTCOMES_METRIC,
                    "endpoint" => self.endpoint.clone(),
                    "outcome" => "rejected",
                    "reason" => reason
                )
            })
            .clone();
        counter.increment(1);
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use axum::http::header::CONTENT_TYPE;
    use metrics::Label;
    use tower::ServiceExt;

    use super::*;
    use crate::inbound::Verifier;
    use crate::protocol::KeyRing;

    async fn body_text(response: Response) -> String {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        String::from_utf8(bytes.to_vec()).expect("UTF-8 body")
    }

    #[tokio::test]
    async fn outcomes_label_a_configured_endpoint_and_never_an_unknown_one() {
        use metrics_util::debugging::DebugValue;

        let recorder = metrics_util::debugging::DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let pool = infra_postgres::PgPool::connect_lazy("postgres://127.0.0.1:1/unused")
            .expect("lazy pool opens no connection");
        let keys = KeyRing::from_encoded("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=", None)
            .expect("key");
        let state = WebhookState::active(Receiver::new(
            pool,
            [("partner".to_owned(), Arc::new(keys) as Arc<dyn Verifier>)],
        ));
        let cloned = state.clone();
        metrics::with_local_recorder(&recorder, || {
            let (_, partner) = state.active.endpoint("partner").expect("partner endpoint");
            partner.record(OutcomeSlot::Accepted);
            let (_, cloned_partner) = cloned.active.endpoint("partner").expect("partner endpoint");
            cloned_partner.record(OutcomeSlot::Accepted);
            partner.record_rejection("invalid_signature");
            partner.record_rejection("invalid_token");
            let _ = state.active.unknown_endpoint();
            let _ = cloned.active.unknown_endpoint();
        });
        let mut samples: Vec<(Vec<Label>, u64)> = snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .map(|(key, _, _, value)| {
                assert_eq!(key.key().name(), WEBHOOK_INGRESS_OUTCOMES_METRIC);
                let DebugValue::Counter(count) = value else {
                    panic!("expected counter, got {value:?}");
                };
                (key.key().labels().cloned().collect(), count)
            })
            .collect();
        samples.sort();
        assert_eq!(
            samples,
            [
                (
                    vec![
                        Label::new("endpoint", "partner"),
                        Label::new("outcome", "accepted")
                    ],
                    2
                ),
                (
                    vec![
                        Label::new("endpoint", "partner"),
                        Label::new("outcome", "rejected"),
                        Label::new("reason", "invalid_signature"),
                    ],
                    1,
                ),
                (
                    vec![
                        Label::new("endpoint", "partner"),
                        Label::new("outcome", "rejected"),
                        Label::new("reason", "invalid_token"),
                    ],
                    1,
                ),
                (vec![Label::new("outcome", "unknown_endpoint")], 2),
            ]
        );

        let second_recorder = metrics_util::debugging::DebuggingRecorder::new();
        let second_snapshotter = second_recorder.snapshotter();
        let inert = WebhookState::inert();
        metrics::with_local_recorder(&second_recorder, || {
            let _ = inert.clone().active.unknown_endpoint();
        });
        assert_eq!(second_snapshotter.snapshot().into_vec().len(), 1);
    }

    #[tokio::test]
    async fn webhook_state_debug_reports_only_activation() {
        let inert = WebhookState::inert();
        assert_eq!(format!("{inert:?}"), "WebhookState { active: false }");
        assert_eq!(
            format!("{inert:#?}"),
            "WebhookState {\n    active: false,\n}"
        );

        let pool = infra_postgres::PgPool::connect_lazy("postgres://127.0.0.1:1/unused")
            .expect("lazy pool opens no connection");
        let active = WebhookState::active(Receiver::new(pool, []));
        assert_eq!(format!("{active:?}"), "WebhookState { active: true }");
        assert_eq!(
            format!("{active:#?}"),
            "WebhookState {\n    active: true,\n}"
        );
    }

    #[tokio::test]
    async fn inert_receiver_returns_a_problem_before_reading_or_authenticating_the_body() {
        let app = infra_http::finalize_public(router())
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
        let problem = body_text(response).await;
        assert!(problem.contains(r#""code":"not_found""#), "{problem}");
    }

    #[tokio::test]
    async fn an_undecodable_endpoint_id_is_a_problem() {
        let app = infra_http::finalize_public(router())
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
        let problem = body_text(response).await;
        assert!(problem.contains(r#""code":"bad_request""#), "{problem}");
        assert!(
            problem.contains(r#""name":"path.endpoint_id""#),
            "{problem}"
        );
    }
}
