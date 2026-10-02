//! Durable Standard Webhooks delivery through the existing jobs and HTTPS owners.
//!
//! Producers enqueue final bytes for a configured endpoint ID inside their business
//! transaction. Each attempt uses the worker startup snapshot of endpoints for routing
//! and signing, then performs one bounded exchange.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    num::NonZeroU32,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use infra_jobs::{EnqueueOptions, Enqueued, Job, JobError, JobId, JobKind, Kinds, Policy};
use infra_outbound_http::{BuildError, Client, Error as HttpError, Limits};
use infra_postgres::Tx;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use url::Url;

use crate::protocol::{KeyRing, MAX_BODY_BYTES};

const DELIVERY_KIND: &str = "webhooks.deliver";
const RETRY_AFTER_CAP: Duration = Duration::from_hours(24);
const DEFAULT_CONTENT_TYPE: &str = "application/json";
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

/// Producer side: the configured endpoint IDs a delivery may target.
#[derive(Clone, Debug)]
pub struct Outbound {
    endpoint_ids: BTreeSet<String>,
}

impl Outbound {
    /// No I/O. Endpoint ID syntax is owned by configuration validation.
    #[must_use]
    pub fn new(endpoint_ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            endpoint_ids: endpoint_ids.into_iter().collect(),
        }
    }

    /// Validate, then enqueue one delivery on the caller's open transaction.
    ///
    /// Returns the job ID, which is the Standard Webhooks message ID.
    /// Validation failures happen before any insert. `content_type: None`
    /// means `application/json`.
    ///
    /// # Errors
    ///
    /// Returns a closed error for an unknown endpoint, a body over
    /// [`MAX_BODY_BYTES`], a content type that is not a visible-ASCII header
    /// value, or an enqueue failure. This delivery has no unique key, so an
    /// unexpected duplicate is closed as an adapter error.
    pub async fn enqueue(
        &self,
        tx: &mut Tx<'_>,
        endpoint_id: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<JobId, OutboundError> {
        if body.len() > MAX_BODY_BYTES {
            return Err(OutboundError::BodyTooLarge);
        }
        if !self.endpoint_ids.contains(endpoint_id) {
            return Err(OutboundError::UnknownEndpoint);
        }
        let content_type = content_type.unwrap_or(DEFAULT_CONTENT_TYPE);
        if HeaderValue::from_str(content_type).is_err() {
            return Err(OutboundError::InvalidContentType);
        }
        let delivery = Delivery {
            version: delivery_version(),
            endpoint_id: endpoint_id.to_owned(),
            content_type: content_type.to_owned(),
            body: Bytes::from(body),
        };
        match infra_jobs::enqueue(tx, &delivery, EnqueueOptions::default())
            .await
            .map_err(OutboundError::Enqueue)?
        {
            Enqueued::Created(id) => Ok(id),
            Enqueued::Duplicate => Err(OutboundError::UnexpectedDuplicate),
        }
    }
}

/// Worker side: one endpoint's destination, transport client and signing keys.
#[derive(Clone)]
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
    pub fn new(destination: &str, keys: KeyRing) -> Result<Self, EndpointError> {
        let destination = parse_destination(destination)?;
        let client = Client::new(&destination, LIMITS).map_err(EndpointError::Client)?;
        Ok(Self {
            destination,
            client,
            keys,
        })
    }

    /// Use a caller-built local test client. Production code must use
    /// [`Endpoint::new`].
    ///
    /// The caller supplies the absolute destination admitted by its client.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::InvalidDestination`] when the destination cannot
    /// be represented as an HTTP request URI.
    #[cfg(feature = "test-support")]
    pub fn with_client(
        destination: Url,
        client: Client,
        keys: KeyRing,
    ) -> Result<Self, EndpointError> {
        if !matches!(destination.scheme(), "http" | "https")
            || destination.host().is_none()
            || !destination.username().is_empty()
            || destination.password().is_some()
            || destination.fragment().is_some()
        {
            return Err(EndpointError::InvalidDestination);
        }
        Ok(Self {
            destination,
            client,
            keys,
        })
    }
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Endpoint([REDACTED])")
    }
}

/// Delivery handler over the worker's startup snapshot of endpoints.
pub struct Dispatcher {
    endpoints: BTreeMap<String, Endpoint>,
}

impl Dispatcher {
    /// Build a dispatcher from an already admitted endpoint snapshot.
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
    pub fn register(self, kinds: &mut Kinds, max_concurrent: Option<NonZeroU32>) -> &mut Kinds {
        metrics::describe_counter!(
            WEBHOOK_DELIVERY_OUTCOMES_METRIC,
            metrics::Unit::Count,
            "Outbound webhook delivery attempts by result."
        );
        let dispatcher = Arc::new(self);
        let policy = Policy {
            max_running: max_concurrent,
            ..DELIVERY_POLICY
        };
        kinds.register(policy, move |job| {
            let dispatcher = Arc::clone(&dispatcher);
            async move { dispatcher.dispatch(job).await }
        })
    }

    async fn dispatch(&self, job: Job<Delivery>) -> Result<(), JobError> {
        let delivery = job.payload();
        let endpoint_id = delivery.endpoint_id.as_str();
        let Some(endpoint) = self.endpoints.get(endpoint_id) else {
            tracing::info!(
                webhook.endpoint = endpoint_id,
                webhook.outcome = "retryable",
                webhook.reason = "missing_endpoint",
                "webhook_delivery_finished"
            );
            // The ID is no longer configured, so it is not a label.
            metrics::counter!(
                WEBHOOK_DELIVERY_OUTCOMES_METRIC,
                "outcome" => "retryable",
                "reason" => "missing_endpoint"
            )
            .increment(1);
            return Err(JobError::retryable(DeliveryOutcome::MissingEndpoint));
        };
        let Some(timestamp) = unix_timestamp(SystemTime::now()) else {
            count_failure(endpoint_id, "retryable", "clock_unavailable");
            return Err(JobError::retryable(DeliveryOutcome::ClockUnavailable));
        };
        let message_id = job.id().to_string();
        let request = endpoint
            .keys
            .signatures(message_id.as_bytes(), timestamp, &delivery.body)
            .ok()
            .and_then(|signature| {
                request(
                    delivery,
                    &endpoint.destination,
                    &message_id,
                    timestamp,
                    &signature,
                )
                .ok()
            });
        let Some(request) = request else {
            tracing::warn!(
                webhook.endpoint = endpoint_id,
                webhook.outcome = "permanent",
                webhook.reason = "invalid_payload",
                "webhook_delivery_finished"
            );
            count_failure(endpoint_id, "permanent", "invalid_payload");
            return Err(JobError::permanent(DeliveryOutcome::InvalidPayload));
        };
        let response = endpoint.client.execute(request, job.deadline()).await;
        classify_response(endpoint_id, response, SystemTime::now())
    }
}

impl fmt::Debug for Dispatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Dispatcher([REDACTED])")
    }
}

// A payload the worker cannot decode is retried by infra-jobs, which keeps a
// rolling deploy safe. Do not convert decode errors to permanent failures.
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

impl JobKind for Delivery {
    const NAME: &'static str = DELIVERY_KIND;
}

const _: () = infra_jobs::assert_valid_kind_name(DELIVERY_KIND);

/// Why producer enqueueing did not establish durable work.
#[derive(Debug, thiserror::Error)]
pub enum OutboundError {
    /// The producer selected no configured endpoint.
    #[error("outbound webhook endpoint is not configured")]
    UnknownEndpoint,
    /// The producer body is larger than the wire capability permits.
    #[error("outbound webhook body exceeds the maximum size")]
    BodyTooLarge,
    /// The supplied content type cannot become an HTTP field value.
    #[error("outbound webhook content type is invalid")]
    InvalidContentType,
    /// The jobs owner rejected or could not insert the delivery.
    #[error("outbound webhook enqueue failed")]
    Enqueue(#[source] infra_jobs::EnqueueError),
    /// A no-unique-key delivery unexpectedly received a duplicate result.
    #[error("outbound webhook enqueue unexpectedly deduplicated")]
    UnexpectedDuplicate,
}

/// Why a worker could not build one configured endpoint.
#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    /// The destination is not an HTTPS URL with a host, or carries
    /// credentials or a fragment.
    #[error("outbound webhook destination is invalid")]
    InvalidDestination,
    /// Existing fixed-authority client construction failed.
    #[error("outbound webhook client configuration is invalid")]
    Client(#[source] BuildError),
}

#[derive(Clone, Copy)]
enum DeliveryOutcome {
    InvalidPayload,
    MissingEndpoint,
    ClockUnavailable,
    Status(StatusCode),
    EndpointGone,
    Transport(&'static str),
}

/// The summary jobs keeps in a delivery's failure history.
impl fmt::Display for DeliveryOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPayload => formatter.write_str("invalid_payload"),
            Self::MissingEndpoint => formatter.write_str("missing_endpoint"),
            Self::ClockUnavailable => formatter.write_str("clock_unavailable"),
            Self::Status(status) => write!(formatter, "response_status_{}", status.as_u16()),
            Self::EndpointGone => formatter.write_str("endpoint_gone"),
            Self::Transport(reason) => formatter.write_str(reason),
        }
    }
}

/// Why an exchange produced no response. The outcome is uncertain in every
/// case: the receiver may have processed the request.
const fn transport_reason(error: &HttpError) -> &'static str {
    match error {
        HttpError::Timeout => "timeout",
        HttpError::ResponseBodyTooLarge => "response_too_large",
        HttpError::Transport { .. } => "transport",
        HttpError::InvalidTarget => "client",
    }
}

fn parse_destination(raw: &str) -> Result<Url, EndpointError> {
    let destination = Url::parse(raw).map_err(|_| EndpointError::InvalidDestination)?;
    if destination.scheme() != "https"
        || destination.host().is_none()
        || !destination.username().is_empty()
        || destination.password().is_some()
        || destination.fragment().is_some()
    {
        return Err(EndpointError::InvalidDestination);
    }
    Ok(destination)
}

fn request(
    delivery: &Delivery,
    destination: &Url,
    message_id: &str,
    timestamp: i64,
    signature: &str,
) -> Result<Request<Bytes>, http::Error> {
    Request::builder()
        .method(Method::POST)
        .uri(destination.as_str())
        .header(header::CONTENT_TYPE, &delivery.content_type)
        .header("webhook-id", message_id)
        .header("webhook-timestamp", timestamp.to_string())
        .header("webhook-signature", signature)
        .body(delivery.body.clone())
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
            // The receiver asks for no more deliveries: disable or remove its
            // static binding and restart workers.
            tracing::warn!(
                webhook.endpoint = endpoint_id,
                webhook.outcome = "permanent",
                webhook.reason = "endpoint_gone",
                http.status = response.status().as_u16(),
                "webhook_delivery_finished"
            );
            count_failure(endpoint_id, "permanent", "endpoint_gone");
            Err(JobError::permanent(DeliveryOutcome::EndpointGone))
        }
        Ok(response) => {
            tracing::info!(
                webhook.endpoint = endpoint_id,
                webhook.outcome = "retryable",
                webhook.reason = "response_status",
                http.status = response.status().as_u16(),
                "webhook_delivery_finished"
            );
            count_failure(endpoint_id, "retryable", "response_status");
            let outcome = DeliveryOutcome::Status(response.status());
            if let Some(delay) = retry_after(response.headers(), response_time) {
                return Err(JobError::retry_after_at_least(outcome, delay)?);
            }
            Err(JobError::retryable(outcome))
        }
        Err(error) => {
            let reason = transport_reason(&error);
            tracing::info!(
                webhook.endpoint = endpoint_id,
                webhook.outcome = "retryable",
                webhook.reason = reason,
                "webhook_delivery_finished"
            );
            count_failure(endpoint_id, "retryable", reason);
            Err(JobError::retryable(DeliveryOutcome::Transport(reason)))
        }
    }
}

fn count_failure(endpoint_id: &str, outcome: &'static str, reason: &'static str) {
    metrics::counter!(
        WEBHOOK_DELIVERY_OUTCOMES_METRIC,
        "endpoint" => endpoint_id.to_owned(),
        "outcome" => outcome,
        "reason" => reason
    )
    .increment(1);
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
    use std::time::{Duration, UNIX_EPOCH};

    use http::{HeaderMap, HeaderValue, header};

    use super::{Endpoint, EndpointError, RETRY_AFTER_CAP, parse_destination, retry_after};
    use crate::protocol::KeyRing;

    fn ring() -> KeyRing {
        KeyRing::from_encoded("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=", None).unwrap()
    }

    #[test]
    fn parse_destination_rejects_credentials_fragments_and_plain_http() {
        for destination in [
            "https://user@partner.example/events",
            "https://partner.example/events#fragment",
            "http://partner.example/events",
        ] {
            assert!(
                parse_destination(destination).is_err(),
                "{destination} must be rejected"
            );
        }
    }

    #[test]
    fn parse_destination_admits_a_private_https_ip_and_endpoint_new_builds() {
        // Endpoint URLs are trusted operator configuration; the outbound client
        // is not an SSRF boundary and does not classify addresses.
        let destination = "https://10.0.0.5/events";
        assert!(parse_destination(destination).is_ok());
        assert!(Endpoint::new(destination, ring()).is_ok());
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

    /// Collects the key of every counter a delivery registers.
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
    fn each_attempt_is_counted_by_endpoint_outcome_and_reason_without_the_status() {
        let response = |status: u16| {
            Ok(http::Response::builder()
                .status(status)
                .body(bytes::Bytes::new())
                .unwrap())
        };
        let keys = Keys::default();
        metrics::with_local_recorder(&keys, || {
            for exchange in [
                response(204),
                response(503),
                response(410),
                Err(super::HttpError::Timeout),
            ] {
                let _ = super::classify_response("partner", exchange, UNIX_EPOCH);
            }
        });
        let labels: Vec<Vec<(String, String)>> = keys
            .0
            .lock()
            .expect("keys")
            .iter()
            .map(|key| {
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

    #[test]
    fn endpoint_construction_failure_is_a_closed_error() {
        assert!(matches!(
            Endpoint::new("http://partner.example/events", ring()),
            Err(EndpointError::InvalidDestination)
        ));
    }
}
