//! Durable receipt admission and processing for inbound Standard Webhooks.
//!
//! This module verifies a configured endpoint through its [`Verifier`] before
//! it reaches PostgreSQL, then owns the one transaction that writes a receipt
//! and enqueues processing.
//! Processing invokes one explicit adopter callback and completes its fenced job
//! in that callback's transaction.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use http::HeaderMap;
use http::header::CONTENT_TYPE;
use infra_jobs::{
    CompleteError, EnqueueError, EnqueueOptions, Handler, Job, JobError, JobKind, Kinds, Policy,
    enqueue,
};
use infra_postgres::{
    Isolation, Tx, TxError, TxOptions, failure_cause, in_tx, in_tx_with, observed, sqlstate,
};
use serde::{Deserialize, Serialize};
use serde_with::base64::Base64;
use serde_with::serde_as;
use sqlx::postgres::PgPool;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::protocol::{KeyRing, MAX_MESSAGE_ID_BYTES, ProtocolError};

/// Re-exported so a consumer crate implements [`Consumer`] without its own
/// `async-trait` dependency, as `tonic::async_trait` does for services.
pub use async_trait::async_trait;

// Under REPEATABLE READ a concurrent `ON CONFLICT DO NOTHING` fails with 40001
// instead of reporting the duplicate, so admission pins READ COMMITTED.
const READ_COMMITTED: TxOptions = TxOptions {
    isolation: Isolation::ReadCommitted,
    read_only: false,
};

/// A sender retries one message ID with fresh timestamps for its whole retry
/// horizon. Standard Webhooks senders retry for more than a day; this
/// template's own outbound schedule runs about six and a half days before
/// jitter and any `Retry-After` floor, so receipts are kept for twice that.
/// The spec's 5-minute example only covers replay of one signed request.
const RECEIPT_RETENTION: Duration = Duration::from_hours(14 * 24);

#[allow(
    clippy::cast_possible_wrap,
    reason = "fourteen days of seconds is far inside i64"
)]
const RETENTION_SECONDS: i64 = RECEIPT_RETENTION.as_secs() as i64;

/// Cleanup cadence; the first run starts at once.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

/// The most rows one cleanup batch deletes.
const CLEANUP_BATCH_ROWS: i64 = 500;

/// Runs of the periodic receipt cleanup, by `outcome` (`completed`, `failed`).
pub const CLEANUP_RUNS_METRIC: &str = "webhook_receipt_cleanup_runs_total";

/// Expired receipts the cleanup deleted, counted per committed batch.
pub const CLEANUP_REMOVED_METRIC: &str = "webhook_receipt_cleanup_removed_receipts_total";

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
    /// No receiving binding owns the requested endpoint.
    #[error("inbound webhook endpoint is not configured")]
    UnknownEndpoint,
    /// The endpoint's verifier did not establish acceptable evidence.
    #[error("inbound webhook delivery was rejected: {}", .0.reason())]
    Rejected(Rejection),
    /// Receipt persistence or its commit acknowledgement was unavailable.
    #[error("inbound webhook receipt storage is unavailable")]
    Unavailable,
}

/// Why a [`Verifier`] refused a delivery.
///
/// The reason is a static label from a small closed set, safe as a log field
/// and a metric label. It never carries request, signature, or key bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rejection(&'static str);

impl Rejection {
    /// A rejection with a static `snake_case` reason label.
    #[must_use]
    pub const fn new(reason: &'static str) -> Self {
        Self(reason)
    }

    /// The reason label.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        self.0
    }
}

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
    /// provider's stable ID for this message, 1 to [`MAX_MESSAGE_ID_BYTES`]
    /// bytes, equal on every redelivery. `body` is the raw request body and
    /// `now` the admission time for replay windows.
    ///
    /// # Errors
    ///
    /// A [`Rejection`] when the evidence is missing, malformed, stale, or does
    /// not match the endpoint's key.
    fn verify(&self, headers: &HeaderMap, body: &[u8], now: SystemTime)
    -> Result<Bytes, Rejection>;
}

impl Verifier for KeyRing {
    fn verify(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
    ) -> Result<Bytes, Rejection> {
        let verified = KeyRing::verify(self, headers, body, now)?;
        Ok(verified.message_id().clone())
    }
}

/// A shared verifier, so one receiver can mix schemes as `Arc<dyn Verifier>`.
impl<V: Verifier + ?Sized> Verifier for Arc<V> {
    fn verify(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
    ) -> Result<Bytes, Rejection> {
        (**self).verify(headers, body, now)
    }
}

/// Configured receiving endpoints and their durable receipt store.
#[derive(Clone)]
pub struct Receiver {
    pool: PgPool,
    endpoints: Arc<HashMap<String, Arc<dyn Verifier>>>,
}

impl Receiver {
    /// Build a receiver over each configured endpoint's verifier. Construction does no I/O.
    #[must_use]
    pub fn new<V: Verifier>(
        pool: PgPool,
        endpoints: impl IntoIterator<Item = (String, V)>,
    ) -> Self {
        Self {
            pool,
            endpoints: Arc::new(
                endpoints
                    .into_iter()
                    .map(|(endpoint_id, verifier)| {
                        (endpoint_id, Arc::new(verifier) as Arc<dyn Verifier>)
                    })
                    .collect(),
            ),
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
    /// Returns a closed rejection for an unknown endpoint, refused evidence, or
    /// a message identity outside 1 to [`MAX_MESSAGE_ID_BYTES`] bytes, or
    /// unavailable when receipt ownership could not be acknowledged.
    pub async fn receive(
        &self,
        endpoint_id: &str,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
    ) -> Result<ReceiptOutcome, ReceiveError> {
        let Some(verifier) = self.endpoints.get(endpoint_id) else {
            return Err(ReceiveError::UnknownEndpoint);
        };
        let message_id = verifier
            .verify(headers, body, now)
            .and_then(|message_id| {
                // The receipt key is indexed, so a verifier's identity is bounded here.
                if message_id.is_empty() || message_id.len() > MAX_MESSAGE_ID_BYTES {
                    return Err(ProtocolError::InvalidMessageId.into());
                }
                Ok(message_id)
            })
            .map_err(|rejection| {
                tracing::info!(
                    webhook.endpoint = endpoint_id,
                    webhook.reason = rejection.reason(),
                    "webhook_delivery_rejected"
                );
                ReceiveError::Rejected(rejection)
            })?;
        let content_type = headers
            .get(CONTENT_TYPE)
            .map(|value| value.as_bytes().to_vec());
        let result = in_tx_with(
            &self.pool,
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
                if inserted.is_some() {
                    let incoming = Incoming::new(
                        endpoint_id,
                        message_id.as_ref(),
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
                let driver = error.driver();
                tracing::warn!(
                    webhook.endpoint = endpoint_id,
                    webhook.reason = reason,
                    sqlstate = driver.and_then(sqlstate).as_deref(),
                    cause = driver.map(failure_cause),
                    "webhook_receipt_unavailable"
                );
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
    /// The failure class of the batch that failed, logged with a bounded
    /// cause; earlier batches stay committed.
    pub async fn remove_expired(&self) -> Result<u64, CleanupError> {
        let mut removed = 0;
        loop {
            let batch = in_tx(&self.pool, async |tx| -> Result<u64, CleanupError> {
                // Bounds the batch on the server, so a batch whose client has
                // gone still ends within 1 s.
                observed(
                    "set statement timeout",
                    sqlx::query!("SET LOCAL statement_timeout = '1000ms'").execute(&mut *tx),
                )
                .await
                .map_err(|err| cleanup_failed(&err, CleanupError::Statement))?;
                // One batch of expired receipts: `$1` is the retention in
                // seconds and `$2` the batch size.
                let deleted = observed(
                    "delete expired webhook receipts",
                    sqlx::query!(
                        "DELETE FROM webhook_receipts WHERE (endpoint_id, message_id) IN \
                         (SELECT endpoint_id, message_id FROM webhook_receipts \
                         WHERE received_at < statement_timestamp() - $1::bigint * interval '1 second' \
                         ORDER BY received_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
                        RETENTION_SECONDS,
                        CLEANUP_BATCH_ROWS,
                    )
                    .execute(&mut *tx),
                )
                .await
                .map_err(|err| cleanup_failed(&err, CleanupError::Statement))?;
                Ok(deleted.rows_affected())
            })
            .await?;
            metrics::counter!(CLEANUP_REMOVED_METRIC).increment(batch);
            removed += batch;
            if batch < CLEANUP_BATCH_ROWS.unsigned_abs() {
                return Ok(removed);
            }
        }
    }

    /// The periodic cleanup task body: one [`Self::remove_expired`] run
    /// every 60 s, the first at once. Every run counts its outcome; a failed
    /// run waits for the next tick and changes neither readiness nor serving.
    /// Returns when `cancel` fires, dropping a run in flight.
    pub async fn run_cleanup(self, cancel: CancellationToken) {
        metrics::describe_counter!(
            CLEANUP_RUNS_METRIC,
            metrics::Unit::Count,
            "Runs of the expired webhook receipt cleanup, by outcome."
        );
        metrics::describe_counter!(
            CLEANUP_REMOVED_METRIC,
            metrics::Unit::Count,
            "Expired webhook receipts the cleanup deleted."
        );
        let _ = cancel
            .run_until_cancelled(async {
                let mut ticker = tokio::time::interval(CLEANUP_INTERVAL);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    let outcome = match self.remove_expired().await {
                        Ok(_) => "completed",
                        Err(_) => "failed",
                    };
                    metrics::counter!(CLEANUP_RUNS_METRIC, "outcome" => outcome).increment(1);
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

impl ReceiptFailure {
    /// The driver error behind this failure, when one caused it.
    fn driver(&self) -> Option<&sqlx::Error> {
        match self {
            Self::Query(err)
            | Self::Enqueue(EnqueueError::Database(err))
            | Self::Transaction(
                TxError::Acquire(err)
                | TxError::Begin(err)
                | TxError::CommitFailed(err)
                | TxError::CommitUnknown(err),
            ) => Some(err),
            Self::Enqueue(_) | Self::Integrity => None,
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

/// An adapter that applies one retained delivery inside its transaction.
///
/// `tx` stays open, and holds a pooled connection, until `process` returns
/// and the job completes in it. Keep `process` to database effects. For an
/// effect outside PostgreSQL, enqueue a job on `tx` and let that job make the
/// call, so a slow recipient holds neither a transaction nor a connection.
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
    ///
    /// # Errors
    ///
    /// Returns [`DuplicateConsumer`] when the endpoint already has a binding;
    /// the first binding stays.
    pub fn insert(
        &mut self,
        endpoint_id: impl Into<String>,
        consumer: Arc<dyn Consumer>,
    ) -> Result<(), DuplicateConsumer> {
        match self.entries.entry(endpoint_id.into()) {
            Entry::Occupied(bound) => Err(DuplicateConsumer {
                endpoint: bound.key().clone(),
            }),
            Entry::Vacant(unbound) => {
                unbound.insert(consumer);
                Ok(())
            }
        }
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

/// An inbound endpoint bound to a consumer twice.
#[derive(Debug, thiserror::Error)]
#[error("inbound webhook endpoint {endpoint} already has a consumer binding")]
pub struct DuplicateConsumer {
    /// The endpoint ID bound twice.
    pub endpoint: String,
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
                    webhook.endpoint = job.payload().endpoint_id(),
                    "webhook_processor_missing_binding"
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
                Err(ProcessFailure::Consumer(error)) => {
                    // Every endpoint shares one job kind, so the attempt's own
                    // event that follows cannot say whose consumer this was.
                    tracing::info!(
                        webhook.endpoint = job.payload().endpoint_id(),
                        permanent = error.is_permanent(),
                        "webhook_consumer_incomplete"
                    );
                    Err(error)
                }
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

impl From<TxError> for CleanupError {
    fn from(err: TxError) -> Self {
        match &err {
            TxError::Acquire(err) => cleanup_failed(err, Self::Acquire),
            TxError::Begin(err) => cleanup_failed(err, Self::Begin),
            TxError::CommitFailed(err) | TxError::CommitUnknown(err) => {
                cleanup_failed(err, Self::Commit)
            }
        }
    }
}

/// Log a failed cleanup batch with only bounded fields, never driver text,
/// and return its class.
fn cleanup_failed(err: &sqlx::Error, class: CleanupError) -> CleanupError {
    tracing::warn!(
        failure = %class,
        sqlstate = sqlstate(err).as_deref(),
        cause = failure_cause(err),
        "webhook_receipt_cleanup_failed"
    );
    class
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_metrics::CounterKeys;

    struct Ignore;

    #[async_trait]
    impl Consumer for Ignore {
        async fn process(&self, _tx: &mut Tx<'_>, _incoming: &Incoming) -> Result<(), JobError> {
            Ok(())
        }
    }

    #[test]
    fn a_second_binding_for_one_endpoint_is_refused_and_an_unbound_one_is_named() {
        let mut consumers = Consumers::new();
        consumers
            .insert("partner", Arc::new(Ignore))
            .expect("first binding");
        let duplicate = consumers
            .insert("partner", Arc::new(Ignore))
            .expect_err("second binding");
        assert_eq!(duplicate.endpoint, "partner");

        assert!(consumers.require(["partner"]).is_ok());
        let missing = consumers
            .require(["partner", "other"])
            .expect_err("unbound endpoint");
        assert_eq!(missing.endpoint, "other");
    }

    #[tokio::test]
    async fn a_cleanup_run_without_a_database_counts_as_failed_and_removes_nothing() {
        let keys = CounterKeys::default();
        let _local = metrics::set_default_local_recorder(&keys);
        // Nothing listens on port 1, so every acquire ends at the pool's bound.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("postgres://127.0.0.1:1/unreachable")
            .expect("a lazy pool opens no connection");
        let receiver = Receiver::new(pool, Vec::<(String, KeyRing)>::new());
        assert_eq!(receiver.remove_expired().await, Err(CleanupError::Acquire));

        let failed = async {
            loop {
                let counted = keys.0.lock().expect("keys").iter().any(|key| {
                    key.name() == CLEANUP_RUNS_METRIC
                        && key
                            .labels()
                            .any(|label| (label.key(), label.value()) == ("outcome", "failed"))
                });
                if counted {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::select! {
            () = receiver.clone().run_cleanup(CancellationToken::new()) => {
                panic!("the cleanup task ends only on cancel")
            }
            () = failed => {}
        }
        assert!(
            keys.0
                .lock()
                .expect("keys")
                .iter()
                .all(|key| key.name() != CLEANUP_REMOVED_METRIC),
            "a failed batch removed nothing"
        );
    }
}
