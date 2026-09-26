//! Durable receipt admission and processing for inbound Standard Webhooks.
//!
//! This module verifies a configured endpoint before it reaches PostgreSQL,
//! then owns the one transaction that writes a receipt and enqueues processing.
//! Processing invokes one explicit adopter callback and completes its fenced job
//! in that callback's transaction.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::SystemTime;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::HeaderMap;
use http::header::CONTENT_TYPE;
use infra_jobs::{
    CompleteError, EnqueueError, EnqueueOptions, Handler, Job, JobError, JobKind, enqueue,
};
use infra_postgres::{Isolation, Tx, TxError, TxOptions, connection, in_tx, in_tx_with};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sqlx::postgres::PgPool;

use crate::protocol::{KeyRing, MAX_BODY_BYTES, verify};

const INCOMING_VERSION: u8 = 1;
const READ_COMMITTED: TxOptions = TxOptions {
    isolation: Isolation::ReadCommitted,
    read_only: false,
};

const INSERT_RECEIPT: &str = "INSERT INTO webhook_receipts (endpoint_id, message_id) \
    VALUES ($1, $2) \
    ON CONFLICT (endpoint_id, message_id) DO NOTHING \
    RETURNING message_id";

/// The durable admission result for one verified delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptOutcome {
    /// The receipt and processing job committed together.
    Accepted,
    /// The endpoint and message identity were already retained; first admission wins.
    Duplicate,
}

/// A closed inbound admission failure for the HTTP adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveError {
    /// No receiving binding owns the requested endpoint.
    UnknownEndpoint,
    /// Verification did not establish acceptable Standard Webhooks evidence.
    Rejected,
    /// Receipt persistence or its commit acknowledgement was unavailable.
    Unavailable,
}

/// Configured receiving endpoints and their durable receipt store.
#[derive(Clone)]
pub struct Receiver {
    pool: PgPool,
    endpoints: Arc<HashMap<String, KeyRing>>,
}

impl Receiver {
    /// Build a receiver over the configured endpoint keys. Construction does no I/O.
    #[must_use]
    pub fn new(pool: PgPool, endpoints: impl IntoIterator<Item = (String, KeyRing)>) -> Self {
        Self {
            pool,
            endpoints: Arc::new(endpoints.into_iter().collect()),
        }
    }

    /// Whether an endpoint has a configured receiving binding.
    #[must_use]
    pub fn has_endpoint(&self, endpoint_id: &str) -> bool {
        self.endpoints.contains_key(endpoint_id)
    }

    /// Verify and atomically retain an inbound delivery.
    ///
    /// A duplicate must pass verification again. A successful result means the
    /// receipt transaction was acknowledged; it never claims the consumer ran.
    ///
    /// # Errors
    ///
    /// Returns a closed rejection for an unknown endpoint or invalid signature,
    /// or unavailable when receipt ownership could not be acknowledged.
    ///
    /// # Errors
    ///
    /// Returns a closed error when the endpoint is unknown, verification is
    /// rejected, or the durable transaction is unavailable or uncertain.
    pub async fn receive(
        &self,
        endpoint_id: &str,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
    ) -> Result<ReceiptOutcome, ReceiveError> {
        let Some(keys) = self.endpoints.get(endpoint_id) else {
            return Err(ReceiveError::UnknownEndpoint);
        };
        let verified = verify(keys, headers, body, now).map_err(|_| ReceiveError::Rejected)?;
        if verified.message_id().len() > 255 {
            return Err(ReceiveError::Rejected);
        }
        let content_type = headers
            .get(CONTENT_TYPE)
            .map(|value| value.as_bytes().to_vec());
        let result = in_tx_with(
            &self.pool,
            READ_COMMITTED,
            async |tx| -> Result<ReceiptOutcome, ReceiptFailure> {
                let inserted = sqlx::query_scalar::<_, Vec<u8>>(INSERT_RECEIPT)
                    .bind(endpoint_id)
                    .bind(verified.message_id().as_ref())
                    .fetch_optional(&mut *connection(tx))
                    .await?;
                if inserted.is_some() {
                    let incoming = Incoming::new(
                        endpoint_id,
                        verified.message_id().as_ref(),
                        content_type.as_deref(),
                        body,
                    );
                    if !matches!(
                        enqueue(tx, &incoming, EnqueueOptions::default()).await?,
                        infra_jobs::Enqueued::Created(_)
                    ) {
                        return Err(ReceiptFailure::Integrity);
                    }
                    return Ok(ReceiptOutcome::Accepted);
                }

                Ok(ReceiptOutcome::Duplicate)
            },
        )
        .await;
        match result {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let reason = match error {
                    ReceiptFailure::Integrity => "integrity",
                    ReceiptFailure::Transaction(TxError::CommitUnknown(_)) => "commit_unknown",
                    ReceiptFailure::Query(_)
                    | ReceiptFailure::Enqueue(_)
                    | ReceiptFailure::Transaction(_) => "unavailable",
                };
                tracing::warn!(event = "webhook_receipt_unavailable", reason);
                Err(ReceiveError::Unavailable)
            }
        }
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
    #[error("webhook receipt integrity was unavailable")]
    Integrity,
}

/// The retained payload of a verified inbound delivery.
#[derive(Clone, Deserialize, Serialize)]
pub struct Incoming {
    version: u8,
    endpoint_id: String,
    #[serde(with = "base64_bytes")]
    message_id: Vec<u8>,
    #[serde(with = "optional_base64_bytes")]
    content_type: Option<Vec<u8>>,
    #[serde(with = "base64_bytes")]
    body: Vec<u8>,
}

impl Incoming {
    fn new(endpoint_id: &str, message_id: &[u8], content_type: Option<&[u8]>, body: &[u8]) -> Self {
        Self {
            version: INCOMING_VERSION,
            endpoint_id: endpoint_id.to_owned(),
            message_id: message_id.to_vec(),
            content_type: content_type.map(ToOwned::to_owned),
            body: body.to_vec(),
        }
    }

    /// The configured endpoint that accepted the delivery.
    #[must_use]
    pub fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    /// The original Standard Webhooks message identifier bytes.
    #[must_use]
    pub fn message_id(&self) -> &[u8] {
        &self.message_id
    }

    /// The first accepted Content-Type header bytes, when present.
    #[must_use]
    pub fn content_type(&self) -> Option<&[u8]> {
        self.content_type.as_deref()
    }

    /// The verified raw delivery body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    fn is_valid(&self) -> bool {
        self.version == INCOMING_VERSION
            && !self.endpoint_id.is_empty()
            && !self.endpoint_id.contains('\0')
            && !self.message_id.is_empty()
            && self.body.len() <= MAX_BODY_BYTES
    }
}

impl fmt::Debug for Incoming {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Incoming")
            .field("version", &self.version)
            .field("endpoint_id", &"[REDACTED]")
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

/// A provider adapter that applies one retained delivery inside its transaction.
pub trait Consumer: Send + Sync + 'static {
    /// Apply `incoming` through the adopter's business boundary.
    fn process<'a>(
        &'a self,
        tx: &'a mut Tx<'_>,
        incoming: &'a Incoming,
    ) -> Pin<Box<dyn Future<Output = Result<(), JobError>> + Send + 'a>>;
}

/// Explicit endpoint-to-consumer bindings for a processing worker.
#[derive(Default)]
pub struct Consumers {
    entries: HashMap<String, Arc<dyn Consumer>>,
}

impl Consumers {
    /// Start with no consumer bindings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind one configured endpoint to its consumer.
    pub fn insert(
        &mut self,
        endpoint_id: impl Into<String>,
        consumer: Arc<dyn Consumer>,
    ) -> Option<Arc<dyn Consumer>> {
        self.entries.insert(endpoint_id.into(), consumer)
    }

    /// Whether an explicit consumer binding exists for `endpoint_id`.
    #[must_use]
    pub fn contains(&self, endpoint_id: &str) -> bool {
        self.entries.contains_key(endpoint_id)
    }

    fn get(&self, endpoint_id: &str) -> Option<Arc<dyn Consumer>> {
        self.entries.get(endpoint_id).cloned()
    }
}

impl fmt::Debug for Consumers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Consumers")
            .field("binding_count", &self.entries.len())
            .finish()
    }
}

/// The jobs handler that invokes one bound inbound consumer.
pub struct Processor {
    consumers: Consumers,
}

impl Processor {
    /// Build a processor over explicit endpoint bindings.
    #[must_use]
    pub fn new(consumers: Consumers) -> Self {
        Self { consumers }
    }
}

impl fmt::Debug for Processor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Processor")
            .field("consumers", &self.consumers)
            .finish()
    }
}

impl Handler<Incoming> for Processor {
    fn run(&self, job: Job<Incoming>) -> impl Future<Output = Result<(), JobError>> + Send {
        let incoming = job.payload().clone();
        let consumer = self.consumers.get(incoming.endpoint_id());
        async move {
            if !incoming.is_valid() {
                return Err(JobError::permanent("invalid inbound webhook payload"));
            }
            let Some(consumer) = consumer else {
                tracing::warn!(
                    event = "webhook_processor_missing_binding",
                    reason = "missing_binding"
                );
                return Err(JobError::retryable("inbound webhook consumer is unavailable"));
            };
            let pool = job.pool().clone();
            let completed = in_tx(&pool, async |tx| -> Result<(), ProcessFailure> {
                consumer
                    .process(tx, &incoming)
                    .await
                    .map_err(ProcessFailure::Consumer)?;
                job.complete_in_tx(tx)
                    .await
                    .map_err(ProcessFailure::Completion)?;
                Ok(())
            })
            .await;
            match completed {
                Ok(()) => Ok(()),
                Err(ProcessFailure::Consumer(error)) => Err(error),
                Err(ProcessFailure::Completion(CompleteError::StaleClaim)) => Err(
                    JobError::retryable("inbound webhook completion became stale"),
                ),
                Err(ProcessFailure::Completion(CompleteError::Database(_))) => Err(
                    JobError::retryable("inbound webhook completion is unavailable"),
                ),
                Err(ProcessFailure::Transaction(TxError::CommitUnknown(_))) => {
                    Err(JobError::transaction_unknown(
                        "inbound webhook processing commit outcome is unknown",
                    ))
                }
                Err(ProcessFailure::Transaction(_)) => Err(JobError::retryable(
                    "inbound webhook processing is unavailable",
                )),
            }
        }
    }
}

#[derive(Debug)]
enum ProcessFailure {
    Consumer(JobError),
    Completion(CompleteError),
    Transaction(TxError),
}

impl From<TxError> for ProcessFailure {
    fn from(error: TxError) -> Self {
        Self::Transaction(error)
    }
}

mod base64_bytes {
    use super::*;

    pub(super) fn serialize<S>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(value))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(D::Error::custom)
    }
}

mod optional_base64_bytes {
    use super::*;

    #[allow(
        clippy::ref_option,
        reason = "serde passes a reference to the optional field"
    )]
    pub(super) fn serialize<S>(value: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_some(&STANDARD.encode(value)),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer)?
            .map(|encoded| STANDARD.decode(encoded).map_err(D::Error::custom))
            .transpose()
    }
}
