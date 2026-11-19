mod producer;

use std::{
    fmt::Write as _,
    num::NonZeroU32,
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use arrayvec::ArrayString;
use bytes::Bytes;
use compact_str::CompactString;
use enum_map::{Enum, EnumMap};
use foldhash::HashMap;
use http::{HeaderMap, Request, StatusCode, Uri, header};
use infra_jobs::{Handler, Job, JobError, JobKind, KindsBuilder, Policy};
use infra_outbound_http::{BuildError, Client, Error as HttpError, Limits};
use metrics::{Counter, SharedString};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use url::Url;

use crate::StreamingBase64;
use crate::protocol::KeyRing;

pub use producer::{JSON_CONTENT_TYPE, OutboundError, Producer};

const DELIVERY_KIND: &str = "webhooks.deliver";
const RESPONSE_HEADER_COUNT: usize = 64;
const RESPONSE_BODY_BYTES: usize = 64 * 1024;
const OUTCOME_DELIVERED: &str = "delivered";
const OUTCOME_PERMANENT: &str = "permanent";
const OUTCOME_RETRYABLE: &str = "retryable";
const REASON_MISSING_ENDPOINT: &str = "missing_endpoint";

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
        let client = Client::try_new(&destination, LIMITS)?;
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
    outcomes: EndpointOutcomes,
}

impl Dispatcher {
    #[must_use]
    pub fn new(endpoints: impl IntoIterator<Item = (String, Endpoint)>) -> Self {
        let endpoints = endpoints
            .into_iter()
            .map(|(endpoint_id, endpoint)| {
                let id: Arc<str> = Arc::from(endpoint_id);
                let outcomes = EndpointOutcomes::new(SharedString::from(Arc::clone(&id)));
                (id, ConfiguredEndpoint { endpoint, outcomes })
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
            webhook.outcome = OUTCOME_RETRYABLE,
            webhook.reason = REASON_MISSING_ENDPOINT,
            http.status = Option::<u16>::None,
            "webhook_delivery_finished"
        );
        self.missing_endpoint
            .get_or_init(|| {
                metrics::counter!(
                    WEBHOOK_DELIVERY_OUTCOMES_METRIC,
                    "outcome" => OUTCOME_RETRYABLE,
                    "reason" => REASON_MISSING_ENDPOINT
                )
            })
            .increment(1);
        JobError::retryable(REASON_MISSING_ENDPOINT)
    }
}

impl Handler<Delivery> for Dispatcher {
    async fn run(&self, job: Job<Delivery>) -> Result<(), JobError> {
        let delivery = job.payload();
        let endpoint_id = delivery.endpoint_id.as_str();
        let Some(ConfiguredEndpoint { endpoint, outcomes }) = self.endpoints.get(endpoint_id)
        else {
            return Err(self.fail_missing_endpoint(endpoint_id));
        };
        let Some(timestamp) = unix_timestamp(SystemTime::now()) else {
            return Err(outcomes.fail(FailureKind::ClockUnavailable));
        };
        let Some(request) = format_message_id(job.id())
            .and_then(|message_id| endpoint.signed_request(delivery, &message_id, timestamp))
        else {
            return Err(outcomes.fail(FailureKind::InvalidPayload));
        };
        let response = endpoint
            .client
            .execute_for_headers(request, &job.context())
            .await;
        outcomes.finish_attempt(response)
    }
}

fn format_message_id(job_id: infra_jobs::JobId) -> Option<ArrayString<36>> {
    let mut message_id = ArrayString::new();
    write!(message_id, "{job_id}").ok()?;
    Some(message_id)
}

#[serde_as]
#[derive(Clone, Serialize, Deserialize)]
struct Delivery {
    #[serde(skip_deserializing, default = "delivery_version")]
    version: u8,
    endpoint_id: CompactString,
    content_type: CompactString,
    #[serde_as(as = "StreamingBase64")]
    body: Bytes,
}

const fn delivery_version() -> u8 {
    2
}

impl Delivery {
    fn empty_serialized_len() -> Result<usize, serde_json::Error> {
        static LEN: OnceLock<usize> = OnceLock::new();
        if let Some(&len) = LEN.get() {
            return Ok(len);
        }
        let len = infra_jobs::json_payload_len(&Self {
            version: delivery_version(),
            endpoint_id: CompactString::const_new(""),
            content_type: CompactString::const_new(""),
            body: Bytes::new(),
        })?;
        Ok(*LEN.get_or_init(|| len))
    }
}

impl JobKind for Delivery {
    const NAME: &'static str = DELIVERY_KIND;
}

const _: () = infra_jobs::assert_valid_kind_name(DELIVERY_KIND);

#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    #[error("outbound webhook destination is invalid")]
    InvalidDestination,
    #[error("outbound webhook client configuration is invalid")]
    Client(#[from] BuildError),
}

#[derive(Clone, Copy, Debug, Enum)]
enum FailureKind {
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

impl FailureKind {
    const fn from_http_error(error: &HttpError) -> Self {
        match error {
            HttpError::Timeout => Self::Timeout,
            HttpError::Cancelled => Self::Cancelled,
            HttpError::Transport { .. } => Self::Transport,
            HttpError::InvalidTarget
            | HttpError::DispatchRefused
            | HttpError::ResponseBodyTooLarge => Self::Client,
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::InvalidPayload => "invalid_payload",
            Self::ClockUnavailable => "clock_unavailable",
            Self::ResponseStatus => "response_status",
            Self::RetryAfterExceedsMaxDelay => "retry_after_exceeds_max_delay",
            Self::EndpointGone => "endpoint_gone",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Transport => "transport",
            Self::Client => "client",
        }
    }

    const fn is_permanent(self) -> bool {
        matches!(
            self,
            Self::InvalidPayload | Self::EndpointGone | Self::RetryAfterExceedsMaxDelay
        )
    }

    const fn outcome(self) -> &'static str {
        if self.is_permanent() {
            OUTCOME_PERMANENT
        } else {
            OUTCOME_RETRYABLE
        }
    }

    fn into_job_error(self) -> JobError {
        if self.is_permanent() {
            JobError::permanent(self.reason())
        } else {
            JobError::retryable(self.reason())
        }
    }
}

struct AttemptFailure {
    kind: FailureKind,
    status: Option<StatusCode>,
    error: JobError,
}

impl AttemptFailure {
    fn new(kind: FailureKind, status: Option<StatusCode>) -> Self {
        Self {
            kind,
            status,
            error: kind.into_job_error(),
        }
    }
}

struct EndpointOutcomes {
    endpoint: SharedString,
    delivered: OnceLock<Counter>,
    failures: EnumMap<FailureKind, OnceLock<Counter>>,
}

impl EndpointOutcomes {
    fn new(endpoint: SharedString) -> Self {
        Self {
            endpoint,
            delivered: OnceLock::new(),
            failures: EnumMap::default(),
        }
    }

    fn finish_attempt(
        &self,
        response: Result<http::response::Parts, HttpError>,
    ) -> Result<(), JobError> {
        match classify_response(response) {
            Ok(status) => {
                self.record_delivered(status);
                Ok(())
            }
            Err(failure) => {
                self.record_failure(failure.kind, failure.status);
                Err(failure.error)
            }
        }
    }

    fn fail(&self, kind: FailureKind) -> JobError {
        self.record_failure(kind, None);
        kind.into_job_error()
    }

    fn record_delivered(&self, status: StatusCode) {
        tracing::debug!(
            webhook.endpoint = &*self.endpoint,
            webhook.outcome = OUTCOME_DELIVERED,
            http.status = status.as_u16(),
            "webhook_delivery_finished"
        );
        self.delivered
            .get_or_init(|| {
                metrics::counter!(
                    WEBHOOK_DELIVERY_OUTCOMES_METRIC,
                    "endpoint" => self.endpoint.clone(),
                    "outcome" => OUTCOME_DELIVERED
                )
            })
            .increment(1);
    }

    fn record_failure(&self, kind: FailureKind, status: Option<StatusCode>) {
        let outcome = kind.outcome();
        let reason = kind.reason();
        let status = status.map(|status| status.as_u16());
        if kind.is_permanent() {
            tracing::warn!(
                webhook.endpoint = &*self.endpoint,
                webhook.outcome = outcome,
                webhook.reason = reason,
                http.status = status,
                "webhook_delivery_finished"
            );
        } else {
            tracing::info!(
                webhook.endpoint = &*self.endpoint,
                webhook.outcome = outcome,
                webhook.reason = reason,
                http.status = status,
                "webhook_delivery_finished"
            );
        }
        self.failures[kind]
            .get_or_init(|| {
                metrics::counter!(
                    WEBHOOK_DELIVERY_OUTCOMES_METRIC,
                    "endpoint" => self.endpoint.clone(),
                    "outcome" => outcome,
                    "reason" => reason
                )
            })
            .increment(1);
    }
}

fn classify_response(
    response: Result<http::response::Parts, HttpError>,
) -> Result<StatusCode, AttemptFailure> {
    match response {
        Ok(response) if response.status.is_success() => Ok(response.status),
        Ok(response) if response.status == StatusCode::GONE => Err(AttemptFailure::new(
            FailureKind::EndpointGone,
            Some(StatusCode::GONE),
        )),
        Ok(response) => {
            let status = response.status;
            let Some(delay) = retry_after(&response.headers, SystemTime::now()) else {
                return Err(AttemptFailure::new(
                    FailureKind::ResponseStatus,
                    Some(status),
                ));
            };
            if let Ok(error) =
                JobError::retry_after_at_least(FailureKind::ResponseStatus.reason(), delay)
            {
                Err(AttemptFailure {
                    kind: FailureKind::ResponseStatus,
                    status: Some(status),
                    error,
                })
            } else {
                Err(AttemptFailure::new(
                    FailureKind::RetryAfterExceedsMaxDelay,
                    Some(status),
                ))
            }
        }
        Err(error) => Err(AttemptFailure::new(
            FailureKind::from_http_error(&error),
            None,
        )),
    }
}

impl Endpoint {
    fn signed_request(
        &self,
        delivery: &Delivery,
        message_id: &str,
        timestamp: i64,
    ) -> Option<Request<Bytes>> {
        let destination = self.destination.clone()?;
        let mut request = Request::post(destination)
            .header(header::CONTENT_TYPE, delivery.content_type.as_str())
            .body(delivery.body.clone())
            .ok()?;
        self.keys
            .write_headers(
                request.headers_mut(),
                message_id.as_bytes(),
                timestamp,
                &delivery.body,
            )
            .ok()?;
        Some(request)
    }
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
            endpoint_id: "partner".into(),
            content_type: "application/json".into(),
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
                endpoint_id: "partner".into(),
                content_type: "application/json".into(),
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
    fn retry_after_accepts_positive_seconds_or_a_future_http_date() {
        let now = UNIX_EPOCH + Duration::from_secs(2);
        for (value, expected_seconds) in [
            ("3153600000", Some(3_153_600_000)),
            ("Thu, 01 Jan 1970 00:00:05 GMT", Some(3)),
            ("Thu, 01 Jan 1970 00:00:01 GMT", None),
            ("+3600", None),
            ("-1", None),
            ("1.5", None),
            ("", None),
            (" 3600", None),
            ("0", None),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::RETRY_AFTER, HeaderValue::from_static(value));
            assert_eq!(
                retry_after(&headers, now),
                expected_seconds.map(Duration::from_secs),
                "{value:?}"
            );
        }
    }

    #[test]
    fn each_attempt_is_counted_by_endpoint_outcome_and_reason_without_the_status() {
        use metrics_util::debugging::DebugValue;

        let response = |status: u16| {
            Ok(http::Response::builder()
                .status(status)
                .body(())
                .unwrap()
                .into_parts()
                .0)
        };
        let outcomes = super::EndpointOutcomes::new("partner".into());
        let dispatcher = super::Dispatcher::new([]);
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
                let _ = outcomes.finish_attempt(exchange);
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
        let second_dispatcher = super::Dispatcher::new([]);
        metrics::with_local_recorder(&second_recorder, || {
            let _ = second_dispatcher.fail_missing_endpoint("removed");
        });
        assert_eq!(second_snapshotter.snapshot().into_vec().len(), 1);
    }
}
