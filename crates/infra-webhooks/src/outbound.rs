//! Durable Standard Webhooks delivery through the existing jobs and HTTPS owners.
//!
//! Producers prepare immutable bytes and endpoint references before their
//! business transaction. The jobs handler resolves those retained references,
//! signs the stable job identity afresh, and performs one bounded exchange.

use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    num::NonZeroU32,
    sync::{Arc, Mutex},
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

use crate::protocol::{KeyRing, MAX_BODY_BYTES, SigningKey};

const DELIVERY_KIND: &str = "webhooks.deliver";
const DELIVERY_VERSION: u8 = 1;
const CACHE_CAPACITY: usize = 64;
const RETRY_AFTER_CAP: Duration = Duration::from_hours(24);
const MISSING_SECRET_DELAY: Duration = Duration::from_secs(60);
const DEFAULT_CONTENT_TYPE: &str = "application/json";
const RESPONSE_HEADER_COUNT: usize = 64;
const RESPONSE_HEADER_BYTES: usize = 16 * 1024;
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
    active_key: String,
    previous_key: Option<String>,
}

impl Endpoint {
    /// Build one static endpoint binding.
    #[must_use]
    pub fn new(destination: String, active_key: String, previous_key: Option<String>) -> Self {
        Self {
            destination,
            active_key,
            previous_key,
        }
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
    endpoints: BTreeMap<String, Endpoint>,
    limits: Limits,
}

impl fmt::Debug for Outbound {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Outbound([REDACTED])")
    }
}

impl Outbound {
    /// Admit the static endpoint bindings without performing network I/O.
    ///
    /// `max_workers` is the jobs worker capacity, reused as the existing
    /// bounded client's active-exchange limit.
    ///
    /// # Errors
    ///
    /// Returns a closed configuration error for an unusable endpoint identity,
    /// key reference, destination, or outbound-client limits.
    pub fn new(
        endpoints: BTreeMap<String, Endpoint>,
        max_workers: NonZeroU32,
    ) -> Result<Self, OutboundError> {
        let limits = limits(max_workers)?;
        for (endpoint_id, endpoint) in &endpoints {
            validate_endpoint_id(endpoint_id)?;
            validate_key_ref(&endpoint.active_key)?;
            if let Some(previous) = &endpoint.previous_key {
                validate_key_ref(previous)?;
                if previous == &endpoint.active_key {
                    return Err(OutboundError::InvalidEndpoint);
                }
            }
            let destination = parse_destination(&endpoint.destination)?;
            // Client construction performs existing fixed-authority admission
            // but does not resolve or contact a receiver.
            Client::new(&origin(&destination)?, limits).map_err(OutboundError::Client)?;
        }
        Ok(Self { endpoints, limits })
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
        let endpoint = self
            .endpoints
            .get(endpoint_id)
            .ok_or(OutboundError::UnknownEndpoint)?;
        let destination = parse_destination(&endpoint.destination)?;
        let content_type = content_type.unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_owned());
        let content_type = HeaderValue::from_str(&content_type)
            .ok()
            .and_then(|value| value.to_str().ok().map(str::to_owned))
            .ok_or(OutboundError::InvalidContentType)?;
        let delivery = Delivery {
            version: DELIVERY_VERSION,
            endpoint_id: endpoint_id.to_owned(),
            destination: destination.to_string(),
            active_key: endpoint.active_key.clone(),
            previous_key: endpoint.previous_key.clone(),
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

    /// Construct the jobs registration adapter with immutable decoded keys.
    #[must_use]
    pub fn dispatcher(&self, keys: BTreeMap<String, SigningKey>) -> Dispatcher {
        Dispatcher {
            keys,
            limits: self.limits,
            clients: Arc::new(Mutex::new(ClientCache::default())),
        }
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

/// Immutable-secret dispatcher and bounded per-origin client cache.
#[derive(Clone)]
pub struct Dispatcher {
    keys: BTreeMap<String, SigningKey>,
    limits: Limits,
    clients: Arc<Mutex<ClientCache>>,
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

    async fn dispatch(&self, job: Job<Delivery>) -> Result<(), JobError> {
        let delivery = job.payload();
        if delivery.version != DELIVERY_VERSION || delivery.body.len() > MAX_BODY_BYTES {
            return Err(JobError::permanent(DeliveryOutcome::InvalidPayload));
        }
        let Some(keys) = self.key_ring(delivery) else {
            tracing::info!(
                webhook.outcome = "missing_secret",
                "webhook_delivery_deferred"
            );
            return Err(JobError::snooze(MISSING_SECRET_DELAY)?);
        };
        let Ok(destination) = parse_destination(&delivery.destination) else {
            tracing::warn!(
                webhook.outcome = "permanent",
                webhook.reason = "invalid_destination",
                "webhook_delivery_finished"
            );
            return Err(JobError::permanent(DeliveryOutcome::InvalidDestination));
        };
        let timestamp = unix_timestamp(SystemTime::now())
            .ok_or_else(|| JobError::retryable(DeliveryOutcome::ClockUnavailable))?;
        let message_id = job.id().to_string();
        let signature = keys
            .signatures(message_id.as_bytes(), timestamp, &delivery.body)
            .map_err(|_| JobError::permanent(DeliveryOutcome::InvalidPayload))?;
        let request = request(delivery, &destination, &message_id, timestamp, &signature)
            .map_err(|_| JobError::permanent(DeliveryOutcome::InvalidPayload))?;
        let client = match self.client(&destination) {
            Ok(client) => client,
            Err(OutboundError::CacheUnavailable) => {
                tracing::info!(
                    webhook.outcome = "retryable",
                    webhook.reason = "cache_unavailable",
                    "webhook_delivery_finished"
                );
                return Err(JobError::retryable(DeliveryOutcome::CacheUnavailable));
            }
            Err(_) => {
                tracing::warn!(
                    webhook.outcome = "permanent",
                    webhook.reason = "invalid_destination",
                    "webhook_delivery_finished"
                );
                return Err(JobError::permanent(DeliveryOutcome::InvalidDestination));
            }
        };
        let response = client
            .execute(
                request,
                Operation {
                    deadline: job.deadline(),
                    timeout: Some(DELIVERY_POLICY.timeout),
                    response_body_bytes: Some(RESPONSE_BODY_BYTES),
                },
            )
            .await;
        classify_response(response, SystemTime::now())
    }

    fn key_ring(&self, delivery: &Delivery) -> Option<KeyRing> {
        let active = self.keys.get(&delivery.active_key).cloned()?;
        let previous = match &delivery.previous_key {
            Some(reference) => Some(self.keys.get(reference).cloned()?),
            None => None,
        };
        Some(KeyRing::new(active, previous))
    }

    fn client(&self, destination: &Url) -> Result<Client, OutboundError> {
        let origin = origin(destination)?;
        if let Some(client) = self.cached_client(&origin)? {
            return Ok(client);
        }
        let candidate = Client::new(&origin, self.limits).map_err(OutboundError::Client)?;
        let mut cache = self
            .clients
            .lock()
            .map_err(|_| OutboundError::CacheUnavailable)?;
        if let Some(position) = cache
            .entries
            .iter()
            .position(|entry| entry.origin == origin)
        {
            return Ok(cache.entries[position].client.clone());
        }
        if cache.entries.len() == CACHE_CAPACITY {
            let _ = cache.entries.pop_front();
        }
        cache.entries.push_back(CachedClient {
            origin,
            client: candidate.clone(),
        });
        Ok(candidate)
    }

    fn cached_client(&self, origin: &str) -> Result<Option<Client>, OutboundError> {
        let cache = self
            .clients
            .lock()
            .map_err(|_| OutboundError::CacheUnavailable)?;
        Ok(cache
            .entries
            .iter()
            .find(|entry| entry.origin == origin)
            .map(|entry| entry.client.clone()))
    }
}

impl fmt::Debug for Dispatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Dispatcher([REDACTED])")
    }
}

#[derive(Default)]
struct ClientCache {
    entries: VecDeque<CachedClient>,
}

struct CachedClient {
    origin: String,
    client: Client,
}

#[derive(Clone, Serialize, Deserialize)]
struct Delivery {
    version: u8,
    endpoint_id: String,
    destination: String,
    active_key: String,
    previous_key: Option<String>,
    content_type: String,
    #[serde(with = "base64_body")]
    body: Vec<u8>,
}

impl JobKind for Delivery {
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
    /// An endpoint identity, key reference, URL, or transport construction is invalid.
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
    /// The process-local cache lock is unavailable after a panic.
    #[error("outbound webhook client cache is unavailable")]
    CacheUnavailable,
    /// Existing fixed-authority client construction failed.
    #[error("outbound webhook client configuration is invalid")]
    Client(#[source] HttpError),
}

#[derive(Clone, Copy)]
enum DeliveryOutcome {
    InvalidPayload,
    InvalidDestination,
    ClockUnavailable,
    CacheUnavailable,
    Retryable,
    PermanentResponse,
    TransportUncertain,
}

impl fmt::Display for DeliveryOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let outcome = match self {
            Self::InvalidPayload => "invalid_payload",
            Self::InvalidDestination => "invalid_destination",
            Self::ClockUnavailable => "clock_unavailable",
            Self::CacheUnavailable => "cache_unavailable",
            Self::Retryable => "retryable_response",
            Self::PermanentResponse => "permanent_response",
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
        response_header_bytes: RESPONSE_HEADER_BYTES,
        response_body_bytes: RESPONSE_BODY_BYTES,
    })
}

fn validate_endpoint_id(endpoint_id: &str) -> Result<(), OutboundError> {
    if endpoint_id.is_empty() || endpoint_id.contains('\0') {
        return Err(OutboundError::InvalidEndpoint);
    }
    Ok(())
}

fn validate_key_ref(reference: &str) -> Result<(), OutboundError> {
    if reference.is_empty() || reference.contains('\0') {
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
        Ok(response) if retryable_status(response.status()) => {
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
        Ok(response) => {
            tracing::warn!(
                webhook.outcome = "permanent",
                http.status = response.status().as_u16(),
                "webhook_delivery_finished"
            );
            Err(JobError::permanent(DeliveryOutcome::PermanentResponse))
        }
        Err(error) if is_permanent_transport_error(&error) => {
            tracing::warn!(
                webhook.outcome = "permanent",
                webhook.reason = "invalid_destination",
                "webhook_delivery_finished"
            );
            Err(JobError::permanent(DeliveryOutcome::InvalidDestination))
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

fn retryable_status(status: StatusCode) -> bool {
    status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_EARLY | StatusCode::TOO_MANY_REQUESTS
        )
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

fn is_permanent_transport_error(error: &HttpError) -> bool {
    matches!(
        error,
        HttpError::InvalidConfiguration
            | HttpError::InvalidTarget
            | HttpError::Denied
            | HttpError::ResolverConfiguration { .. }
            | HttpError::ClientBuild { .. }
    )
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

    use http::{HeaderMap, HeaderValue, StatusCode, header};
    use infra_jobs::MAX_PAYLOAD_BYTES;
    use url::Url;

    use super::{
        CACHE_CAPACITY, DEFAULT_CONTENT_TYPE, DELIVERY_VERSION, Endpoint, Outbound, OutboundError,
        RETRY_AFTER_CAP, parse_destination, request, retry_after, retryable_status,
    };
    use crate::protocol::{KeyRing, MAX_BODY_BYTES, SigningKey};

    fn outbound(destination: &str) -> Outbound {
        outbound_with("partner", destination, "partner_v2", None)
    }

    fn outbound_with(
        endpoint_id: &str,
        destination: &str,
        active_key: &str,
        previous_key: Option<&str>,
    ) -> Outbound {
        Outbound::new(
            BTreeMap::from([(
                endpoint_id.to_owned(),
                Endpoint::new(
                    destination.to_owned(),
                    active_key.to_owned(),
                    previous_key.map(str::to_owned),
                ),
            )]),
            NonZeroU32::new(1).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn prepares_final_raw_bytes_with_the_default_content_type() {
        let prepared = outbound("https://partner.example/events?source=template")
            .prepare("partner", b"raw\0bytes".to_vec(), None)
            .unwrap();

        assert_eq!(prepared.delivery.version, DELIVERY_VERSION);
        assert_eq!(prepared.delivery.body, b"raw\0bytes");
        assert_eq!(prepared.delivery.content_type, DEFAULT_CONTENT_TYPE);
        assert_eq!(
            prepared.delivery.destination,
            "https://partner.example/events?source=template"
        );
    }

    #[test]
    fn production_request_preserves_raw_body_and_stable_id_while_timestamp_refreshes_signature() {
        let body = vec![0, b'{', b'\"', b'x', b'\"', b':', b'1', b'}', 255];
        let prepared = outbound("https://partner.example/events?source=template")
            .prepare(
                "partner",
                body.clone(),
                Some("application/webhook+json".to_owned()),
            )
            .unwrap();
        let destination = parse_destination(&prepared.delivery.destination).unwrap();
        let keys =
            KeyRing::from_encoded("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=", None).unwrap();
        let message_id = "2ef5ca91-8d1b-495c-95eb-ecb3b7334718";
        let first = keys
            .signatures(message_id.as_bytes(), 1_700_000_000, &body)
            .unwrap();
        let second = keys
            .signatures(message_id.as_bytes(), 1_700_000_001, &body)
            .unwrap();
        let request = request(
            &prepared.delivery,
            &destination,
            message_id,
            1_700_000_000,
            &first,
        )
        .unwrap();

        assert_eq!(request.method(), http::Method::POST);
        assert_eq!(request.uri().to_string(), "/events?source=template");
        assert_eq!(request.body().as_ref(), body.as_slice());
        assert_eq!(
            request.headers()["webhook-id"].to_str().unwrap(),
            message_id
        );
        assert_eq!(
            request.headers()["webhook-timestamp"].to_str().unwrap(),
            "1700000000"
        );
        assert_eq!(
            request.headers()["webhook-signature"].to_str().unwrap(),
            first
        );
        assert_ne!(
            first, second,
            "each retry receives a fresh signature timestamp"
        );
        assert!(
            keys.verify(
                request.headers(),
                request.body().as_ref(),
                UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            )
            .is_ok()
        );
    }

    #[test]
    fn dispatcher_uses_historical_payload_destination_and_key_references_after_config_changes() {
        let historical = outbound_with(
            "partner",
            "https://historical.example/hooks/first?revision=1",
            "partner_v1",
            Some("partner_v0"),
        );
        let prepared = historical
            .prepare("partner", b"historical".to_vec(), None)
            .unwrap();
        let current = outbound_with(
            "partner",
            "https://current.example/hooks/current",
            "partner_v2",
            None,
        );
        let dispatcher = current.dispatcher(BTreeMap::from([
            (
                "partner_v1".to_owned(),
                SigningKey::from_encoded("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=").unwrap(),
            ),
            (
                "partner_v0".to_owned(),
                SigningKey::from_encoded("whsec_C2FVsBQIhrscChlQIMV+b5sSYspob7oD").unwrap(),
            ),
        ]));
        let destination = parse_destination(&prepared.delivery.destination).unwrap();

        let _client = dispatcher.client(&destination).unwrap();
        let cache = dispatcher.clients.lock().unwrap();
        assert_eq!(prepared.delivery.active_key, "partner_v1");
        assert_eq!(
            prepared.delivery.previous_key.as_deref(),
            Some("partner_v0")
        );
        assert!(dispatcher.key_ring(&prepared.delivery).is_some());
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.entries[0].origin, "https://historical.example/");
    }

    #[test]
    fn client_cache_normalizes_origins_and_evicts_fifo_after_sixty_four_entries() {
        let normalized = outbound("https://current.example/hooks").dispatcher(BTreeMap::new());
        let first = Url::parse("https://partner.example/a").unwrap();
        let second = Url::parse("https://partner.example/b?request=2").unwrap();
        let _first = normalized.client(&first).unwrap();
        let _second = normalized.client(&second).unwrap();
        assert_eq!(normalized.clients.lock().unwrap().entries.len(), 1);

        let fifo = outbound("https://current.example/hooks").dispatcher(BTreeMap::new());
        for index in 0..=CACHE_CAPACITY {
            let destination =
                Url::parse(&format!("https://history-{index}.example/hooks")).unwrap();
            let _client = fifo.client(&destination).unwrap();
        }
        let cache = fifo.clients.lock().unwrap();
        assert_eq!(cache.entries.len(), CACHE_CAPACITY);
        assert_eq!(
            cache.entries.front().unwrap().origin,
            "https://history-1.example/"
        );
        assert_eq!(
            cache.entries.back().unwrap().origin,
            "https://history-64.example/"
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
    fn rejects_credentials_fragments_and_nonpublic_or_nonhttps_static_destinations() {
        for destination in [
            "https://user@partner.example/events",
            "https://partner.example/events#fragment",
            "http://partner.example/events",
            "https://127.0.0.1/events",
            "https://169.254.169.254/latest/meta-data",
        ] {
            assert!(
                Outbound::new(
                    BTreeMap::from([(
                        "partner".to_owned(),
                        Endpoint::new(destination.to_owned(), "partner_v2".to_owned(), None),
                    )]),
                    NonZeroU32::new(1).unwrap(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn refuses_a_prepared_delivery_that_exceeds_the_existing_jobs_payload_limit() {
        let endpoint_id = "e".repeat(90_000);
        let outbound = outbound_with(
            &endpoint_id,
            "https://partner.example/events",
            "partner_v2",
            None,
        );
        let error = outbound
            .prepare(&endpoint_id, vec![0; MAX_BODY_BYTES], None)
            .unwrap_err();

        assert!(
            matches!(error, OutboundError::PayloadTooLarge { bytes } if bytes > MAX_PAYLOAD_BYTES),
            "{error:?}"
        );
    }

    #[test]
    fn maps_only_the_selected_statuses_to_retries() {
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_EARLY,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert!(retryable_status(status), "{status}");
        }
        for status in [
            StatusCode::MOVED_PERMANENTLY,
            StatusCode::BAD_REQUEST,
            StatusCode::GONE,
        ] {
            assert!(!retryable_status(status), "{status}");
        }
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
