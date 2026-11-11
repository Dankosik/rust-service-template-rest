//! Durable receipt admission and processing for inbound Standard Webhooks.
//!
//! This module verifies a configured endpoint through its [`Verifier`] before
//! it reaches PostgreSQL, then owns the one transaction that writes a receipt
//! and enqueues processing.
//! Processing invokes one explicit adopter callback and completes its fenced job
//! in that callback's transaction.

mod maintenance;
mod processing;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use base64::display::Base64Display;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue};
use infra_jobs::{EnqueueError, EnqueueOptions, Enqueued, JobKind, enqueue};
use infra_postgres::{
    Isolation, TxError, TxOptions, failure_cause, in_tx_until, observed, sqlstate,
};
use serde::{Deserialize, Serialize, Serializer};
use serde_with::base64::Base64;
use serde_with::{SerializeAs, serde_as};
use sqlx::postgres::PgPool;
use tokio::time::Instant;

use crate::protocol::{KeyRing, ProtocolError, ensure_body_size, ensure_message_id_len};

pub use maintenance::{CLEANUP_REMOVED_METRIC, CLEANUP_RUNS_METRIC, CleanupError};
pub use processing::{Consumer, Consumers, DuplicateConsumer, MissingConsumer, Processor};

/// Re-exported so a consumer crate implements [`Consumer`] without its own
/// `async-trait` dependency, as `tonic::async_trait` does for services.
pub use async_trait::async_trait;

// Under REPEATABLE READ a concurrent `ON CONFLICT DO NOTHING` fails with 40001
// instead of reporting the duplicate, so admission pins READ COMMITTED.
const READ_COMMITTED: TxOptions = TxOptions {
    isolation: Isolation::ReadCommitted,
    read_only: false,
};

/// The durable admission result for one verified delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptOutcome {
    /// The receipt and processing job committed together.
    Accepted,
    /// The endpoint and message identity were already retained; first admission wins.
    Duplicate,
}

/// A closed inbound admission failure for the HTTP adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReceiveError {
    #[error("inbound webhook endpoint is not configured")]
    UnknownEndpoint,
    #[error("inbound webhook delivery was rejected: {}", .0.reason())]
    Rejected(Rejection),
    /// Receipt persistence or its commit acknowledgement was unavailable.
    #[error("inbound webhook receipt storage is unavailable")]
    Unavailable,
}

/// Why a [`Verifier`] refused a delivery.
///
/// The type guarantees only a `'static` label. The label becomes a log field
/// and a metric label, so a [`Verifier`] keeps its own set of reasons small and
/// `snake_case`, and never builds one from request, signature, or key bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rejection(&'static str);

impl Rejection {
    /// A rejection with a static `snake_case` reason label.
    #[must_use]
    pub const fn new(reason: &'static str) -> Self {
        Self(reason)
    }

    #[must_use]
    pub const fn reason(self) -> &'static str {
        self.0
    }
}

const PAYLOAD_TOO_LARGE: Rejection = Rejection::new("payload_too_large");

impl From<ProtocolError> for Rejection {
    fn from(error: ProtocolError) -> Self {
        Self(error.reason())
    }
}

/// One endpoint's sender authentication.
///
/// [`KeyRing`] is the Standard Webhooks verifier. A provider that signs
/// another way implements this for its own scheme and keeps the durable
/// receipt, deduplication, and processing path.
pub trait Verifier: Send + Sync + 'static {
    /// Authenticate one delivery and return its message identity: the
    /// provider's stable ID for this message, 1 to [`MAX_MESSAGE_ID_BYTES`](crate::protocol::MAX_MESSAGE_ID_BYTES)
    /// bytes, equal on every redelivery. `body` is the raw request body and
    /// `now` the admission time for replay windows.
    ///
    /// # Errors
    ///
    /// A [`Rejection`] when the evidence is missing, malformed, stale, or does
    /// not match the endpoint's key.
    fn authenticate(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
    ) -> Result<Bytes, Rejection>;
}

impl Verifier for KeyRing {
    fn authenticate(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
    ) -> Result<Bytes, Rejection> {
        let verified = self.verify(headers, body, now)?;
        Ok(verified.message_id().clone())
    }
}

/// Configured receiving endpoints and their durable receipt store.
#[derive(Clone)]
pub struct Receiver {
    pool: PgPool,
    endpoints: Arc<HashMap<String, Arc<dyn Verifier>>>,
}

impl Receiver {
    /// Build a receiver over each configured endpoint's verifier, so one
    /// receiver can mix schemes. Construction does no I/O.
    #[must_use]
    pub fn new(
        pool: PgPool,
        endpoints: impl IntoIterator<Item = (String, Arc<dyn Verifier>)>,
    ) -> Self {
        Self {
            pool,
            endpoints: Arc::new(endpoints.into_iter().collect()),
        }
    }

    #[must_use]
    pub fn has_endpoint(&self, endpoint_id: &str) -> bool {
        self.endpoints.contains_key(endpoint_id)
    }

    /// Verify and atomically retain an inbound delivery.
    ///
    /// A duplicate must pass verification again. A successful result means the
    /// receipt transaction was acknowledged; it never claims the consumer ran.
    /// The wait for a pooled connection lasts until `deadline`, the caller's
    /// own budget, instead of the pool's fixed bound.
    ///
    /// # Errors
    ///
    /// Returns a closed rejection for an unknown endpoint, refused evidence, a
    /// body over [`MAX_BODY_BYTES`](crate::protocol::MAX_BODY_BYTES), a message identity outside 1 to
    /// [`MAX_MESSAGE_ID_BYTES`](crate::protocol::MAX_MESSAGE_ID_BYTES) bytes, or a delivery too large to retain, or
    /// unavailable when receipt ownership could not be acknowledged.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub async fn receive(
        &self,
        endpoint_id: &str,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
        deadline: Instant,
    ) -> Result<ReceiptOutcome, ReceiveError> {
        self.receive_inner(
            endpoint_id,
            headers,
            ReceiveBody::Borrowed(body),
            now,
            deadline,
        )
        .await
    }

    /// Verify and atomically retain an inbound delivery, taking ownership of its body.
    ///
    /// This has the same admission semantics as [`Self::receive`], but moves the
    /// body into the winning receipt's payload without copying it.
    ///
    /// # Errors
    ///
    /// Returns the same endpoint, verification and persistence errors as [`Self::receive`].
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub async fn receive_bytes(
        &self,
        endpoint_id: &str,
        headers: &HeaderMap,
        body: Bytes,
        now: SystemTime,
        deadline: Instant,
    ) -> Result<ReceiptOutcome, ReceiveError> {
        self.receive_inner(
            endpoint_id,
            headers,
            ReceiveBody::Owned(body),
            now,
            deadline,
        )
        .await
    }

    async fn receive_inner(
        &self,
        endpoint_id: &str,
        headers: &HeaderMap,
        body: ReceiveBody<'_>,
        now: SystemTime,
        deadline: Instant,
    ) -> Result<ReceiptOutcome, ReceiveError> {
        let Some(verifier) = self.endpoints.get(endpoint_id) else {
            return Err(ReceiveError::UnknownEndpoint);
        };
        let message_id = admit(verifier.as_ref(), headers, body.as_ref(), now)
            .map_err(|rejection| rejected(endpoint_id, rejection))?;
        let content_type = headers.get(CONTENT_TYPE).map(HeaderValue::as_bytes);
        in_tx_until(
            &self.pool,
            deadline,
            READ_COMMITTED,
            async |tx| -> Result<ReceiptOutcome, ReceiptFailure> {
                let inserted = observed(
                    "insert webhook receipt",
                    sqlx::query_scalar!(
                        "INSERT INTO webhook_receipts (endpoint_id, message_id) \
                         VALUES ($1, $2) \
                         ON CONFLICT (endpoint_id, message_id) DO NOTHING \
                         RETURNING message_id",
                        endpoint_id,
                        message_id.as_ref(),
                    )
                    .fetch_optional(&mut *tx),
                )
                .await?;
                if inserted.is_none() {
                    return Ok(ReceiptOutcome::Duplicate);
                }
                let incoming =
                    Incoming::new(endpoint_id, message_id, content_type, body.into_bytes());
                match enqueue(tx, &incoming, EnqueueOptions::default()).await {
                    Ok(Enqueued::Created(_)) => Ok(ReceiptOutcome::Accepted),
                    Ok(Enqueued::Duplicate) => Err(ReceiptFailure::UnexpectedDuplicate),
                    // Only the unsigned Content-Type can push a body within
                    // MAX_BODY_BYTES past the job payload bound; a retry fails
                    // the same way, so it is the request's fault.
                    Err(EnqueueError::PayloadTooLarge { .. }) => {
                        Err(ReceiptFailure::PayloadTooLarge)
                    }
                    Err(error) => Err(error.into()),
                }
            },
        )
        .await
        .map_err(|error| match error {
            ReceiptFailure::PayloadTooLarge => rejected(endpoint_id, PAYLOAD_TOO_LARGE),
            error => receipt_unavailable(endpoint_id, &error),
        })
    }
}

impl fmt::Debug for Receiver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Receiver")
            .field("endpoint_count", &self.endpoints.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
enum ReceiptFailure {
    #[error("webhook receipt query failed")]
    Query(#[from] sqlx::Error),
    #[error("webhook receipt enqueue failed")]
    Enqueue(#[from] EnqueueError),
    #[error("webhook receipt transaction failed")]
    Transaction(#[from] TxError),
    /// A receipt without a unique key cannot be a duplicate job.
    #[error("webhook receipt enqueue unexpectedly deduplicated")]
    UnexpectedDuplicate,
    #[error("webhook receipt payload exceeds the job payload bound")]
    PayloadTooLarge,
}

impl ReceiptFailure {
    /// The static `webhook.reason` log label.
    const fn reason(&self) -> &'static str {
        match self {
            Self::UnexpectedDuplicate => "integrity",
            Self::Transaction(TxError::CommitUnknown(_)) => "commit_unknown",
            Self::PayloadTooLarge => PAYLOAD_TOO_LARGE.reason(),
            Self::Query(_) | Self::Enqueue(_) | Self::Transaction(_) => "unavailable",
        }
    }

    fn driver(&self) -> Option<&sqlx::Error> {
        match self {
            Self::Query(err) | Self::Enqueue(EnqueueError::Database(err)) => Some(err),
            Self::Transaction(err) => Some(err.driver()),
            Self::Enqueue(_) | Self::UnexpectedDuplicate | Self::PayloadTooLarge => None,
        }
    }
}

fn rejected(endpoint_id: &str, rejection: Rejection) -> ReceiveError {
    tracing::info!(
        webhook.endpoint = endpoint_id,
        webhook.reason = rejection.reason(),
        "webhook_delivery_rejected"
    );
    ReceiveError::Rejected(rejection)
}

/// Log a receipt the store could not acknowledge, with bounded fields only.
fn receipt_unavailable(endpoint_id: &str, error: &ReceiptFailure) -> ReceiveError {
    let driver = error.driver();
    tracing::warn!(
        webhook.endpoint = endpoint_id,
        webhook.reason = error.reason(),
        sqlstate = driver.and_then(sqlstate).as_deref(),
        cause = driver.map(failure_cause),
        "webhook_receipt_unavailable"
    );
    ReceiveError::Unavailable
}

/// Authenticate a delivery and return its message identity.
///
/// Both bounds are checked here as well as in [`KeyRing::verify`], so they
/// hold for every verifier; the identity bound protects the indexed receipt
/// key. The signed-content `.` rule stays with Standard Webhooks.
fn admit(
    verifier: &dyn Verifier,
    headers: &HeaderMap,
    body: &[u8],
    now: SystemTime,
) -> Result<Bytes, Rejection> {
    ensure_body_size(body)?;
    let message_id = verifier.authenticate(headers, body, now)?;
    ensure_message_id_len(&message_id)?;
    Ok(message_id)
}

enum ReceiveBody<'a> {
    Borrowed(&'a [u8]),
    Owned(Bytes),
}

impl ReceiveBody<'_> {
    /// Copy a borrowed body only here, after its receipt won admission.
    fn into_bytes(self) -> Bytes {
        match self {
            Self::Borrowed(body) => Bytes::copy_from_slice(body),
            Self::Owned(body) => body,
        }
    }
}

impl AsRef<[u8]> for ReceiveBody<'_> {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Borrowed(body) => body,
            Self::Owned(body) => body,
        }
    }
}

/// The retained payload of a verified inbound delivery.
///
/// `version` is written and never read: a worker from before the tag became
/// optional refuses a row without it, so a rolling deploy or a rollback still
/// needs it on the wire. A reader ignores it, whatever its value.
#[serde_as]
#[derive(Clone, Deserialize, Serialize)]
pub struct Incoming {
    #[serde(skip_deserializing, default = "incoming_version")]
    version: u8,
    endpoint_id: String,
    #[serde_as(serialize_as = "StreamingBase64", deserialize_as = "Base64")]
    message_id: Bytes,
    #[serde_as(
        serialize_as = "Option<StreamingBase64>",
        deserialize_as = "Option<Base64>"
    )]
    content_type: Option<Vec<u8>>,
    #[serde_as(serialize_as = "StreamingBase64", deserialize_as = "Base64")]
    body: Bytes,
}

/// Stream Base64 into JSON without an intermediate String.
struct StreamingBase64;

impl<T: AsRef<[u8]>> SerializeAs<T> for StreamingBase64 {
    fn serialize_as<S: Serializer>(source: &T, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Base64Display::new(source.as_ref(), &STANDARD))
    }
}

const fn incoming_version() -> u8 {
    1
}

impl Incoming {
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn new(endpoint_id: &str, message_id: Bytes, content_type: Option<&[u8]>, body: Bytes) -> Self {
        Self {
            version: incoming_version(),
            endpoint_id: endpoint_id.to_owned(),
            message_id,
            content_type: content_type.map(ToOwned::to_owned),
            body,
        }
    }

    #[must_use]
    pub fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    #[must_use]
    pub fn message_id(&self) -> &[u8] {
        &self.message_id
    }

    /// The first accepted Content-Type header bytes, when present.
    #[must_use]
    pub fn content_type(&self) -> Option<&[u8]> {
        self.content_type.as_deref()
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

impl fmt::Debug for Incoming {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Incoming")
            .field("version", &self.version)
            .field("endpoint_id", &self.endpoint_id)
            .field("message_id", &"[REDACTED]")
            .field("content_type", &self.content_type.is_some())
            .field("body", &"[REDACTED]")
            .finish()
    }
}

impl JobKind for Incoming {
    const NAME: &'static str = "webhooks.process";
}

const _: () = infra_jobs::assert_valid_kind_name(Incoming::NAME);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::MAX_BODY_BYTES;

    #[tokio::test]
    async fn a_body_over_the_bound_is_rejected_before_any_verifier_or_write() {
        struct AcceptAll;
        impl Verifier for AcceptAll {
            fn authenticate(
                &self,
                _headers: &HeaderMap,
                _body: &[u8],
                _now: SystemTime,
            ) -> Result<Bytes, Rejection> {
                panic!("an oversized body never reaches the verifier")
            }
        }
        // Nothing listens on port 1, so any write would fail as Unavailable.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://127.0.0.1:1/unreachable")
            .expect("a lazy pool opens no connection");
        let receiver = Receiver::new(pool, [("provider".to_owned(), Arc::new(AcceptAll) as _)]);
        let outcome = receiver
            .receive(
                "provider",
                &HeaderMap::new(),
                &vec![0; MAX_BODY_BYTES + 1],
                SystemTime::now(),
                Instant::now(),
            )
            .await;
        assert_eq!(
            outcome,
            Err(ReceiveError::Rejected(Rejection::new("body_too_large")))
        );
    }
}
