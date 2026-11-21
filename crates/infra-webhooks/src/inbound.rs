mod processing;
pub mod route;

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use compact_str::CompactString;
use foldhash::HashMap;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue};
use infra_jobs::{EnqueueError, JobKind, KindName, enqueue};
use infra_postgres::{Isolation, Pool, TxError, TxOptions, failure_cause, observed, sqlstate};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use tokio::time::Instant;

use crate::protocol::{KeyRing, ProtocolError, ensure_body_size, ensure_message_id_len};

pub use processing::{Consumer, Consumers, EndpointAlreadyBound, MissingConsumer, Processor};

const READ_COMMITTED: TxOptions = TxOptions {
    isolation: Isolation::ReadCommitted,
    read_only: false,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptOutcome {
    Accepted,
    Duplicate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReceiveError {
    #[error("inbound webhook endpoint is not configured")]
    UnknownEndpoint,
    #[error("inbound webhook delivery was rejected: {0}")]
    Rejected(Rejection),
    #[error("inbound webhook receipt storage is unavailable")]
    Unavailable,
}

#[derive(Clone, Copy)]
pub struct Rejection(RejectionKind);

#[derive(Clone, Copy)]
enum RejectionKind {
    Protocol(ProtocolError),
    PayloadTooLarge,
    Custom(&'static str),
}

impl Rejection {
    #[must_use]
    pub const fn new(reason: &'static str) -> Self {
        Self(RejectionKind::Custom(reason))
    }

    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self.0 {
            RejectionKind::Protocol(error) => error.reason(),
            RejectionKind::PayloadTooLarge => "payload_too_large",
            RejectionKind::Custom(reason) => reason,
        }
    }

    const fn kind(self) -> RejectionKind {
        self.0
    }
}

impl PartialEq for Rejection {
    fn eq(&self, other: &Self) -> bool {
        self.reason() == other.reason()
    }
}

impl Eq for Rejection {}

impl fmt::Debug for Rejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Rejection")
            .field(&self.reason())
            .finish()
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason())
    }
}

impl std::error::Error for Rejection {}

impl Hash for Rejection {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.reason().hash(state);
    }
}

const PAYLOAD_TOO_LARGE: Rejection = Rejection(RejectionKind::PayloadTooLarge);

impl From<ProtocolError> for Rejection {
    fn from(error: ProtocolError) -> Self {
        Self(RejectionKind::Protocol(error))
    }
}

enum EndpointReceiveError {
    Rejected(Rejection),
    Unavailable,
}

impl From<EndpointReceiveError> for ReceiveError {
    fn from(error: EndpointReceiveError) -> Self {
        match error {
            EndpointReceiveError::Rejected(rejection) => Self::Rejected(rejection),
            EndpointReceiveError::Unavailable => Self::Unavailable,
        }
    }
}

pub type MessageId = Bytes;

pub trait Verifier: Send + Sync + 'static {
    fn authenticate(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
    ) -> Result<MessageId, Rejection>;
}

impl<V: Verifier> Verifier for Arc<V> {
    fn authenticate(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
    ) -> Result<MessageId, Rejection> {
        (**self).authenticate(headers, body, now)
    }
}

impl Verifier for KeyRing {
    fn authenticate(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
    ) -> Result<MessageId, Rejection> {
        Ok(self.verify(headers, body, now)?.into_message_id())
    }
}

#[derive(Default)]
pub struct Verifiers {
    bindings: HashMap<Arc<str>, Arc<dyn Verifier>>,
}

impl Verifiers {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind(&mut self, endpoint_id: impl Into<Arc<str>>, verifier: impl Verifier) -> &mut Self {
        self.bindings.insert(endpoint_id.into(), Arc::new(verifier));
        self
    }

    #[must_use]
    pub fn into_receiver(self, pool: Pool) -> Receiver {
        Receiver {
            pool,
            endpoints: Arc::new(self.bindings),
        }
    }
}

impl fmt::Debug for Verifiers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Verifiers")
            .field("binding_count", &self.bindings.len())
            .finish()
    }
}

#[derive(Clone)]
pub struct Receiver {
    pool: Pool,
    endpoints: Arc<HashMap<Arc<str>, Arc<dyn Verifier>>>,
}

impl Receiver {
    pub async fn receive(
        &self,
        endpoint_id: &str,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
        deadline: Instant,
    ) -> Result<ReceiptOutcome, ReceiveError> {
        let verifier = self.verifier(endpoint_id)?;
        receive_for_endpoint(
            &self.pool,
            endpoint_id,
            verifier,
            headers,
            ReceiveBody::Borrowed(body),
            now,
            deadline,
        )
        .await
        .map_err(ReceiveError::from)
    }

    pub async fn receive_bytes(
        &self,
        endpoint_id: &str,
        headers: &HeaderMap,
        body: Bytes,
        now: SystemTime,
        deadline: Instant,
    ) -> Result<ReceiptOutcome, ReceiveError> {
        let verifier = self.verifier(endpoint_id)?;
        receive_for_endpoint(
            &self.pool,
            endpoint_id,
            verifier,
            headers,
            ReceiveBody::Owned(body),
            now,
            deadline,
        )
        .await
        .map_err(ReceiveError::from)
    }

    fn verifier(&self, endpoint_id: &str) -> Result<&dyn Verifier, ReceiveError> {
        self.endpoints
            .get(endpoint_id)
            .map(AsRef::as_ref)
            .ok_or(ReceiveError::UnknownEndpoint)
    }
}

#[cfg_attr(feature = "hotpath", hotpath::measure)]
async fn receive_for_endpoint(
    pool: &Pool,
    endpoint_id: &str,
    verifier: &dyn Verifier,
    headers: &HeaderMap,
    body: ReceiveBody<'_>,
    now: SystemTime,
    deadline: Instant,
) -> Result<ReceiptOutcome, EndpointReceiveError> {
    let admitted = admit(verifier, headers, body, now)
        .map_err(|rejection| rejected(endpoint_id, rejection))?;
    let content_type = headers.get(CONTENT_TYPE).map(HeaderValue::as_bytes);
    Box::pin(pool.in_tx_with_deadline(
        deadline,
        READ_COMMITTED,
        async |tx| -> Result<ReceiptOutcome, ReceiptFailure> {
            let inserted = observed!(
                "insert webhook receipt",
                sqlx::query!(
                    "INSERT INTO webhook_receipts (endpoint_id, message_id) \
                     VALUES ($1, $2) \
                     ON CONFLICT (endpoint_id, message_id) DO NOTHING",
                    endpoint_id,
                    admitted.message_id.as_ref(),
                )
                .execute(&mut *tx),
            )
            .await?;
            if inserted.rows_affected() == 0 {
                return Ok(ReceiptOutcome::Duplicate);
            }
            let incoming = Incoming::new(endpoint_id, admitted, content_type);
            enqueue(tx, &incoming).await?;
            Ok(ReceiptOutcome::Accepted)
        },
    ))
    .await
    .map_err(|error| match error {
        ReceiptFailure::Enqueue(EnqueueError::PayloadTooLarge { .. }) => {
            rejected(endpoint_id, PAYLOAD_TOO_LARGE)
        }
        error => receipt_unavailable(endpoint_id, &error),
    })
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
    #[error(transparent)]
    Deadline(#[from] infra_postgres::OperationDeadlineElapsed),
    #[error("webhook receipt query failed")]
    Query(#[from] sqlx::Error),
    #[error("webhook receipt enqueue failed")]
    Enqueue(#[from] EnqueueError),
    #[error("webhook receipt transaction failed")]
    Transaction(#[from] TxError),
}

fn rejected(endpoint_id: &str, rejection: Rejection) -> EndpointReceiveError {
    tracing::info!(
        webhook.endpoint = endpoint_id,
        webhook.reason = rejection.reason(),
        "webhook_delivery_rejected"
    );
    EndpointReceiveError::Rejected(rejection)
}

fn receipt_unavailable(endpoint_id: &str, error: &ReceiptFailure) -> EndpointReceiveError {
    let (stage, reason, driver) = match error {
        ReceiptFailure::Query(err) => ("insert_receipt", "unavailable", Some(err)),
        ReceiptFailure::Enqueue(EnqueueError::Database(err)) => {
            ("enqueue", "unavailable", Some(err))
        }
        ReceiptFailure::Enqueue(_) => ("enqueue", "unavailable", None),
        ReceiptFailure::Transaction(err @ TxError::CommitUnknown(_)) => {
            ("transaction", "commit_unknown", Some(err.driver()))
        }
        ReceiptFailure::Transaction(err) => ("transaction", "unavailable", Some(err.driver())),
        ReceiptFailure::Deadline(_) => ("transaction", "unavailable", None),
    };
    tracing::warn!(
        webhook.endpoint = endpoint_id,
        webhook.reason = reason,
        webhook.stage = stage,
        sqlstate = driver.and_then(sqlstate).as_deref(),
        cause = driver.map(failure_cause),
        "webhook_receipt_unavailable"
    );
    EndpointReceiveError::Unavailable
}

struct AdmittedBody<'a> {
    message_id: MessageId,
    body: ReceiveBody<'a>,
}

fn admit<'a>(
    verifier: &dyn Verifier,
    headers: &HeaderMap,
    body: ReceiveBody<'a>,
    now: SystemTime,
) -> Result<AdmittedBody<'a>, Rejection> {
    ensure_body_size(body.as_ref())?;
    let message_id = verifier.authenticate(headers, body.as_ref(), now)?;
    ensure_message_id_len(&message_id)?;
    Ok(AdmittedBody { message_id, body })
}

enum ReceiveBody<'a> {
    Borrowed(&'a [u8]),
    Owned(Bytes),
}

impl ReceiveBody<'_> {
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

#[derive(Clone, Deserialize, Serialize)]
struct ContentType(#[serde(with = "crate::base64_bytes")] SmallVec<[u8; 32]>);

#[derive(Clone, derive_more::Debug, Deserialize, Serialize)]
pub struct Incoming {
    #[serde(skip_deserializing, default = "incoming_version")]
    version: u8,
    endpoint_id: CompactString,
    #[debug("{:?}", "[REDACTED]")]
    #[serde(with = "crate::base64_bytes")]
    message_id: MessageId,
    #[debug("{:?}", content_type.is_some())]
    #[serde(default)]
    content_type: Option<ContentType>,
    #[debug("{:?}", "[REDACTED]")]
    #[serde(with = "crate::base64_bytes")]
    body: Bytes,
}

const fn incoming_version() -> u8 {
    1
}

impl Incoming {
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn new(endpoint_id: &str, admitted: AdmittedBody<'_>, content_type: Option<&[u8]>) -> Self {
        Self {
            version: incoming_version(),
            endpoint_id: CompactString::from(endpoint_id),
            message_id: admitted.message_id,
            content_type: content_type.map(|bytes| ContentType(SmallVec::from_slice(bytes))),
            body: admitted.body.into_bytes(),
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

    #[must_use]
    pub fn content_type(&self) -> Option<&[u8]> {
        self.content_type.as_ref().map(|bytes| bytes.0.as_slice())
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

impl JobKind for Incoming {
    const NAME: KindName = KindName::new("webhooks.process");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::MAX_BODY_BYTES;

    #[tokio::test]
    async fn a_body_over_the_bound_is_rejected_before_any_verifier_or_write() {
        struct MustNotAuthenticate;
        impl Verifier for MustNotAuthenticate {
            fn authenticate(
                &self,
                _headers: &HeaderMap,
                _body: &[u8],
                _now: SystemTime,
            ) -> Result<Bytes, Rejection> {
                panic!("an oversized body never reaches the verifier")
            }
        }
        let pool = infra_postgres::test_support::connect_lazy("postgres://127.0.0.1:1/unreachable")
            .expect("a lazy pool opens no connection");
        let mut verifiers = Verifiers::new();
        verifiers.bind("provider", Arc::new(MustNotAuthenticate));
        assert_eq!(format!("{verifiers:?}"), "Verifiers { binding_count: 1 }");
        let receiver = verifiers.into_receiver(pool);
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

    #[test]
    fn incoming_debug_redacts_message_id_content_type_and_body() {
        let incoming = |content_type: Option<&[u8]>| {
            Incoming::new(
                "partner",
                AdmittedBody {
                    message_id: Bytes::from_static(b"secret-id"),
                    body: ReceiveBody::Borrowed(b"secret-body"),
                },
                content_type,
            )
        };
        let with_type = incoming(Some(b"application/secret"));
        assert_eq!(
            format!("{with_type:?}"),
            r#"Incoming { version: 1, endpoint_id: "partner", message_id: "[REDACTED]", content_type: true, body: "[REDACTED]" }"#
        );
        assert_eq!(
            format!("{with_type:#?}"),
            "Incoming {\n    version: 1,\n    endpoint_id: \"partner\",\n    message_id: \"[REDACTED]\",\n    content_type: true,\n    body: \"[REDACTED]\",\n}"
        );

        let without_type = incoming(None);
        assert_eq!(
            format!("{without_type:?}"),
            r#"Incoming { version: 1, endpoint_id: "partner", message_id: "[REDACTED]", content_type: false, body: "[REDACTED]" }"#
        );
    }

    #[test]
    fn rejection_equality_hash_display_error_and_debug_preserve_reason_spelling() {
        use std::collections::hash_map::DefaultHasher;

        let hash = |rejection: Rejection| {
            let mut hasher = DefaultHasher::new();
            rejection.hash(&mut hasher);
            hasher.finish()
        };

        let protocol = Rejection::from(ProtocolError::InvalidSignature);
        let custom = Rejection::new("invalid_signature");
        assert_eq!(protocol, custom);
        assert_eq!(hash(protocol), hash(custom));
        assert_eq!(protocol.to_string(), "invalid_signature");
        assert_eq!(custom.to_string(), "invalid_signature");
        let error: &dyn std::error::Error = &protocol;
        assert_eq!(error.to_string(), "invalid_signature");
        assert!(std::error::Error::source(&ReceiveError::Rejected(protocol)).is_none());
        assert_eq!(
            ReceiveError::Rejected(protocol).to_string(),
            "inbound webhook delivery was rejected: invalid_signature"
        );
        assert_eq!(format!("{protocol:?}"), r#"Rejection("invalid_signature")"#);
        assert_eq!(format!("{custom:?}"), r#"Rejection("invalid_signature")"#);
        assert_eq!(PAYLOAD_TOO_LARGE, Rejection::new("payload_too_large"));
    }

    #[test]
    fn incoming_deserializes_omitted_content_type_and_unpadded_base64() {
        let omitted: Incoming = serde_json::from_str(
            r#"{"version":1,"endpoint_id":"partner","message_id":"aWQ","body":"cmF3/w"}"#,
        )
        .expect("omitted content_type and unpadded base64");
        assert_eq!(omitted.endpoint_id(), "partner");
        assert_eq!(omitted.message_id(), b"id");
        assert_eq!(omitted.content_type(), None);
        assert_eq!(omitted.body(), b"raw\xff");
        assert_eq!(
            serde_json::to_string(&omitted).expect("serialized"),
            r#"{"version":1,"endpoint_id":"partner","message_id":"aWQ=","content_type":null,"body":"cmF3/w=="}"#
        );

        let unpadded_type: Incoming = serde_json::from_str(
            r#"{"version":1,"endpoint_id":"partner","message_id":"aWQ","content_type":"/w","body":""}"#,
        )
        .expect("unpadded content_type");
        assert_eq!(unpadded_type.content_type(), Some(b"\xff".as_slice()));
    }
}
