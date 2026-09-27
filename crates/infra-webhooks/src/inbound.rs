//! Durable receipt admission and processing for inbound Standard Webhooks.
//!
//! This module verifies a configured endpoint before it reaches PostgreSQL,
//! then owns the one transaction that writes a receipt and enqueues processing.
//! Processing invokes one explicit adopter callback and completes its fenced job
//! in that callback's transaction.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use http::HeaderMap;
use http::header::CONTENT_TYPE;
use infra_jobs::{
    CompleteError, EnqueueError, EnqueueOptions, Handler, Job, JobError, JobKind, Kinds, Policy,
    enqueue,
};
use infra_postgres::{Isolation, Tx, TxError, TxOptions, connection, in_tx, in_tx_with};
use serde::{Deserialize, Serialize};
use serde_with::base64::Base64;
use serde_with::serde_as;
use sqlx::postgres::PgPool;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::protocol::KeyRing;

/// Re-exported so a consumer crate implements [`Consumer`] without its own
/// `async-trait` dependency, as `tonic::async_trait` does for services.
pub use async_trait::async_trait;

// Under REPEATABLE READ a concurrent `ON CONFLICT DO NOTHING` fails with 40001
// instead of reporting the duplicate, so admission pins READ COMMITTED.
const READ_COMMITTED: TxOptions = TxOptions {
    isolation: Isolation::ReadCommitted,
    read_only: false,
};

const INSERT_RECEIPT: &str = "INSERT INTO webhook_receipts (endpoint_id, message_id) \
    VALUES ($1, $2) \
    ON CONFLICT (endpoint_id, message_id) DO NOTHING \
    RETURNING message_id";

/// A sender retries one message ID with fresh timestamps for its whole retry
/// horizon. Standard Webhooks senders retry for more than a day; this
/// template's own outbound schedule runs about six days, so receipts must
/// outlive that horizon. The spec's 5-minute example only covers replay of
/// one signed request.
const RECEIPT_RETENTION: Duration = Duration::from_hours(7 * 24);

/// Cleanup cadence; the first run starts at once.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

/// The most rows one cleanup batch deletes: the `LIMIT` in [`CLEANUP_BATCH`].
const CLEANUP_BATCH_ROWS: u64 = 500;

/// Bounds a cleanup batch on the server, so a batch whose client has gone
/// still ends within 1 s.
const CLEANUP_STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '1000ms'";

/// One batch of expired receipts. The `interval '7 days'` is [`RECEIPT_RETENTION`].
const CLEANUP_BATCH: &str = "DELETE FROM webhook_receipts WHERE (endpoint_id, message_id) IN \
    (SELECT endpoint_id, message_id FROM webhook_receipts \
    WHERE received_at < statement_timestamp() - interval '7 days' \
    ORDER BY received_at LIMIT 500 FOR UPDATE SKIP LOCKED)";

const _: () = assert!(RECEIPT_RETENTION.as_secs() == 7 * 24 * 60 * 60);

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
        let verified = keys
            .verify(headers, body, now)
            .map_err(|_| ReceiveError::Rejected)?;
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

    /// Delete expired receipts in batches of at most 500 until a batch
    /// deletes fewer, and return how many were deleted. Each batch is its
    /// own transaction with a 1 s statement timeout, and skips receipts a
    /// concurrent admission holds.
    ///
    /// # Errors
    ///
    /// The failure class of the batch that failed; earlier batches stay
    /// committed.
    pub async fn remove_expired(&self) -> Result<u64, CleanupError> {
        let mut removed = 0;
        loop {
            let batch = in_tx(&self.pool, async |tx| -> Result<u64, Failed> {
                sqlx::query(CLEANUP_STATEMENT_TIMEOUT)
                    .execute(connection(tx))
                    .await
                    .map_err(|_| Failed(CleanupError::Statement))?;
                let deleted = sqlx::query(CLEANUP_BATCH)
                    .execute(connection(tx))
                    .await
                    .map_err(|_| Failed(CleanupError::Statement))?;
                Ok(deleted.rows_affected())
            })
            .await
            .map_err(|Failed(failure)| failure)?;
            removed += batch;
            if batch < CLEANUP_BATCH_ROWS {
                return Ok(removed);
            }
        }
    }

    /// The periodic cleanup task body: one [`Self::remove_expired`] run
    /// every 60 s, the first at once. A failed run logs its class and waits
    /// for the next tick; it changes neither readiness nor serving. Returns
    /// when `cancel` fires, dropping a run in flight.
    pub async fn run_cleanup(self, cancel: CancellationToken) {
        let _ = cancel
            .run_until_cancelled(async {
                let mut ticker = tokio::time::interval(CLEANUP_INTERVAL);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    if let Err(failure) = self.remove_expired().await {
                        tracing::warn!(failure = %failure, "webhook_receipt_cleanup_failed");
                    }
                }
            })
            .await;
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
///
/// Unknown fields are ignored so a queued row written with `"version": 1`
/// still decodes.
#[serde_as]
#[derive(Clone, Deserialize, Serialize)]
pub struct Incoming {
    #[serde(skip_deserializing, default = "incoming_version")]
    version: u8,
    endpoint_id: String,
    #[serde_as(as = "Base64")]
    message_id: Vec<u8>,
    #[serde_as(as = "Option<Base64>")]
    content_type: Option<Vec<u8>>,
    #[serde_as(as = "Base64")]
    body: Vec<u8>,
}

const fn incoming_version() -> u8 {
    1
}

impl Incoming {
    fn new(endpoint_id: &str, message_id: &[u8], content_type: Option<&[u8]>, body: &[u8]) -> Self {
        Self {
            version: incoming_version(),
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
///
/// Implement it under [`async_trait`](macro@async_trait), the workspace idiom
/// for object-safe async traits.
#[async_trait]
pub trait Consumer: Send + Sync + 'static {
    /// Apply `incoming` through the adopter's business boundary.
    async fn process(&self, tx: &mut Tx<'_>, incoming: &Incoming) -> Result<(), JobError>;
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

    /// Fail when a configured endpoint has no consumer binding.
    ///
    /// # Errors
    ///
    /// Returns [`MissingConsumer`] for the first unbound endpoint.
    pub fn require<'a>(
        &self,
        endpoint_ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), MissingConsumer> {
        for endpoint in endpoint_ids {
            if !self.entries.contains_key(endpoint) {
                return Err(MissingConsumer {
                    endpoint: endpoint.to_owned(),
                });
            }
        }
        Ok(())
    }

    fn get(&self, endpoint_id: &str) -> Option<Arc<dyn Consumer>> {
        self.entries.get(endpoint_id).cloned()
    }
}

/// A configured inbound endpoint with no consumer binding.
#[derive(Debug, thiserror::Error)]
#[error("inbound webhook endpoint {endpoint} has no consumer binding")]
pub struct MissingConsumer {
    /// The unbound endpoint ID.
    pub endpoint: String,
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

    /// Register `webhooks.process` with the default jobs policy.
    pub fn register(self, kinds: &mut Kinds) -> &mut Kinds {
        kinds.register(Policy::default(), self)
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
        let consumer = self.consumers.get(job.payload().endpoint_id());
        async move {
            let Some(consumer) = consumer else {
                tracing::warn!(
                    event = "webhook_processor_missing_binding",
                    reason = "missing_binding"
                );
                return Err(JobError::retryable(
                    "inbound webhook consumer is unavailable",
                ));
            };
            let pool = job.pool().clone();
            let completed = in_tx(&pool, async |tx| -> Result<(), ProcessFailure> {
                consumer
                    .process(tx, job.payload())
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
                Err(other) => Err(JobError::retryable(other)),
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ProcessFailure {
    #[error("inbound webhook consumer failed")]
    Consumer(JobError),
    #[error("inbound webhook completion failed")]
    Completion(CompleteError),
    #[error("inbound webhook processing is unavailable")]
    Transaction(TxError),
}

impl From<TxError> for ProcessFailure {
    fn from(error: TxError) -> Self {
        Self::Transaction(error)
    }
}

/// The failure class of one receipt cleanup run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CleanupError {
    #[error("acquire")]
    Acquire,
    #[error("begin")]
    Begin,
    #[error("statement")]
    Statement,
    #[error("commit")]
    Commit,
}

/// A failed cleanup batch, by class, which leaves the batch's transaction as
/// an error.
struct Failed(CleanupError);

impl From<TxError> for Failed {
    fn from(err: TxError) -> Self {
        Self(match err {
            TxError::Acquire(_) => CleanupError::Acquire,
            TxError::Begin(_) => CleanupError::Begin,
            TxError::CommitFailed(_) | TxError::CommitUnknown(_) => CleanupError::Commit,
        })
    }
}
