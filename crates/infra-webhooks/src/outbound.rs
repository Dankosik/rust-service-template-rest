//! Durable Standard Webhooks delivery through the existing jobs and HTTPS owners.
//!
//! Producers enqueue final bytes for a configured endpoint ID inside their business
//! transaction. Each attempt uses the worker startup snapshot of endpoints for routing
//! and signing, then performs one bounded exchange.

mod producer;

use std::{
    collections::BTreeMap,
    fmt,
    num::NonZeroU32,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use infra_jobs::{Handler, Job, JobError, JobKind, KindsBuilder, Policy};
use infra_outbound_http::{BuildError, Client, Error as HttpError, Limits};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use url::Url;

use crate::protocol::{KeyRing, WEBHOOK_ID, WEBHOOK_SIGNATURE, WEBHOOK_TIMESTAMP};

pub use producer::{JSON_CONTENT_TYPE, OutboundError, Producer};

const DELIVERY_KIND: &str = "webhooks.deliver";
const RETRY_AFTER_CAP: Duration = Duration::from_hours(24);
const RESPONSE_HEADER_COUNT: usize = 64;
const RESPONSE_BODY_BYTES: usize = 64 * 1024;

/// Delivery attempts by result. Label `outcome` is `delivered`, `retryable`,
/// or `permanent`. A configured endpoint adds its operator-chosen ID as
/// `endpoint`, and a failure adds its static `reason`.
pub const WEBHOOK_DELIVERY_OUTCOMES_METRIC: &str = "webhook_delivery_outcomes_total";

/// Jobs policy for one outbound delivery attempt. [`Dispatcher::register`]
/// sets the concurrency bound.
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

/// Worker side: one endpoint's destination, transport client and signing keys.
#[derive(Clone, derive_more::Debug)]
#[debug("Endpoint([REDACTED])")]
pub struct Endpoint {
    destination: Url,
    client: Client,
    keys: KeyRing,
}

impl Endpoint {
    /// Parse the HTTPS destination (no credentials, no fragment) and build its client.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::InvalidDestination`] for an unusable destination,
    /// and [`EndpointError::Client`] when client construction fails.
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
        Ok(Self {
            destination,
            client,
            keys,
        })
    }

    /// Use a caller-built local test client. Production code must use
    /// [`Endpoint::try_new`].
    ///
    /// The caller supplies the absolute destination admitted by its client.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::InvalidDestination`] when the destination is not
    /// `http` or `https`, has no host, or carries credentials or a fragment.
    #[cfg(feature = "test-support")]
    pub fn with_client(
        destination: Url,
        client: Client,
        keys: KeyRing,
    ) -> Result<Self, EndpointError> {
        if !matches!(destination.scheme(), "http" | "https") || !is_admissible(&destination) {
            return Err(EndpointError::InvalidDestination);
        }
        Ok(Self {
            destination,
            client,
            keys,
        })
    }
}

/// A destination with a host and no credentials or fragment; each constructor
/// owns its scheme rule.
fn is_admissible(destination: &Url) -> bool {
    destination.host().is_some()
        && destination.username().is_empty()
        && destination.password().is_none()
        && destination.fragment().is_none()
}

/// Delivery handler over the worker's startup snapshot of endpoints.
#[derive(derive_more::Debug)]
#[debug("Dispatcher([REDACTED])")]
pub struct Dispatcher {
    endpoints: BTreeMap<String, Endpoint>,
}

impl Dispatcher {
    #[must_use]
    pub fn new(endpoints: BTreeMap<String, Endpoint>) -> Self {
        Self { endpoints }
    }

    /// Register `webhooks.deliver` with the existing jobs kind registry.
    ///
    /// `max_concurrent` bounds the deliveries one engine runs at once, so a
    /// receiver that answers slowly cannot hold every job slot; `None` leaves
    /// the engine's slots as the only bound. The dispatcher is consumed into
    /// the registered handler; no webhook task or retry loop is spawned.
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
}

impl Handler<Delivery> for Dispatcher {
    async fn run(&self, job: Job<Delivery>) -> Result<(), JobError> {
        let delivery = job.payload();
        let endpoint_id = delivery.endpoint_id.as_str();
        let Some(endpoint) = self.endpoints.get(endpoint_id) else {
            return Err(fail(endpoint_id, DeliveryFailure::MissingEndpoint));
        };
        let Some(timestamp) = unix_timestamp(SystemTime::now()) else {
            return Err(fail(endpoint_id, DeliveryFailure::ClockUnavailable));
        };
        let message_id = job.id().to_string();
        let Some(request) = endpoint.signed_request(delivery, &message_id, timestamp) else {
            return Err(fail(endpoint_id, DeliveryFailure::InvalidPayload));
        };
        let mut endpoint_gone = false;
        let response = endpoint
            .client
            .execute_with_response_status(request, &job.context(), |status| {
                endpoint_gone = status == StatusCode::GONE;
            })
            .await;
        if endpoint_gone
            && matches!(
                &response,
                Err(HttpError::ResponseBodyTooLarge | HttpError::Transport { .. })
            )
        {
            return Err(fail(endpoint_id, DeliveryFailure::EndpointGone));
        }
        classify_response(endpoint_id, response, SystemTime::now())
    }
}

/// The durable payload of one outbound delivery.
///
/// `version` is written and never read: a worker from before the tag became
/// optional refuses a row without it, so a rolling deploy or a rollback still
/// needs it on the wire. A reader ignores it, whatever its value.
///
/// A payload the worker cannot decode is retried by infra-jobs, which keeps a
/// rolling deploy safe. Do not convert decode errors to permanent failures.
#[serde_as]
#[derive(Clone, Serialize, Deserialize)]
struct Delivery {
    #[serde(skip_deserializing, default = "delivery_version")]
    version: u8,
    endpoint_id: String,
    content_type: String,
    #[serde_as(as = "serde_with::base64::Base64")]
    body: Bytes,
}

const fn delivery_version() -> u8 {
    2
}

/// [`Delivery`] serialized with empty strings and body. The size check adds
/// each field's escaped contents and the body's Base64 length to it instead
/// of serializing this constant envelope on every enqueue.
const EMPTY_DELIVERY_JSON: &str = r#"{"version":2,"endpoint_id":"","content_type":"","body":""}"#;

impl JobKind for Delivery {
    const NAME: &'static str = DELIVERY_KIND;
}

const _: () = infra_jobs::assert_valid_kind_name(DELIVERY_KIND);

/// Why a worker could not build one configured endpoint.
#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    /// The destination has no host, carries credentials or a fragment, or
    /// uses a scheme its constructor refuses: HTTPS only, except for a test
    /// client.
    #[error("outbound webhook destination is invalid")]
    InvalidDestination,
    #[error("outbound webhook client configuration is invalid")]
    Client(#[source] BuildError),
}

/// Why one delivery attempt did not deliver. It owns the attempt's retry
/// class, log level, metric labels, and jobs failure summary.
#[derive(Clone, Copy)]
enum DeliveryFailure {
    InvalidPayload,
    MissingEndpoint,
    ClockUnavailable,
    Status(StatusCode),
    EndpointGone,
    Exchange(&'static str),
}

impl DeliveryFailure {
    /// The static `reason` label for logs and metrics.
    const fn reason(self) -> &'static str {
        match self {
            Self::InvalidPayload => "invalid_payload",
            Self::MissingEndpoint => "missing_endpoint",
            Self::ClockUnavailable => "clock_unavailable",
            Self::Status(_) => "response_status",
            // The receiver asks for no more deliveries: disable or remove its
            // static binding and restart workers.
            Self::EndpointGone => "endpoint_gone",
            Self::Exchange(reason) => reason,
        }
    }

    const fn is_permanent(self) -> bool {
        match self {
            Self::InvalidPayload | Self::EndpointGone => true,
            Self::MissingEndpoint
            | Self::ClockUnavailable
            | Self::Status(_)
            | Self::Exchange(_) => false,
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
            Self::Status(status) => Some(status),
            Self::EndpointGone => Some(StatusCode::GONE),
            Self::InvalidPayload
            | Self::MissingEndpoint
            | Self::ClockUnavailable
            | Self::Exchange(_) => None,
        }
    }
}

/// The summary jobs keeps in a delivery's failure history.
impl fmt::Display for DeliveryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(status) => write!(formatter, "response_status_{}", status.as_u16()),
            _ => formatter.write_str(self.reason()),
        }
    }
}

/// Map an HTTP exchange error to its log and metric reason. An error can arrive
/// after the client has received response headers, and the receiver may already
/// have processed the request.
///
/// An invalid target stays retryable, like a missing endpoint: each endpoint's
/// client is built for its own destination, so only a configuration fix and a
/// worker restart can repair it, and retries keep the delivery until then.
const fn exchange_reason(error: &HttpError) -> &'static str {
    match error {
        HttpError::Timeout => "timeout",
        HttpError::Cancelled => "cancelled",
        HttpError::ResponseBodyTooLarge => "response_too_large",
        HttpError::Transport { .. } => "transport",
        HttpError::InvalidTarget | HttpError::DispatchRefused => "client",
    }
}

impl Endpoint {
    /// `None` when signing or request construction fails; the caller treats
    /// either as a permanent payload failure.
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
            .uri(self.destination.as_str())
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
    response: Result<http::Response<Bytes>, HttpError>,
    response_time: SystemTime,
) -> Result<(), JobError> {
    match response {
        Ok(response) if response.status().is_success() => {
            tracing::debug!(
                webhook.endpoint = endpoint_id,
                webhook.outcome = "delivered",
                http.status = response.status().as_u16(),
                "webhook_delivery_finished"
            );
            metrics::counter!(
                WEBHOOK_DELIVERY_OUTCOMES_METRIC,
                "endpoint" => endpoint_id.to_owned(),
                "outcome" => "delivered"
            )
            .increment(1);
            Ok(())
        }
        Ok(response) if response.status() == StatusCode::GONE => {
            Err(fail(endpoint_id, DeliveryFailure::EndpointGone))
        }
        Ok(response) => {
            let failure = DeliveryFailure::Status(response.status());
            record_failure(endpoint_id, failure);
            // The capped Retry-After is far inside the jobs delay limit; an
            // invalid delay would still fall back to jobs backoff.
            Err(retry_after(response.headers(), response_time)
                .and_then(|delay| JobError::retry_after_at_least(failure, delay).ok())
                .unwrap_or_else(|| JobError::retryable(failure)))
        }
        Err(error) => Err(fail(
            endpoint_id,
            DeliveryFailure::Exchange(exchange_reason(&error)),
        )),
    }
}

fn fail(endpoint_id: &str, failure: DeliveryFailure) -> JobError {
    record_failure(endpoint_id, failure);
    if failure.is_permanent() {
        JobError::permanent(failure)
    } else {
        JobError::retryable(failure)
    }
}

fn record_failure(endpoint_id: &str, failure: DeliveryFailure) {
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
    if matches!(failure, DeliveryFailure::MissingEndpoint) {
        // The ID is no longer configured, so it is not a label.
        metrics::counter!(
            WEBHOOK_DELIVERY_OUTCOMES_METRIC,
            "outcome" => outcome,
            "reason" => reason
        )
        .increment(1);
    } else {
        metrics::counter!(
            WEBHOOK_DELIVERY_OUTCOMES_METRIC,
            "endpoint" => endpoint_id.to_owned(),
            "outcome" => outcome,
            "reason" => reason
        )
        .increment(1);
    }
}

fn retry_after(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?;
    let delay = if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        // An over-long delta saturates to the cap instead of being ignored.
        Duration::from_secs(value.parse().unwrap_or(u64::MAX))
    } else {
        httpdate::parse_http_date(value)
            .ok()?
            .duration_since(now)
            .ok()?
    };
    (!delay.is_zero()).then(|| delay.min(RETRY_AFTER_CAP))
}

fn unix_timestamp(now: SystemTime) -> Option<i64> {
    i64::try_from(now.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()
}

#[cfg(test)]
mod tests {
    use metrics_util::debugging::DebuggingRecorder;
    use std::assert_matches;
    use std::time::{Duration, UNIX_EPOCH};

    use http::{HeaderMap, HeaderValue, header};

    use super::{Endpoint, EndpointError, RETRY_AFTER_CAP, retry_after};
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
        // Endpoint URLs are trusted operator configuration; the outbound client
        // is not an SSRF boundary and does not classify addresses.
        assert!(Endpoint::try_new("https://10.0.0.5/events", ring()).is_ok());
    }

    #[test]
    fn serialized_payload_is_endpoint_content_type_and_base64_body() {
        let delivery = super::Delivery {
            version: super::delivery_version(),
            endpoint_id: "partner".to_owned(),
            content_type: "application/json".to_owned(),
            body: bytes::Bytes::from_static(b"raw\0bytes"),
        };

        assert_eq!(
            serde_json::to_value(&delivery).unwrap(),
            serde_json::json!({
                "version": 2,
                "endpoint_id": "partner",
                "content_type": "application/json",
                "body": "cmF3AGJ5dGVz"
            })
        );
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
            r#"{"version":99,"endpoint_id":"partner","content_type":"application/json","body":"cmF3AGJ5dGVz"}"#,
        )
        .unwrap();

        assert_eq!(delivery.endpoint_id, "partner");
        assert_eq!(delivery.content_type, "application/json");
        assert_eq!(delivery.body.as_ref(), b"raw\0bytes");
    }

    #[test]
    fn retry_after_uses_a_capped_delta_or_future_http_date() {
        for value in [
            "90000",
            "18446744073709551616",
            "999999999999999999999999999999999",
        ] {
            let mut delta = HeaderMap::new();
            delta.insert(header::RETRY_AFTER, HeaderValue::from_static(value));
            assert_eq!(retry_after(&delta, UNIX_EPOCH), Some(RETRY_AFTER_CAP));
        }
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
        let response = |status: u16| {
            Ok(http::Response::builder()
                .status(status)
                .body(bytes::Bytes::new())
                .unwrap())
        };
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            for exchange in [
                response(204),
                response(503),
                response(410),
                Err(super::HttpError::Timeout),
            ] {
                let _ = super::classify_response("partner", exchange, UNIX_EPOCH);
            }
        });
        let labels: Vec<Vec<(String, String)>> = snapshotter
            .snapshot()
            .into_vec()
            .iter()
            .map(|(key, ..)| {
                let key = key.key();
                assert_eq!(key.name(), super::WEBHOOK_DELIVERY_OUTCOMES_METRIC);
                key.labels()
                    .map(|label| (label.key().to_owned(), label.value().to_owned()))
                    .collect()
            })
            .collect();
        let failure = |outcome: &str, reason: &str| {
            vec![
                ("endpoint".to_owned(), "partner".to_owned()),
                ("outcome".to_owned(), outcome.to_owned()),
                ("reason".to_owned(), reason.to_owned()),
            ]
        };
        assert_eq!(
            labels,
            [
                vec![
                    ("endpoint".to_owned(), "partner".to_owned()),
                    ("outcome".to_owned(), "delivered".to_owned()),
                ],
                failure("retryable", "response_status"),
                failure("permanent", "endpoint_gone"),
                failure("retryable", "timeout"),
            ]
        );
    }
}
