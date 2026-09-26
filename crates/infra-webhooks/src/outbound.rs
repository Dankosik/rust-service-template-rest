//! Durable Standard Webhooks delivery through the existing jobs and HTTPS owners.
//!
//! Producers retain immutable bytes and endpoint identity before their business
//! transaction. Each attempt uses the worker startup snapshot for routing and
//! signing, then performs one bounded exchange.

use std::{
    collections::BTreeMap,
    fmt,
    num::NonZeroU32,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, header};
use infra_jobs::{
    EnqueueOptions, Enqueued, Job, JobError, JobId, JobKind, Kinds, MAX_PAYLOAD_BYTES, Policy,
    enqueue,
};
use infra_outbound_http::{Client, Error as HttpError, Limits, Operation};
use infra_postgres::Tx;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::protocol::{KeyRing, MAX_BODY_BYTES};

const DELIVERY_KIND: &str = "webhooks.deliver";
const DELIVERY_VERSION: u8 = 2;
const RETRY_AFTER_CAP: Duration = Duration::from_hours(24);
const DEFAULT_CONTENT_TYPE: &str = "application/json";
const RESPONSE_HEADER_COUNT: usize = 64;
const RESPONSE_BODY_BYTES: usize = 64 * 1024;

/// Jobs policy for one outbound delivery attempt.
pub const DELIVERY_POLICY: Policy = Policy {
    max_attempts: 20,
    timeout: Duration::from_secs(30),
};

/// Static, non-secret properties of one outbound endpoint.
#[derive(Clone)]
pub struct Endpoint {
    destination: String,
}

impl Endpoint {
    /// Build one static endpoint binding.
    #[must_use]
    pub fn new(destination: String) -> Self {
        Self { destination }
    }
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Endpoint([REDACTED])")
    }
}

/// Producer-side bindings for configured outbound webhook endpoints.
#[derive(Clone)]
pub struct Outbound {
    endpoints: BTreeMap<String, Url>,
}

impl fmt::Debug for Outbound {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Outbound([REDACTED])")
    }
}

impl Outbound {
    /// Admit the static endpoint bindings without performing network I/O.
    ///
    /// # Errors
    ///
    /// Returns a closed configuration error for an unusable endpoint identity
    /// or destination.
    pub fn new(endpoints: BTreeMap<String, Endpoint>) -> Result<Self, OutboundError> {
        let endpoints = endpoints
            .into_iter()
            .map(|(endpoint_id, endpoint)| {
                validate_endpoint_id(&endpoint_id)?;
                let destination = parse_destination(&endpoint.destination)?;
                // Admission never resolves or contacts a receiver.
                Client::new(&origin(&destination)?, limits(NonZeroU32::MIN)?)
                    .map_err(OutboundError::Client)?;
                Ok((endpoint_id, destination))
            })
            .collect::<Result<_, OutboundError>>()?;
        Ok(Self { endpoints })
    }

    /// Prepare immutable work for one configured endpoint before a transaction.
    ///
    /// The caller supplies final raw bytes and an optional content type. An
    /// absent content type is the Standard Webhooks template default
    /// `application/json`.
    ///
    /// # Errors
    ///
    /// Returns a closed error before any job insert for an unknown endpoint,
    /// oversized body, or unusable content type.
    pub fn prepare(
        &self,
        endpoint_id: &str,
        body: Vec<u8>,
        content_type: Option<String>,
    ) -> Result<PreparedDelivery, OutboundError> {
        if body.len() > MAX_BODY_BYTES {
            return Err(OutboundError::BodyTooLarge);
        }
        if !self.endpoints.contains_key(endpoint_id) {
            return Err(OutboundError::UnknownEndpoint);
        }
        let content_type = content_type.unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_owned());
        let content_type = HeaderValue::from_str(&content_type)
            .ok()
            .and_then(|value| value.to_str().ok().map(str::to_owned))
            .ok_or(OutboundError::InvalidContentType)?;
        let delivery = Delivery {
            version: DELIVERY_VERSION,
            endpoint_id: endpoint_id.to_owned(),
            content_type,
            body,
        };
        let serialized = serde_json::to_vec(&delivery).map_err(OutboundError::Serialize)?;
        if serialized.len() > MAX_PAYLOAD_BYTES {
            return Err(OutboundError::PayloadTooLarge {
                bytes: serialized.len(),
            });
        }
        Ok(PreparedDelivery { delivery })
    }

    /// Construct a fixed endpoint/client/key snapshot before claiming jobs.
    ///
    /// `max_workers` is the jobs worker capacity. Each endpoint client admits
    /// that many exchanges, so a local attempt never fails for capacity; the
    /// jobs slots are the only in-process concurrency bound.
    ///
    /// # Errors
    ///
    /// Every configured endpoint must have a decoded signing ring and an
    /// admitted transport client.
    pub fn dispatcher(
        &self,
        keys: BTreeMap<String, KeyRing>,
        max_workers: NonZeroU32,
    ) -> Result<Dispatcher, OutboundError> {
        let limits = limits(max_workers)?;
        self.build_dispatcher(keys, |destination| {
            Client::new(&origin(destination)?, limits).map_err(OutboundError::Client)
        })
    }

    /// Construct the same dispatcher for the fixed metadata fixture and local HTTP peer.
    ///
    /// # Errors
    ///
    /// Rejects missing rings, non-fixture destinations or non-loopback HTTP peers.
    #[cfg(feature = "test-support")]
    pub fn dispatcher_for_test_http(
        &self,
        keys: BTreeMap<String, KeyRing>,
        max_workers: NonZeroU32,
        socket: std::net::SocketAddr,
    ) -> Result<Dispatcher, OutboundError> {
        let limits = limits(max_workers)?;
        self.build_dispatcher(keys, |destination| {
            if destination.host_str() != Some("authn.fixture.test")
                || destination.port_or_known_default() != Some(443)
            {
                return Err(OutboundError::InvalidEndpoint);
            }
            Client::new_for_test_http(&format!("http://{socket}/"), limits)
                .map_err(OutboundError::Client)
        })
    }

    fn build_dispatcher(
        &self,
        mut keys: BTreeMap<String, KeyRing>,
        make_client: impl Fn(&Url) -> Result<Client, OutboundError>,
    ) -> Result<Dispatcher, OutboundError> {
        let endpoints = self
            .endpoints
            .iter()
            .map(|(endpoint_id, destination)| {
                let keys = keys
                    .remove(endpoint_id)
                    .ok_or(OutboundError::MissingKeyRing)?;
                let client = make_client(destination)?;
                Ok((
                    endpoint_id.clone(),
                    Binding {
                        destination: destination.clone(),
                        client,
                        keys,
                    },
                ))
            })
            .collect::<Result<_, OutboundError>>()?;
        Ok(Dispatcher { endpoints })
    }
}

/// Prepared, one-transaction delivery work.
#[derive(Clone)]
pub struct PreparedDelivery {
    delivery: Delivery,
}

impl fmt::Debug for PreparedDelivery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedDelivery([REDACTED])")
    }
}

impl PreparedDelivery {
    /// Enqueue this delivery on the caller's already-open transaction.
    ///
    /// The returned job ID is the stable Standard Webhooks message ID. A caller
    /// must propagate any error so its accompanying business transaction rolls
    /// back rather than claiming durable delivery ownership.
    ///
    /// # Errors
    ///
    /// Returns an enqueue failure from the existing jobs owner. This delivery
    /// has no unique key, so an unexpected duplicate is closed as an adapter
    /// error rather than silently treated as accepted work.
    pub async fn enqueue(&self, tx: &mut Tx<'_>) -> Result<JobId, OutboundError> {
        match enqueue(tx, &self.delivery, EnqueueOptions::default())
            .await
            .map_err(OutboundError::Enqueue)?
        {
            Enqueued::Created(id) => Ok(id),
            Enqueued::Duplicate => Err(OutboundError::UnexpectedDuplicate),
        }
    }
}

/// Immutable current endpoint destinations, clients and signing keys.
#[derive(Clone)]
pub struct Dispatcher {
    endpoints: BTreeMap<String, Binding>,
}

#[derive(Clone)]
struct Binding {
    destination: Url,
    client: Client,
    keys: KeyRing,
}

impl Dispatcher {
    /// Register `webhooks.deliver` with the existing jobs kind registry.
    ///
    /// The dispatcher is consumed into the registered handler; no webhook task
    /// or retry loop is spawned.
    pub fn register(self, kinds: &mut Kinds) -> &mut Kinds {
        let dispatcher = Arc::new(self);
        kinds.register(DELIVERY_POLICY, move |job| {
            let dispatcher = Arc::clone(&dispatcher);
            async move { dispatcher.dispatch(job).await }
        })
    }

    async fn dispatch(&self, job: Job<QueuedDelivery>) -> Result<(), JobError> {
        let QueuedDelivery::Valid(delivery) = job.payload() else {
            return Err(JobError::permanent(DeliveryOutcome::InvalidPayload));
        };
        if !matches!(delivery.version, 1 | DELIVERY_VERSION)
            || validate_endpoint_id(&delivery.endpoint_id).is_err()
            || delivery.body.len() > MAX_BODY_BYTES
            || !HeaderValue::from_str(&delivery.content_type)
                .is_ok_and(|value| value.to_str().is_ok())
        {
            return Err(JobError::permanent(DeliveryOutcome::InvalidPayload));
        }
        if delivery.version == 1 {
            tracing::debug!(
                webhook.reason = "legacy_payload",
                "webhook_delivery_legacy_payload"
            );
        }
        let Some(binding) = self.endpoints.get(&delivery.endpoint_id) else {
            tracing::info!(
                webhook.outcome = "retryable",
                webhook.reason = "missing_endpoint",
                "webhook_delivery_finished"
            );
            return Err(JobError::retryable(DeliveryOutcome::MissingEndpoint));
        };
        let timestamp = unix_timestamp(SystemTime::now())
            .ok_or_else(|| JobError::retryable(DeliveryOutcome::ClockUnavailable))?;
        let message_id = job.id().to_string();
        let signature = binding
            .keys
            .signatures(message_id.as_bytes(), timestamp, &delivery.body)
            .map_err(|_| JobError::permanent(DeliveryOutcome::InvalidPayload))?;
        let request = request(
            delivery,
            &binding.destination,
            &message_id,
            timestamp,
            &signature,
        )
        .map_err(|_| JobError::permanent(DeliveryOutcome::InvalidPayload))?;
        let response = binding
            .client
            .execute(
                request,
                Operation {
                    deadline: job.deadline(),
                    response_body_bytes: Some(RESPONSE_BODY_BYTES),
                },
            )
            .await;
        classify_response(response, SystemTime::now())
    }
}

impl fmt::Debug for Dispatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Dispatcher([REDACTED])")
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Delivery {
    version: u8,
    endpoint_id: String,
    content_type: String,
    #[serde(with = "base64_body")]
    body: Vec<u8>,
}

impl JobKind for Delivery {
    const NAME: &'static str = DELIVERY_KIND;
}

// The jobs engine retries typed-deserialization errors. Decode malformed common
// fields here as an explicit variant so the webhook owner can reject them
// permanently; obsolete v1 routing fields remain ignored by Delivery.
#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum QueuedDelivery {
    Valid(Delivery),
    #[serde(skip_serializing)]
    Invalid(serde::de::IgnoredAny),
}

impl JobKind for QueuedDelivery {
    const NAME: &'static str = DELIVERY_KIND;
}

const _: () = infra_jobs::assert_valid_kind_name(DELIVERY_KIND);

impl fmt::Debug for Delivery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Delivery([REDACTED])")
    }
}

/// Why producer preparation or enqueueing did not establish durable work.
#[derive(Debug, thiserror::Error)]
pub enum OutboundError {
    /// The producer selected no configured endpoint.
    #[error("outbound webhook endpoint is not configured")]
    UnknownEndpoint,
    /// An endpoint identity, URL, or transport construction is invalid.
    #[error("outbound webhook endpoint configuration is invalid")]
    InvalidEndpoint,
    /// The producer body is larger than the wire capability permits.
    #[error("outbound webhook body exceeds the maximum size")]
    BodyTooLarge,
    /// The supplied content type cannot become an HTTP field value.
    #[error("outbound webhook content type is invalid")]
    InvalidContentType,
    /// The jobs owner rejected or could not insert the delivery.
    #[error("outbound webhook enqueue failed")]
    Enqueue(#[source] infra_jobs::EnqueueError),
    /// The queued JSON representation could not be prepared.
    #[error("outbound webhook payload could not be serialized")]
    Serialize(#[source] serde_json::Error),
    /// The prepared queued JSON exceeds the existing jobs payload limit.
    #[error("outbound webhook payload is {bytes} bytes, above the jobs maximum")]
    PayloadTooLarge {
        /// Serialized JSON bytes that would have been enqueued.
        bytes: usize,
    },
    /// A no-unique-key delivery unexpectedly received a duplicate result.
    #[error("outbound webhook enqueue unexpectedly deduplicated")]
    UnexpectedDuplicate,
    /// A configured endpoint has no current signing ring.
    #[error("outbound webhook endpoint signing ring is missing")]
    MissingKeyRing,
    /// Existing fixed-authority client construction failed.
    #[error("outbound webhook client configuration is invalid")]
    Client(#[source] HttpError),
}

#[derive(Clone, Copy)]
enum DeliveryOutcome {
    InvalidPayload,
    MissingEndpoint,
    ClockUnavailable,
    Retryable,
    EndpointGone,
    TransportUncertain,
}

impl fmt::Display for DeliveryOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let outcome = match self {
            Self::InvalidPayload => "invalid_payload",
            Self::MissingEndpoint => "missing_endpoint",
            Self::ClockUnavailable => "clock_unavailable",
            Self::Retryable => "retryable_response",
            Self::EndpointGone => "endpoint_gone",
            Self::TransportUncertain => "transport_uncertain",
        };
        formatter.write_str(outcome)
    }
}

fn limits(max_workers: NonZeroU32) -> Result<Limits, OutboundError> {
    let max_active =
        usize::try_from(max_workers.get()).map_err(|_| OutboundError::InvalidEndpoint)?;
    Ok(Limits {
        max_active,
        operation_timeout: DELIVERY_POLICY.timeout,
        response_header_count: RESPONSE_HEADER_COUNT,
        response_body_bytes: RESPONSE_BODY_BYTES,
    })
}

fn validate_endpoint_id(endpoint_id: &str) -> Result<(), OutboundError> {
    if endpoint_id.is_empty() || endpoint_id.contains('\0') {
        return Err(OutboundError::InvalidEndpoint);
    }
    Ok(())
}

fn parse_destination(raw: &str) -> Result<Url, OutboundError> {
    let destination = Url::parse(raw).map_err(|_| OutboundError::InvalidEndpoint)?;
    if destination.scheme() != "https"
        || destination.host().is_none()
        || !destination.username().is_empty()
        || destination.password().is_some()
        || destination.fragment().is_some()
    {
        return Err(OutboundError::InvalidEndpoint);
    }
    Ok(destination)
}

fn origin(destination: &Url) -> Result<String, OutboundError> {
    let mut origin = destination.clone();
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    origin
        .set_username("")
        .map_err(|()| OutboundError::InvalidEndpoint)?;
    origin
        .set_password(None)
        .map_err(|()| OutboundError::InvalidEndpoint)?;
    Ok(origin.to_string())
}

fn request(
    delivery: &Delivery,
    destination: &Url,
    message_id: &str,
    timestamp: i64,
    signature: &str,
) -> Result<Request<Bytes>, OutboundError> {
    let target = origin_form(destination)?;
    Request::builder()
        .method(Method::POST)
        .uri(target)
        .header(header::CONTENT_TYPE, &delivery.content_type)
        .header("webhook-id", message_id)
        .header("webhook-timestamp", timestamp.to_string())
        .header("webhook-signature", signature)
        .body(Bytes::copy_from_slice(&delivery.body))
        .map_err(|_| OutboundError::InvalidEndpoint)
}

fn origin_form(destination: &Url) -> Result<Uri, OutboundError> {
    let mut target = destination.path().to_owned();
    if target.is_empty() {
        target.push('/');
    }
    if let Some(query) = destination.query() {
        target.push('?');
        target.push_str(query);
    }
    target.parse().map_err(|_| OutboundError::InvalidEndpoint)
}

fn classify_response(
    response: Result<http::Response<Bytes>, HttpError>,
    response_time: SystemTime,
) -> Result<(), JobError> {
    match response {
        Ok(response) if response.status().is_success() => {
            tracing::debug!(
                webhook.outcome = "delivered",
                http.status = response.status().as_u16(),
                "webhook_delivery_finished"
            );
            Ok(())
        }
        Ok(response) if response.status() == StatusCode::GONE => {
            tracing::warn!(
                webhook.outcome = "permanent",
                webhook.reason = "endpoint_gone",
                "webhook endpoint returned 410; disable or remove its static binding and restart workers"
            );
            Err(JobError::permanent(DeliveryOutcome::EndpointGone))
        }
        Ok(response) => {
            tracing::info!(
                webhook.outcome = "retryable",
                http.status = response.status().as_u16(),
                "webhook_delivery_finished"
            );
            if let Some(delay) = retry_after(response.headers(), response_time) {
                return Err(JobError::retry_after_at_least(
                    DeliveryOutcome::Retryable,
                    delay,
                )?);
            }
            Err(JobError::retryable(DeliveryOutcome::Retryable))
        }
        Err(_) => {
            tracing::info!(
                webhook.outcome = "retryable",
                webhook.reason = "transport_uncertain",
                "webhook_delivery_finished"
            );
            Err(JobError::retryable(DeliveryOutcome::TransportUncertain))
        }
    }
}

fn retry_after(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    let mut values = headers.get_all(header::RETRY_AFTER).iter();
    let first = values.next()?;
    if values.any(|value| value.as_bytes() != first.as_bytes()) {
        return None;
    }
    let value = first.to_str().ok()?;
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        let seconds = value.bytes().fold(0_u64, |seconds, byte| {
            seconds
                .saturating_mul(10)
                .saturating_add(u64::from(byte - b'0'))
        });
        return Some(Duration::from_secs(seconds).min(RETRY_AFTER_CAP));
    }
    let at = httpdate::parse_http_date(value).ok()?;
    at.duration_since(now)
        .ok()
        .filter(|delay| !delay.is_zero())
        .map(|delay| delay.min(RETRY_AFTER_CAP))
}

fn unix_timestamp(now: SystemTime) -> Option<i64> {
    i64::try_from(now.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()
}

mod base64_body {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub(super) fn serialize<S>(body: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(body))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        num::NonZeroU32,
        time::{Duration, UNIX_EPOCH},
    };

    use http::{HeaderMap, HeaderValue, header};
    use infra_jobs::MAX_PAYLOAD_BYTES;

    use super::{
        DEFAULT_CONTENT_TYPE, Endpoint, Outbound, OutboundError, RETRY_AFTER_CAP, retry_after,
    };
    use crate::protocol::{KeyRing, MAX_BODY_BYTES};

    fn outbound(destination: &str) -> Outbound {
        outbound_with("partner", destination)
    }

    fn outbound_with(endpoint_id: &str, destination: &str) -> Outbound {
        Outbound::new(BTreeMap::from([(
            endpoint_id.to_owned(),
            Endpoint::new(destination.to_owned()),
        )]))
        .unwrap()
    }

    #[test]
    fn prepares_final_raw_bytes_with_the_default_content_type() {
        let prepared = outbound("https://partner.example/events?source=template")
            .prepare("partner", b"raw\0bytes".to_vec(), None)
            .unwrap();

        assert_eq!(prepared.delivery.body, b"raw\0bytes");
        assert_eq!(prepared.delivery.content_type, DEFAULT_CONTENT_TYPE);
        assert_eq!(
            serde_json::to_value(&prepared.delivery).unwrap(),
            serde_json::json!({
                "version": 2,
                "endpoint_id": "partner",
                "content_type": "application/json",
                "body": "cmF3AGJ5dGVz"
            })
        );
    }

    #[test]
    fn dispatcher_requires_a_current_ring_for_every_configured_endpoint() {
        let outbound = outbound("https://partner.example/events");
        assert!(matches!(
            outbound.dispatcher(BTreeMap::new(), NonZeroU32::MIN),
            Err(OutboundError::MissingKeyRing)
        ));
        let ring =
            KeyRing::from_encoded("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=", None).unwrap();
        assert!(
            outbound
                .dispatcher(
                    BTreeMap::from([("partner".to_owned(), ring)]),
                    NonZeroU32::MIN
                )
                .is_ok()
        );
    }

    #[test]
    fn rejects_an_unknown_endpoint_before_enqueue() {
        let error = outbound("https://partner.example/events")
            .prepare("absent", Vec::new(), None)
            .unwrap_err();
        assert!(matches!(error, OutboundError::UnknownEndpoint));
    }

    #[test]
    fn rejects_credentials_fragments_and_non_https_static_destinations() {
        for destination in [
            "https://user@partner.example/events",
            "https://partner.example/events#fragment",
            "http://partner.example/events",
        ] {
            assert!(
                Outbound::new(BTreeMap::from([(
                    "partner".to_owned(),
                    Endpoint::new(destination.to_owned()),
                )]),)
                .is_err()
            );
        }
    }

    #[test]
    fn admits_operator_configured_private_https_destinations() {
        // Endpoint URLs are trusted operator configuration; the outbound client
        // is not an SSRF boundary and does not classify addresses.
        assert!(
            Outbound::new(BTreeMap::from([(
                "partner".to_owned(),
                Endpoint::new("https://10.0.0.5/events".to_owned()),
            )]),)
            .is_ok()
        );
    }

    #[test]
    fn refuses_a_prepared_delivery_that_exceeds_the_existing_jobs_payload_limit() {
        let endpoint_id = "e".repeat(90_000);
        let outbound = outbound_with(&endpoint_id, "https://partner.example/events");
        let error = outbound
            .prepare(&endpoint_id, vec![0; MAX_BODY_BYTES], None)
            .unwrap_err();

        assert!(
            matches!(error, OutboundError::PayloadTooLarge { bytes } if bytes > MAX_PAYLOAD_BYTES),
            "{error:?}"
        );
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
        for value in ["+3600", "-1", "1.5", "", " 3600"] {
            let mut invalid = HeaderMap::new();
            invalid.insert(header::RETRY_AFTER, HeaderValue::from_static(value));
            assert_eq!(retry_after(&invalid, UNIX_EPOCH), None);
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
    fn conflicting_or_elapsed_retry_after_falls_back_to_jobs_backoff() {
        let mut conflicting = HeaderMap::new();
        conflicting.append(header::RETRY_AFTER, HeaderValue::from_static("1"));
        conflicting.append(header::RETRY_AFTER, HeaderValue::from_static("2"));
        assert_eq!(retry_after(&conflicting, UNIX_EPOCH), None);

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
}
