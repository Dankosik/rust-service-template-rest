mod producer;

use std::{
    collections::BTreeMap,
    num::NonZeroU32,
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use enum_map::{Enum, EnumMap};
use foldhash::HashMap;
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, header};
use infra_jobs::{Handler, Job, JobError, JobKind, KindsBuilder, Policy};
use infra_outbound_http::{BuildError, Client, Error as HttpError, Limits};
use metrics::{Counter, SharedString};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use url::Url;

use crate::StreamingBase64;
use crate::protocol::{KeyRing, WEBHOOK_ID, WEBHOOK_SIGNATURE, WEBHOOK_TIMESTAMP};

pub use producer::{JSON_CONTENT_TYPE, OutboundError, Producer};

const DELIVERY_KIND: &str = "webhooks.deliver";
const RESPONSE_HEADER_COUNT: usize = 64;
const RESPONSE_BODY_BYTES: usize = 64 * 1024;

pub const WEBHOOK_DELIVERY_OUTCOMES_METRIC: &str = "webhook_delivery_outcomes_total";

pub const DELIVERY_POLICY: Policy = Policy {
    max_attempts: 20,
    timeout: Duration::from_secs(30),
    max_running: None,
};

const LIMITS: Limits = Limits {
    operation_timeout: DELIVERY_POLICY.timeout,
    response_header_count: RESPONSE_HEADER_COUNT,
    response_body_bytes: RESPONSE_BODY_BYTES,
};

#[derive(Clone, derive_more::Debug)]
#[debug("Endpoint([REDACTED])")]
pub struct Endpoint {
    destination: Option<Uri>,
    client: Client,
    keys: KeyRing,
}

impl Endpoint {
    pub fn try_new(destination: &str, keys: KeyRing) -> Result<Self, EndpointError> {
        if shared_rules::uri::has_whitespace_or_control(destination)
            || shared_rules::uri::authority_has_userinfo(destination)
        {
            return Err(EndpointError::InvalidDestination);
        }
        let destination = Url::parse(destination)
            .ok()
            .filter(|destination| destination.scheme() == "https" && is_admissible(destination))
            .ok_or(EndpointError::InvalidDestination)?;
        let client = Client::try_new(&destination, LIMITS).map_err(EndpointError::Client)?;
        Ok(Self::from_destination(destination, client, keys))
    }

    #[cfg(feature = "test-support")]
    pub fn with_client(
        destination: Url,
        client: Client,
        keys: KeyRing,
    ) -> Result<Self, EndpointError> {
        if !matches!(destination.scheme(), "http" | "https") || !is_admissible(&destination) {
            return Err(EndpointError::InvalidDestination);
        }
        Ok(Self::from_destination(destination, client, keys))
    }

    fn from_destination(destination: Url, client: Client, keys: KeyRing) -> Self {
        Self {
            destination: Uri::try_from(String::from(destination)).ok(),
            client,
            keys,
        }
    }
}

fn is_admissible(destination: &Url) -> bool {
    destination.host().is_some()
        && destination.username().is_empty()
        && destination.password().is_none()
        && destination.fragment().is_none()
}

#[derive(derive_more::Debug)]
#[debug("Dispatcher([REDACTED])")]
pub struct Dispatcher {
    endpoints: HashMap<Arc<str>, ConfiguredEndpoint>,
    missing_endpoint: OnceLock<Counter>,
}

struct ConfiguredEndpoint {
    endpoint: Endpoint,
    metrics: EndpointMetrics,
}

impl Dispatcher {
    #[must_use]
    pub fn new(endpoints: BTreeMap<String, Endpoint>) -> Self {
        let endpoints = endpoints
            .into_iter()
            .map(|(endpoint_id, endpoint)| {
                let id: Arc<str> = Arc::from(endpoint_id);
                let metrics = EndpointMetrics::new(SharedString::from(Arc::clone(&id)));
                (id, ConfiguredEndpoint { endpoint, metrics })
            })
            .collect();
        Self {
            endpoints,
            missing_endpoint: OnceLock::new(),
        }
    }

    pub fn register(
        self,
        kinds: &mut KindsBuilder,
        max_concurrent: Option<NonZeroU32>,
    ) -> &mut KindsBuilder {
        metrics::describe_counter!(
            WEBHOOK_DELIVERY_OUTCOMES_METRIC,
            metrics::Unit::Count,
            "Outbound webhook delivery attempts by result."
        );
        let policy = Policy {
            max_running: max_concurrent,
            ..DELIVERY_POLICY
        };
        kinds.register(policy, self)
    }

    fn fail_missing_endpoint(&self, endpoint_id: &str) -> JobError {
        tracing::info!(
            webhook.endpoint = endpoint_id,
            webhook.outcome = "retryable",
            webhook.reason = "missing_endpoint",
            http.status = Option::<u16>::None,
            "webhook_delivery_finished"
        );
        self.missing_endpoint
            .get_or_init(|| {
                metrics::counter!(
                    WEBHOOK_DELIVERY_OUTCOMES_METRIC,
                    "outcome" => "retryable",
                    "reason" => "missing_endpoint"
                )
            })
            .increment(1);
        JobError::retryable("missing_endpoint")
    }
}

impl Handler<Delivery> for Dispatcher {
    async fn run(&self, job: Job<Delivery>) -> Result<(), JobError> {
        let delivery = job.payload();
        let endpoint_id = delivery.endpoint_id.as_str();
        let Some(ConfiguredEndpoint { endpoint, metrics }) = self.endpoints.get(endpoint_id) else {
            return Err(self.fail_missing_endpoint(endpoint_id));
        };
        let Some(timestamp) = unix_timestamp(SystemTime::now()) else {
            return Err(fail(
                endpoint_id,
                metrics,
                DeliveryFailure::ClockUnavailable,
            ));
        };
        let message_id = job.id().to_string();
        let Some(request) = endpoint.signed_request(delivery, &message_id, timestamp) else {
            return Err(fail(endpoint_id, metrics, DeliveryFailure::InvalidPayload));
        };
        let response = endpoint
            .client
            .execute_for_headers(request, &job.context())
            .await;
        classify_response(endpoint_id, metrics, response, SystemTime::now())
    }
}

#[serde_as]
#[derive(Clone, Serialize, Deserialize)]
struct Delivery {
    #[serde(skip_deserializing, default = "delivery_version")]
    version: u8,
    endpoint_id: String,
    content_type: String,
    #[serde_as(
        serialize_as = "StreamingBase64",
        deserialize_as = "serde_with::base64::Base64"
    )]
    body: Bytes,
}

const fn delivery_version() -> u8 {
    2
}

const EMPTY_DELIVERY_JSON: &str = r#"{"version":2,"endpoint_id":"","content_type":"","body":""}"#;

impl JobKind for Delivery {
    const NAME: &'static str = DELIVERY_KIND;
}

const _: () = infra_jobs::assert_valid_kind_name(DELIVERY_KIND);

#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    #[error("outbound webhook destination is invalid")]
    InvalidDestination,
    #[error("outbound webhook client configuration is invalid")]
    Client(#[source] BuildError),
}

#[derive(Clone, Copy, Debug, Enum)]
enum OutcomeSlot {
    Delivered,
    InvalidPayload,
    ClockUnavailable,
    ResponseStatus,
    RetryAfterExceedsMaxDelay,
    EndpointGone,
    Timeout,
    Cancelled,
    Transport,
    Client,
}

struct EndpointMetrics {
    endpoint: SharedString,
    outcomes: EnumMap<OutcomeSlot, OnceLock<Counter>>,
}

impl EndpointMetrics {
    fn new(endpoint: SharedString) -> Self {
        Self {
            endpoint,
            outcomes: EnumMap::default(),
        }
    }

    fn record_delivered(&self) {
        self.outcomes[OutcomeSlot::Delivered]
            .get_or_init(|| {
                metrics::counter!(
                    WEBHOOK_DELIVERY_OUTCOMES_METRIC,
                    "endpoint" => self.endpoint.clone(),
                    "outcome" => "delivered"
                )
            })
            .increment(1);
    }

    fn record_failure(&self, failure: DeliveryFailure) {
        self.outcomes[failure.slot()]
            .get_or_init(|| {
                metrics::counter!(
                    WEBHOOK_DELIVERY_OUTCOMES_METRIC,
                    "endpoint" => self.endpoint.clone(),
                    "outcome" => failure.outcome(),
                    "reason" => failure.reason()
                )
            })
            .increment(1);
    }
}

#[derive(Clone, Copy)]
enum DeliveryFailure {
    InvalidPayload,
    ClockUnavailable,
    Status(StatusCode),
    RetryAfterExceedsMaxDelay(StatusCode),
    EndpointGone,
    Timeout,
    Cancelled,
    Transport,
    Client,
}

impl DeliveryFailure {
    const fn from_exchange(error: &HttpError) -> Self {
        match error {
            HttpError::Timeout => Self::Timeout,
            HttpError::Cancelled => Self::Cancelled,
            HttpError::Transport { .. } => Self::Transport,
            HttpError::InvalidTarget
            | HttpError::DispatchRefused
            | HttpError::ResponseBodyTooLarge => Self::Client,
        }
    }

    const fn slot(self) -> OutcomeSlot {
        match self {
            Self::InvalidPayload => OutcomeSlot::InvalidPayload,
            Self::ClockUnavailable => OutcomeSlot::ClockUnavailable,
            Self::Status(_) => OutcomeSlot::ResponseStatus,
            Self::RetryAfterExceedsMaxDelay(_) => OutcomeSlot::RetryAfterExceedsMaxDelay,
            Self::EndpointGone => OutcomeSlot::EndpointGone,
            Self::Timeout => OutcomeSlot::Timeout,
            Self::Cancelled => OutcomeSlot::Cancelled,
            Self::Transport => OutcomeSlot::Transport,
            Self::Client => OutcomeSlot::Client,
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::InvalidPayload => "invalid_payload",
            Self::ClockUnavailable => "clock_unavailable",
            Self::Status(_) => "response_status",
            Self::RetryAfterExceedsMaxDelay(_) => "retry_after_exceeds_max_delay",
            Self::EndpointGone => "endpoint_gone",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Transport => "transport",
            Self::Client => "client",
        }
    }

    const fn is_permanent(self) -> bool {
        match self {
            Self::InvalidPayload | Self::EndpointGone | Self::RetryAfterExceedsMaxDelay(_) => true,
            Self::ClockUnavailable
            | Self::Status(_)
            | Self::Timeout
            | Self::Cancelled
            | Self::Transport
            | Self::Client => false,
        }
    }

    const fn outcome(self) -> &'static str {
        if self.is_permanent() {
            "permanent"
        } else {
            "retryable"
        }
    }

    const fn status(self) -> Option<StatusCode> {
        match self {
            Self::Status(status) | Self::RetryAfterExceedsMaxDelay(status) => Some(status),
            Self::EndpointGone => Some(StatusCode::GONE),
            Self::InvalidPayload
            | Self::ClockUnavailable
            | Self::Timeout
            | Self::Cancelled
            | Self::Transport
            | Self::Client => None,
        }
    }
}

impl Endpoint {
    fn signed_request(
        &self,
        delivery: &Delivery,
        message_id: &str,
        timestamp: i64,
    ) -> Option<Request<Bytes>> {
        let signatures = self
            .keys
            .signatures(message_id.as_bytes(), timestamp, &delivery.body)
            .ok()?;
        Request::builder()
            .method(Method::POST)
            .uri(self.destination.clone()?)
            .header(header::CONTENT_TYPE, &delivery.content_type)
            .header(WEBHOOK_ID, message_id)
            .header(WEBHOOK_TIMESTAMP, HeaderValue::from(timestamp))
            .header(WEBHOOK_SIGNATURE, signatures)
            .body(delivery.body.clone())
            .ok()
    }
}

fn classify_response(
    endpoint_id: &str,
    metrics: &EndpointMetrics,
    response: Result<http::response::Parts, HttpError>,
    response_time: SystemTime,
) -> Result<(), JobError> {
    match response {
        Ok(response) if response.status.is_success() => {
            tracing::debug!(
                webhook.endpoint = endpoint_id,
                webhook.outcome = "delivered",
                http.status = response.status.as_u16(),
                "webhook_delivery_finished"
            );
            metrics.record_delivered();
            Ok(())
        }
        Ok(response) if response.status == StatusCode::GONE => {
            Err(fail(endpoint_id, metrics, DeliveryFailure::EndpointGone))
        }
        Ok(response) => {
            let failure = DeliveryFailure::Status(response.status);
            let Some(delay) = retry_after(&response.headers, response_time) else {
                return Err(fail(endpoint_id, metrics, failure));
            };
            let Ok(retry) = JobError::retry_after_at_least(failure.reason(), delay) else {
                return Err(fail(
                    endpoint_id,
                    metrics,
                    DeliveryFailure::RetryAfterExceedsMaxDelay(response.status),
                ));
            };
            record_failure(endpoint_id, metrics, failure);
            Err(retry)
        }
        Err(error) => Err(fail(
            endpoint_id,
            metrics,
            DeliveryFailure::from_exchange(&error),
        )),
    }
}

fn fail(endpoint_id: &str, metrics: &EndpointMetrics, failure: DeliveryFailure) -> JobError {
    record_failure(endpoint_id, metrics, failure);
    if failure.is_permanent() {
        JobError::permanent(failure.reason())
    } else {
        JobError::retryable(failure.reason())
    }
}

fn record_failure(endpoint_id: &str, metrics: &EndpointMetrics, failure: DeliveryFailure) {
    let outcome = failure.outcome();
    let reason = failure.reason();
    let status = failure.status().map(|status| status.as_u16());
    if failure.is_permanent() {
        tracing::warn!(
            webhook.endpoint = endpoint_id,
            webhook.outcome = outcome,
            webhook.reason = reason,
            http.status = status,
            "webhook_delivery_finished"
        );
    } else {
        tracing::info!(
            webhook.endpoint = endpoint_id,
            webhook.outcome = outcome,
            webhook.reason = reason,
            http.status = status,
            "webhook_delivery_finished"
        );
    }
    metrics.record_failure(failure);
}

fn retry_after(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?;
    let delay = if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        Duration::from_secs(value.parse().unwrap_or(u64::MAX))
    } else {
        httpdate::parse_http_date(value)
            .ok()?
            .duration_since(now)
            .ok()?
    };
    (!delay.is_zero()).then_some(delay)
}

fn unix_timestamp(now: SystemTime) -> Option<i64> {
    i64::try_from(now.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()
}

#[cfg(test)]
mod tests {
    use metrics::Label;
    use metrics_util::debugging::DebuggingRecorder;
    use std::assert_matches;
    use std::time::{Duration, UNIX_EPOCH};

    use http::{HeaderMap, HeaderValue, header};

    use super::{Endpoint, EndpointError, retry_after};
    use crate::protocol::KeyRing;

    fn ring() -> KeyRing {
        KeyRing::from_encoded("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=", None).unwrap()
    }

    #[test]
    fn an_endpoint_refuses_credentials_fragments_and_plain_http() {
        for destination in [
            "https://user@partner.example/events",
            "https://@partner.example/events",
            "https://partner.exa\nmple/events",
            "https://partner.example/events#fragment",
            "http://partner.example/events",
        ] {
            assert_matches!(
                Endpoint::try_new(destination, ring()),
                Err(EndpointError::InvalidDestination),
                "{destination} must be rejected"
            );
        }
    }

    #[test]
    fn an_endpoint_admits_a_private_https_ip() {
        assert!(Endpoint::try_new("https://10.0.0.5/events", ring()).is_ok());
    }

    #[test]
    fn https_registration_preserves_the_http_uri_length_boundary_at_delivery() {
        let origin = "https://partner.example/";
        let destination = format!("{origin}{}", "a".repeat(65_534 - origin.len()));
        let delivery = super::Delivery {
            version: super::delivery_version(),
            endpoint_id: "partner".to_owned(),
            content_type: "application/json".to_owned(),
            body: bytes::Bytes::new(),
        };

        let endpoint = Endpoint::try_new(&destination, ring()).unwrap();
        let request = endpoint.signed_request(&delivery, "id", 0).unwrap();
        assert_eq!(request.uri().to_string(), destination);
        let endpoint = Endpoint::try_new(&format!("{destination}a"), ring()).unwrap();
        assert!(endpoint.signed_request(&delivery, "id", 0).is_none());
    }

    #[test]
    fn serialized_payload_is_endpoint_content_type_and_padded_base64_body() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        for len in [1, 2, 3, 4, crate::protocol::MAX_BODY_BYTES] {
            let body = bytes::Bytes::from(vec![0xff; len]);
            let encoded = STANDARD.encode(&body);
            let delivery = super::Delivery {
                version: super::delivery_version(),
                endpoint_id: "partner".to_owned(),
                content_type: "application/json".to_owned(),
                body,
            };
            assert_eq!(
                serde_json::to_string(&delivery).unwrap(),
                format!(
                    r#"{{"version":2,"endpoint_id":"partner","content_type":"application/json","body":"{encoded}"}}"#
                )
            );
        }
    }

    #[test]
    fn empty_delivery_json_is_the_serialized_empty_delivery() {
        let empty = super::Delivery {
            version: super::delivery_version(),
            endpoint_id: String::new(),
            content_type: String::new(),
            body: bytes::Bytes::new(),
        };

        assert_eq!(
            serde_json::to_string(&empty).unwrap(),
            super::EMPTY_DELIVERY_JSON
        );
    }

    #[test]
    fn a_versioned_row_still_deserializes() {
        let delivery: super::Delivery = serde_json::from_str(
            r#"{"version":99,"endpoint_id":"partner","content_type":"application/json","body":"cmF3AGJ5dGVz/w"}"#,
        )
        .unwrap();

        assert_eq!(delivery.endpoint_id, "partner");
        assert_eq!(delivery.content_type, "application/json");
        assert_eq!(delivery.body.as_ref(), b"raw\0bytes\xff");
    }

    #[test]
    fn retry_after_accepts_unsigned_seconds_or_a_future_http_date() {
        let mut delta = HeaderMap::new();
        delta.insert(header::RETRY_AFTER, HeaderValue::from_static("3153600000"));
        assert_eq!(retry_after(&delta, UNIX_EPOCH), Some(infra_jobs::MAX_DELAY));
        for value in ["+3600", "-1", "1.5", "", " 3600", "0"] {
            let mut invalid = HeaderMap::new();
            invalid.insert(header::RETRY_AFTER, HeaderValue::from_static(value));
            assert_eq!(retry_after(&invalid, UNIX_EPOCH), None, "{value:?}");
        }

        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let mut date = HeaderMap::new();
        date.insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&httpdate::fmt_http_date(now + Duration::from_secs(3))).unwrap(),
        );
        assert_eq!(retry_after(&date, now), Some(Duration::from_secs(3)));
    }

    #[test]
    fn elapsed_retry_after_falls_back_to_jobs_backoff() {
        let mut elapsed = HeaderMap::new();
        elapsed.insert(
            header::RETRY_AFTER,
            HeaderValue::from_static("Thu, 01 Jan 1970 00:00:01 GMT"),
        );
        assert_eq!(
            retry_after(&elapsed, UNIX_EPOCH + Duration::from_secs(2)),
            None
        );
    }

    #[test]
    fn each_attempt_is_counted_by_endpoint_outcome_and_reason_without_the_status() {
        use metrics_util::debugging::DebugValue;
        use std::collections::BTreeMap;

        let response = |status: u16| {
            Ok(http::Response::builder()
                .status(status)
                .body(())
                .unwrap()
                .into_parts()
                .0)
        };
        let metrics = super::EndpointMetrics::new("partner".into());
        let dispatcher = super::Dispatcher::new(BTreeMap::new());
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            for exchange in [
                response(204),
                response(200),
                response(503),
                response(410),
                Err(super::HttpError::Timeout),
            ] {
                let _ = super::classify_response("partner", &metrics, exchange, UNIX_EPOCH);
            }
            let _ = dispatcher.fail_missing_endpoint("removed");
            let _ = dispatcher.fail_missing_endpoint("removed");
        });
        let mut samples: Vec<(Vec<Label>, u64)> = snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .map(|(key, _, _, value)| {
                let key = key.key();
                assert_eq!(key.name(), super::WEBHOOK_DELIVERY_OUTCOMES_METRIC);
                let DebugValue::Counter(count) = value else {
                    panic!("expected counter, got {value:?}");
                };
                (key.labels().cloned().collect(), count)
            })
            .collect();
        samples.sort();
        let failure = |outcome: &'static str, reason: &'static str| {
            vec![
                Label::new("endpoint", "partner"),
                Label::new("outcome", outcome),
                Label::new("reason", reason),
            ]
        };
        let mut expected = vec![
            (
                vec![
                    Label::new("endpoint", "partner"),
                    Label::new("outcome", "delivered"),
                ],
                2,
            ),
            (failure("retryable", "response_status"), 1),
            (failure("permanent", "endpoint_gone"), 1),
            (failure("retryable", "timeout"), 1),
            (
                vec![
                    Label::new("outcome", "retryable"),
                    Label::new("reason", "missing_endpoint"),
                ],
                2,
            ),
        ];
        expected.sort();
        assert_eq!(samples, expected);

        let second_recorder = DebuggingRecorder::new();
        let second_snapshotter = second_recorder.snapshotter();
        let second_dispatcher = super::Dispatcher::new(BTreeMap::new());
        metrics::with_local_recorder(&second_recorder, || {
            let _ = second_dispatcher.fail_missing_endpoint("removed");
        });
        assert_eq!(second_snapshotter.snapshot().into_vec().len(), 1);
    }
}
