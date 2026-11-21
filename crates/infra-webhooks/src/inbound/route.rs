use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use arc_swap::ArcSwap;
use axum::extract::{FromRef, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use compact_str::CompactString;
use enum_map::{Enum, EnumMap};
use foldhash::HashMap;
use infra_http::extract::Path;
use infra_http::problem::{body_too_large, caused_by_length_limit, sanitized_problem};
use infra_http::{Code, Problem, RequestContext};
use infra_postgres::Pool;
use metrics::{Counter, SharedString};
use tokio::time::Instant;
use utoipa::{IntoResponses, OpenApi, ToResponse};

use super::{
    EndpointReceiveError, PAYLOAD_TOO_LARGE, ReceiptOutcome, ReceiveBody, Receiver, Rejection,
    RejectionKind, Verifier, receive_for_endpoint,
};
use crate::protocol::{MAX_BODY_BYTES, MAX_MESSAGE_ID_BYTES, ProtocolError};

pub const WEBHOOK_INGRESS_OUTCOMES_METRIC: &str = "webhook_ingress_outcomes_total";

#[derive(Clone)]
pub struct WebhookState {
    state: Arc<ReceiverState>,
}

impl std::fmt::Debug for WebhookState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookState")
            .field("active", &self.state.active.is_some())
            .finish()
    }
}

impl WebhookState {
    #[must_use]
    pub fn inert() -> Self {
        Self {
            state: Arc::new(ReceiverState::inert()),
        }
    }

    #[must_use]
    pub fn active(receiver: Receiver) -> Self {
        Self {
            state: Arc::new(ReceiverState::active(receiver)),
        }
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub async fn receive(&self, endpoint_id: &str, request: Request) -> Response {
        let state = self.state.as_ref();
        let Some((pool, ConfiguredEndpoint { verifier, metrics })) = state.endpoint(endpoint_id)
        else {
            return state.reject_unknown_endpoint();
        };
        let (parts, body) = request.into_parts();
        let Some((context, deadline)) = parts
            .extensions
            .get::<RequestContext>()
            .and_then(|context| Some((context, context.deadline()?)))
        else {
            tracing::error!(
                endpoint = endpoint_id,
                failure = "deadline_missing",
                "webhook_wiring_failed"
            );
            return sanitized_problem().into_response();
        };
        let body = match context.read_body_bytes(body, MAX_BODY_BYTES).await {
            Ok(bytes) => bytes,
            Err(error) if caused_by_length_limit(&error) => {
                metrics.record_protocol_rejection(ProtocolError::BodyTooLarge);
                return body_too_large().into_response();
            }
            Err(_) => {
                metrics.record_body_unreadable();
                return Problem::new(Code::WebhookRejected).into_response();
            }
        };
        let Some(attempt_end) = deadline.attempt_end() else {
            return receipt_deadline_exceeded(metrics);
        };
        let receipt = receive_for_endpoint(
            pool,
            endpoint_id,
            verifier.as_ref(),
            &parts.headers,
            ReceiveBody::Owned(body),
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
            Err(EndpointReceiveError::Rejected(rejection)) => {
                metrics.record_rejection(rejection);
                Problem::new(Code::WebhookRejected).into_response()
            }
            Err(EndpointReceiveError::Unavailable) if Instant::now() >= attempt_end => {
                receipt_deadline_exceeded(metrics)
            }
            Err(EndpointReceiveError::Unavailable) => {
                metrics.record(OutcomeSlot::Unavailable);
                Problem::new(Code::ServiceUnavailable).into_response()
            }
        }
    }
}

struct ReceiverState {
    active: Option<ActiveReceiver>,
    unknown_endpoint: OnceLock<Counter>,
}

struct ActiveReceiver {
    pool: Pool,
    endpoints: HashMap<Arc<str>, ConfiguredEndpoint>,
}

struct ConfiguredEndpoint {
    verifier: Arc<dyn Verifier>,
    metrics: EndpointMetrics,
}

impl ReceiverState {
    fn inert() -> Self {
        Self {
            active: None,
            unknown_endpoint: OnceLock::new(),
        }
    }

    fn active(receiver: Receiver) -> Self {
        let Receiver { pool, endpoints } = receiver;
        let endpoints = endpoints
            .iter()
            .map(|(endpoint_id, verifier)| {
                let id = Arc::clone(endpoint_id);
                let metrics = EndpointMetrics::new(SharedString::from(Arc::clone(&id)));
                let endpoint = ConfiguredEndpoint {
                    verifier: Arc::clone(verifier),
                    metrics,
                };
                (id, endpoint)
            })
            .collect();
        Self {
            active: Some(ActiveReceiver { pool, endpoints }),
            unknown_endpoint: OnceLock::new(),
        }
    }

    fn endpoint(&self, endpoint_id: &str) -> Option<(&Pool, &ConfiguredEndpoint)> {
        let active = self.active.as_ref()?;
        let endpoint = active.endpoints.get(endpoint_id)?;
        Some((&active.pool, endpoint))
    }

    fn reject_unknown_endpoint(&self) -> Response {
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

#[derive(Debug, IntoResponses)]
pub enum ReceiveResponses {
    /// webhook receipt is durable
    #[response(status = 204)]
    Durable,
    #[response(status = 400)]
    Rejected(#[ref_response] WebhookRejected),
    #[response(status = 404)]
    NotFound(#[ref_response] NotFound),
    #[response(status = 503)]
    Unavailable(#[ref_response] ServiceUnavailable),
}

#[derive(OpenApi)]
#[openapi(components(responses(NotFound, WebhookRejected, ServiceUnavailable)))]
struct WebhookComponents;

pub fn with_webhook_responses<S>(
    contract: infra_http::HttpContract<S>,
) -> Result<infra_http::HttpContract<S>, infra_http::FinalizeError>
where
    S: Clone + Send + Sync + 'static,
{
    metrics::describe_counter!(
        WEBHOOK_INGRESS_OUTCOMES_METRIC,
        metrics::Unit::Count,
        "Inbound webhook deliveries by admission outcome."
    );
    contract.with_components(WebhookComponents::openapi().components.unwrap_or_default())
}

pub fn router<S>() -> Result<infra_http::HttpContract<S>, infra_http::FinalizeError>
where
    WebhookState: FromRef<S>,
    S: Clone + Send + Sync + 'static,
{
    with_webhook_responses(infra_http::http_route!(receive)?)
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
    responses(ReceiveResponses)
)]
async fn receive(
    Path(endpoint_id): Path<CompactString>,
    State(state): State<WebhookState>,
    request: Request,
) -> Response {
    state.receive(&endpoint_id, request).await
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
}

impl OutcomeSlot {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Duplicate => "duplicate",
            Self::Unavailable => "unavailable",
            Self::DeadlineExceeded => "deadline_exceeded",
        }
    }
}

struct EndpointMetrics {
    endpoint: SharedString,
    outcomes: EnumMap<OutcomeSlot, OnceLock<Counter>>,
    protocol_rejections: EnumMap<ProtocolError, OnceLock<Counter>>,
    body_unreadable: OnceLock<Counter>,
    payload_too_large: OnceLock<Counter>,
    custom_rejections: ArcSwap<HashMap<&'static str, Counter>>,
}

impl EndpointMetrics {
    fn new(endpoint: SharedString) -> Self {
        Self {
            endpoint,
            outcomes: EnumMap::default(),
            protocol_rejections: EnumMap::default(),
            body_unreadable: OnceLock::new(),
            payload_too_large: OnceLock::new(),
            custom_rejections: ArcSwap::default(),
        }
    }

    fn record(&self, slot: OutcomeSlot) {
        self.outcomes[slot]
            .get_or_init(|| {
                metrics::counter!(
                    WEBHOOK_INGRESS_OUTCOMES_METRIC,
                    "endpoint" => self.endpoint.clone(),
                    "outcome" => slot.as_str()
                )
            })
            .increment(1);
    }

    fn record_protocol_rejection(&self, error: ProtocolError) {
        self.protocol_rejections[error]
            .get_or_init(|| self.rejection_counter(error.reason()))
            .increment(1);
    }

    fn record_body_unreadable(&self) {
        self.record_fixed_rejection(&self.body_unreadable, "body_unreadable");
    }

    fn record_rejection(&self, rejection: Rejection) {
        match rejection.kind() {
            RejectionKind::Protocol(error) => self.record_protocol_rejection(error),
            RejectionKind::PayloadTooLarge => {
                self.record_fixed_rejection(&self.payload_too_large, PAYLOAD_TOO_LARGE.reason());
            }
            RejectionKind::Custom(reason) => self.record_custom_rejection(reason),
        }
    }

    fn record_fixed_rejection(&self, slot: &OnceLock<Counter>, reason: &'static str) {
        slot.get_or_init(|| self.rejection_counter(reason))
            .increment(1);
    }

    fn record_custom_rejection(&self, reason: &'static str) {
        let snapshot = self.custom_rejections.load();
        if let Some(counter) = snapshot.get(reason) {
            counter.increment(1);
            return;
        }
        drop(snapshot);
        let counter = self.rejection_counter(reason);
        self.custom_rejections.rcu(|current| {
            if current.contains_key(reason) {
                return Arc::clone(current);
            }
            let mut next = (**current).clone();
            next.insert(reason, counter.clone());
            Arc::new(next)
        });
        counter.increment(1);
    }

    fn rejection_counter(&self, reason: &'static str) -> Counter {
        metrics::counter!(
            WEBHOOK_INGRESS_OUTCOMES_METRIC,
            "endpoint" => self.endpoint.clone(),
            "outcome" => "rejected",
            "reason" => reason
        )
    }
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use axum::http::header::CONTENT_TYPE;
    use metrics::Label;
    use tower::ServiceExt;

    use super::*;
    use crate::inbound::{MessageId, Verifiers};
    use crate::protocol::KeyRing;

    fn inert_app() -> infra_http::Application {
        use std::time::Duration;
        router()
            .unwrap()
            .build(
                WebhookState::inert(),
                infra_http::Authentication::Public,
                &infra_http::HardenOptions {
                    max_body_bytes: MAX_BODY_BYTES + 1,
                    request_timeout: Duration::from_secs(1),
                    max_in_flight: None,
                    admission_wait: Duration::ZERO,
                    log_health_probes: false,
                    trust_trace_context: false,
                    draining: None,
                },
            )
            .expect("the webhook operation is explicitly public")
    }

    async fn body_text(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        String::from_utf8(bytes.to_vec()).expect("UTF-8 body")
    }

    #[tokio::test]
    async fn outcomes_label_a_configured_endpoint_and_never_an_unknown_one() {
        use metrics_util::debugging::DebugValue;

        let recorder = metrics_util::debugging::DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let pool = infra_postgres::test_support::connect_lazy("postgres://127.0.0.1:1/unused")
            .expect("lazy pool opens no connection");
        let keys = KeyRing::from_encoded("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=", None)
            .expect("key");
        let mut verifiers = Verifiers::new();
        verifiers.bind("partner", keys);
        let state = WebhookState::active(verifiers.into_receiver(pool));
        let cloned = state.clone();
        metrics::with_local_recorder(&recorder, || {
            let (_, partner) = state.state.endpoint("partner").expect("partner endpoint");
            partner.metrics.record(OutcomeSlot::Accepted);
            let (_, cloned_partner) = cloned.state.endpoint("partner").expect("partner endpoint");
            cloned_partner.metrics.record(OutcomeSlot::Accepted);
            partner
                .metrics
                .record_rejection(Rejection::from(ProtocolError::InvalidSignature));
            partner
                .metrics
                .record_rejection(Rejection::new("invalid_token"));
            partner
                .metrics
                .record_rejection(Rejection::new("invalid_token"));
            let _ = state.state.reject_unknown_endpoint();
            let _ = cloned.state.reject_unknown_endpoint();
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
                    2,
                ),
                (vec![Label::new("outcome", "unknown_endpoint")], 2),
            ]
        );

        let second_recorder = metrics_util::debugging::DebuggingRecorder::new();
        let second_snapshotter = second_recorder.snapshotter();
        let inert = WebhookState::inert();
        metrics::with_local_recorder(&second_recorder, || {
            let _ = inert.clone().state.reject_unknown_endpoint();
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

        let pool = infra_postgres::test_support::connect_lazy("postgres://127.0.0.1:1/unused")
            .expect("lazy pool opens no connection");
        let active = WebhookState::active(Verifiers::new().into_receiver(pool));
        assert_eq!(format!("{active:?}"), "WebhookState { active: true }");
        assert_eq!(
            format!("{active:#?}"),
            "WebhookState {\n    active: true,\n}"
        );
    }

    #[tokio::test]
    async fn inert_receiver_returns_a_problem_before_reading_or_authenticating_the_body() {
        let response = inert_app()
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
        let response = inert_app()
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

    #[utoipa::path(
        post,
        path = "/webhooks/provider",
        security(),
        responses(ReceiveResponses)
    )]
    async fn receive_provider(
        State(state): State<WebhookState>,
        request: axum::extract::Request,
    ) -> Response {
        state.receive("provider", request).await
    }

    #[tokio::test]
    async fn custom_provider_route_reuses_webhook_state_receive() {
        use std::time::Duration;

        struct TokenVerifier;
        impl Verifier for TokenVerifier {
            fn authenticate(
                &self,
                headers: &http::HeaderMap,
                _body: &[u8],
                _now: SystemTime,
            ) -> Result<MessageId, Rejection> {
                if headers
                    .get("x-provider-token")
                    .and_then(|value| value.to_str().ok())
                    != Some("expected")
                {
                    return Err(Rejection::new("invalid_token"));
                }
                Ok(bytes::Bytes::from_static(b"delivery-1"))
            }
        }

        let pool = infra_postgres::test_support::connect_lazy("postgres://127.0.0.1:1/unused")
            .expect("lazy pool opens no connection");
        let mut verifiers = Verifiers::new();
        verifiers.bind("provider", Arc::new(TokenVerifier));
        let state = WebhookState::active(verifiers.into_receiver(pool));
        let contract = with_webhook_responses(
            infra_http::http_route!(receive_provider).expect("custom provider route registers"),
        )
        .expect("webhook response components attach");
        let merged = router::<WebhookState>()
            .expect("standard webhook route registers")
            .merge(contract.clone())
            .expect("custom provider route merges without component conflicts");
        assert!(merged.into_openapi().is_ok());
        let app = contract
            .build(
                state,
                infra_http::Authentication::Public,
                &infra_http::HardenOptions {
                    max_body_bytes: MAX_BODY_BYTES + 1,
                    request_timeout: Duration::from_secs(1),
                    max_in_flight: None,
                    admission_wait: Duration::ZERO,
                    log_health_probes: false,
                    trust_trace_context: false,
                    draining: None,
                },
            )
            .expect("custom provider route is public");

        let rejected = app
            .clone()
            .oneshot(
                Request::post("/webhooks/provider")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("router response");
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        assert!(
            body_text(rejected)
                .await
                .contains(r#""code":"webhook_rejected""#)
        );

        let oversized = app
            .oneshot(
                Request::post("/webhooks/provider")
                    .body(Body::from(vec![b'x'; MAX_BODY_BYTES + 1]))
                    .expect("request"),
            )
            .await
            .expect("router response");
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            body_text(oversized)
                .await
                .contains(r#""code":"request_entity_too_large""#)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unfinished_webhook_upload_returns_the_declared_timeout() {
        use std::time::Duration;

        let pool = infra_postgres::test_support::connect_lazy("postgres://localhost/unused")
            .expect("lazy pool");
        let mut verifiers = Verifiers::new();
        verifiers.bind(
            "partner/a?#",
            KeyRing::from_encoded("whsec_Q0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0M=", None)
                .expect("key"),
        );
        let app = router()
            .expect("webhook operation registers")
            .build(
                WebhookState::active(verifiers.into_receiver(pool)),
                infra_http::Authentication::Public,
                &infra_http::HardenOptions {
                    max_body_bytes: MAX_BODY_BYTES + 1,
                    request_timeout: Duration::from_secs(1),
                    max_in_flight: None,
                    admission_wait: Duration::from_secs(1),
                    log_health_probes: false,
                    trust_trace_context: false,
                    draining: None,
                },
            )
            .expect("webhook operation is public");
        let upload = Body::from_stream(futures_util::stream::pending::<
            Result<bytes::Bytes, std::io::Error>,
        >());
        let response = app
            .oneshot(
                Request::post("/webhooks/partner%2Fa%3F%23")
                    .body(upload)
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
        let bytes = to_bytes(response.into_body(), 4096)
            .await
            .expect("problem body");
        let problem: serde_json::Value = serde_json::from_slice(&bytes).expect("problem JSON");
        assert_eq!(problem["code"], "request_timeout");
        let document = router::<WebhookState>()
            .expect("webhook operation registers")
            .into_openapi()
            .expect("valid webhook contract");
        let document = serde_json::to_value(document).expect("document");
        assert_eq!(
            document["paths"]["/webhooks/{endpoint_id}"]["post"]["responses"]["408"]["$ref"],
            "#/components/responses/RequestTimeout"
        );
    }
}
