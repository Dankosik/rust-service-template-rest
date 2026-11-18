mod processing;
pub mod route;

use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use foldhash::HashMap;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue};
use infra_jobs::{EnqueueError, EnqueueOptions, Enqueued, JobKind, enqueue};
use infra_postgres::{
    Isolation, TxError, TxOptions, failure_cause, in_tx_with_deadline, observed, sqlstate,
};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use sqlx::postgres::PgPool;
use tokio::time::Instant;

use crate::StreamingBase64;
use crate::protocol::{KeyRing, ProtocolError, ensure_body_size, ensure_message_id_len};

pub use processing::{Consumer, Consumers, EndpointAlreadyBound, MissingConsumer, Processor};

pub use async_trait::async_trait;

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
    #[error("inbound webhook delivery was rejected: {}", .0.reason())]
    Rejected(Rejection),
    #[error("inbound webhook receipt storage is unavailable")]
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rejection(&'static str);

impl Rejection {
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

pub type MessageId = Bytes;

pub trait Verifier: Send + Sync + 'static {
    fn authenticate(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
    ) -> Result<MessageId, Rejection>;
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

#[derive(Clone)]
pub struct Receiver {
    pool: PgPool,
    endpoints: Arc<HashMap<String, Arc<dyn Verifier>>>,
}

impl Receiver {
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

    pub fn endpoint_ids(&self) -> impl ExactSizeIterator<Item = &str> {
        self.endpoints.keys().map(String::as_str)
    }

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
        let admitted = admit(verifier.as_ref(), headers, body, now)
            .map_err(|rejection| rejected(endpoint_id, rejection))?;
        let content_type = headers.get(CONTENT_TYPE).map(HeaderValue::as_bytes);
        in_tx_with_deadline(
            &self.pool,
            deadline,
            READ_COMMITTED,
            async |tx| -> Result<ReceiptOutcome, ReceiptFailure> {
                let inserted = observed(
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
                match enqueue(tx, &incoming, EnqueueOptions::default()).await? {
                    Enqueued::Created(_) => Ok(ReceiptOutcome::Accepted),
                    Enqueued::Duplicate => Err(ReceiptFailure::UnexpectedDuplicate),
                }
            },
        )
        .await
        .map_err(|error| match error {
            ReceiptFailure::Enqueue(EnqueueError::PayloadTooLarge { .. }) => {
                rejected(endpoint_id, PAYLOAD_TOO_LARGE)
            }
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
    #[error(transparent)]
    Deadline(#[from] infra_postgres::OperationDeadlineElapsed),
    #[error("webhook receipt query failed")]
    Query(#[from] sqlx::Error),
    #[error("webhook receipt enqueue failed")]
    Enqueue(#[from] EnqueueError),
    #[error("webhook receipt transaction failed")]
    Transaction(#[from] TxError),
    #[error("webhook receipt enqueue unexpectedly deduplicated")]
    UnexpectedDuplicate,
}

fn rejected(endpoint_id: &str, rejection: Rejection) -> ReceiveError {
    tracing::info!(
        webhook.endpoint = endpoint_id,
        webhook.reason = rejection.reason(),
        "webhook_delivery_rejected"
    );
    ReceiveError::Rejected(rejection)
}

fn receipt_unavailable(endpoint_id: &str, error: &ReceiptFailure) -> ReceiveError {
    let (stage, reason, driver) = match error {
        ReceiptFailure::Query(err) => ("insert_receipt", "unavailable", Some(err)),
        ReceiptFailure::Enqueue(EnqueueError::Database(err)) => {
            ("enqueue", "unavailable", Some(err))
        }
        ReceiptFailure::Enqueue(_) => ("enqueue", "unavailable", None),
        ReceiptFailure::UnexpectedDuplicate => ("enqueue", "integrity", None),
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
    ReceiveError::Unavailable
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

#[serde_as]
#[derive(Clone, derive_more::Debug, Deserialize, Serialize)]
pub struct Incoming {
    #[serde(skip_deserializing, default = "incoming_version")]
    version: u8,
    endpoint_id: String,
    #[debug("{:?}", "[REDACTED]")]
    #[serde_as(as = "StreamingBase64")]
    message_id: MessageId,
    #[debug("{:?}", content_type.is_some())]
    #[serde_as(as = "Option<StreamingBase64>")]
    content_type: Option<Vec<u8>>,
    #[debug("{:?}", "[REDACTED]")]
    #[serde_as(as = "StreamingBase64")]
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
            endpoint_id: endpoint_id.to_owned(),
            message_id: admitted.message_id,
            content_type: content_type.map(ToOwned::to_owned),
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
        self.content_type.as_deref()
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
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
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://127.0.0.1:1/unreachable")
            .expect("a lazy pool opens no connection");
        let verifier: Arc<dyn Verifier> = Arc::new(AcceptAll);
        let receiver = Receiver::new(pool, [("provider".to_owned(), verifier)]);
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
}
