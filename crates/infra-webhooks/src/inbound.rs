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

use base64::display::Base64Display;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue};
use infra_jobs::{
    CompleteError, EnqueueError, EnqueueOptions, Enqueued, Handler, Job, JobError, JobKind, Kinds,
    Policy, enqueue,
};
use infra_postgres::{
    Cleanup, CleanupPass, CleanupSchedule, Isolation, MAINTENANCE_OBSERVATION_INTERVAL,
    MaintenanceFailure, MaintenanceObserver, MaintenancePopulation, PopulationSample, Tx, TxError,
    TxOptions, failure_cause, in_observation_tx, in_tx, in_tx_with, observed, sqlstate,
};
use serde::{Deserialize, Serialize, Serializer};
use serde_with::base64::Base64;
use serde_with::{SerializeAs, serde_as};
use sqlx::postgres::PgPool;
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
/// template's nominal outbound backoff runs about six and a half days. The
/// fixed admission window does not cover every retry floor, outage, redrive,
/// or restore; consumers retain business effect identity for their replay lifetime.
/// The spec's 5-minute example only covers replay of one signed request.
const RECEIPT_RETENTION: Duration = Duration::from_hours(14 * 24);

#[expect(
    clippy::cast_possible_wrap,
    reason = "fourteen days of seconds is far inside i64"
)]
const RETENTION_SECONDS: i64 = RECEIPT_RETENTION.as_secs() as i64;

const CLEANUP_BATCH_ROWS: i64 = 500;

/// Periodic receipt cleanup runs, by `completed` or `failed`.
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
    ///
    /// # Errors
    ///
    /// Returns a closed rejection for an unknown endpoint, refused evidence, or
    /// a message identity outside 1 to [`MAX_MESSAGE_ID_BYTES`] bytes, or
    /// unavailable when receipt ownership could not be acknowledged.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub async fn receive(
        &self,
        endpoint_id: &str,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
    ) -> Result<ReceiptOutcome, ReceiveError> {
        self.receive_inner(endpoint_id, headers, ReceiveBody::Borrowed(body), now)
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
    ) -> Result<ReceiptOutcome, ReceiveError> {
        self.receive_inner(endpoint_id, headers, ReceiveBody::Owned(body), now)
            .await
    }

    async fn receive_inner(
        &self,
        endpoint_id: &str,
        headers: &HeaderMap,
        body: ReceiveBody<'_>,
        now: SystemTime,
    ) -> Result<ReceiptOutcome, ReceiveError> {
        let Some(verifier) = self.endpoints.get(endpoint_id) else {
            return Err(ReceiveError::UnknownEndpoint);
        };
        let message_id = verifier
            .authenticate(headers, body.as_ref(), now)
            .and_then(bounded_message_id)
            .map_err(|rejection| {
                tracing::info!(
                    webhook.endpoint = endpoint_id,
                    webhook.reason = rejection.reason(),
                    "webhook_delivery_rejected"
                );
                ReceiveError::Rejected(rejection)
            })?;
        let content_type = headers.get(CONTENT_TYPE).map(HeaderValue::as_bytes);
        in_tx_with(
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
                if inserted.is_none() {
                    return Ok(ReceiptOutcome::Duplicate);
                }
                let incoming = Incoming::new(endpoint_id, message_id.as_ref(), content_type, body);
                match enqueue(tx, &incoming, EnqueueOptions::default()).await? {
                    Enqueued::Created(_) => Ok(ReceiptOutcome::Accepted),
                    Enqueued::Duplicate => Err(ReceiptFailure::UnexpectedDuplicate),
                }
            },
        )
        .await
        .map_err(|error| {
            let driver = error.driver();
            tracing::warn!(
                webhook.endpoint = endpoint_id,
                webhook.reason = error.reason(),
                sqlstate = driver.and_then(sqlstate).as_deref(),
                cause = driver.map(failure_cause),
                "webhook_receipt_unavailable"
            );
            ReceiveError::Unavailable
        })
    }

    /// Delete expired receipts in batches of at most 500, returning confirmed
    /// progress after a short batch. Each batch is its own transaction with a
    /// 5 s statement timeout and skips receipts another cleanup holds; an
    /// admission's `ON CONFLICT DO NOTHING` locks no existing receipt.
    /// Confirmed counts do not certify an empty expired population.
    ///
    /// # Errors
    ///
    /// The failure class of the batch that failed, logged with a bounded
    /// cause; earlier batches stay committed.
    pub async fn remove_expired(&self) -> Result<u64, CleanupError> {
        self.remove_expired_pass(&CancellationToken::new())
            .await
            .map(|(removed, _)| removed)
    }

    async fn remove_expired_pass(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(u64, CleanupDisposition), CleanupError> {
        let mut pass = CleanupPass::start(Cleanup::WebhookReceipts);
        let mut removed = 0;
        loop {
            if cancel.is_cancelled() {
                return Ok((removed, CleanupDisposition::Cancelled));
            }
            let batch = in_tx(&self.pool, async |tx| -> Result<u64, CleanupError> {
                // Bounds the batch on the server, so a batch whose client has
                // gone still ends within 5 s. A hang guard, not a pace: a slow
                // batch is waited for.
                observed(
                    "set statement timeout",
                    sqlx::query!("SELECT set_config('statement_timeout', $1, true)", "5s")
                        .fetch_one(&mut *tx),
                )
                .await
                .map_err(|err| cleanup_failed(&err, CleanupError::Statement))?;
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
            .await
            .inspect_err(|_| pass.failed())?;
            pass.committed(batch);
            metrics::counter!(CLEANUP_REMOVED_METRIC).increment(batch);
            removed += batch;
            if batch < CLEANUP_BATCH_ROWS.unsigned_abs() {
                pass.completed();
                return Ok((removed, CleanupDisposition::Completed));
            }
        }
    }

    /// Run periodic cleanup and independent population observation in the same
    /// tracked owner. Neither failure changes readiness. Cancellation drops
    /// both loops, including any transaction in flight.
    pub async fn run_cleanup(self, cancel: CancellationToken) {
        if cancel.is_cancelled() {
            return;
        }
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
        let _ = Box::pin(cancel.run_until_cancelled(async {
            tokio::join!(self.cleanup_loop(&cancel), self.observe_population(&cancel));
        }))
        .await;
    }

    async fn cleanup_loop(&self, cancel: &CancellationToken) {
        let mut schedule = CleanupSchedule::new(Cleanup::WebhookReceipts);
        loop {
            if cancel.is_cancelled() {
                return;
            }
            schedule.next().await;
            if cancel.is_cancelled() {
                return;
            }
            let outcome = match self.remove_expired_pass(cancel).await {
                Ok((_, CleanupDisposition::Cancelled)) => return,
                Ok((_, CleanupDisposition::Completed)) => "completed",
                Err(_) => "failed",
            };
            metrics::counter!(CLEANUP_RUNS_METRIC, "outcome" => outcome).increment(1);
        }
    }

    async fn observe_population(&self, cancel: &CancellationToken) {
        let mut observer = MaintenanceObserver::start(MaintenancePopulation::WebhookReceipts);
        loop {
            if cancel.is_cancelled() {
                return;
            }
            observer.sample(self.sample_population()).await;
            if cancel.is_cancelled() {
                return;
            }
            tokio::time::sleep(MAINTENANCE_OBSERVATION_INTERVAL).await;
        }
    }

    async fn sample_population(&self) -> Result<PopulationSample, MaintenanceFailure> {
        in_observation_tx(&self.pool, async |tx| {
            let sample = observed(
                "observe expired webhook receipts",
                sqlx::query!(
                    r#"SELECT extract(epoch FROM observation.observed_at)::float8 AS "observed_at!",
                                  extract(epoch FROM oldest.received_at + $1::bigint * interval '1 second')::float8 AS "oldest?"
                           FROM (SELECT statement_timestamp() AS observed_at) observation
                           LEFT JOIN LATERAL (
                               SELECT received_at FROM webhook_receipts
                               WHERE received_at < observation.observed_at - $1::bigint * interval '1 second'
                               ORDER BY received_at LIMIT 1
                           ) oldest ON true"#,
                    RETENTION_SECONDS,
                )
                .fetch_one(&mut *tx),
            )
            .await?;
            Ok(PopulationSample {
                observed_at: sample.observed_at,
                oldest: sample.oldest,
            })
        })
        .await
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
}

impl ReceiptFailure {
    /// The static `webhook.reason` log label.
    const fn reason(&self) -> &'static str {
        match self {
            Self::UnexpectedDuplicate => "integrity",
            Self::Transaction(TxError::CommitUnknown(_)) => "commit_unknown",
            Self::Query(_) | Self::Enqueue(_) | Self::Transaction(_) => "unavailable",
        }
    }

    fn driver(&self) -> Option<&sqlx::Error> {
        match self {
            Self::Query(err) | Self::Enqueue(EnqueueError::Database(err)) => Some(err),
            Self::Transaction(err) => Some(err.driver()),
            Self::Enqueue(_) | Self::UnexpectedDuplicate => None,
        }
    }
}

/// The receipt key is indexed, so a verifier's identity is bounded here.
fn bounded_message_id(message_id: Bytes) -> Result<Bytes, Rejection> {
    if message_id.is_empty() || message_id.len() > MAX_MESSAGE_ID_BYTES {
        return Err(ProtocolError::InvalidMessageId.into());
    }
    Ok(message_id)
}

enum ReceiveBody<'a> {
    Borrowed(&'a [u8]),
    Owned(Bytes),
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
    message_id: Vec<u8>,
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
    fn new(
        endpoint_id: &str,
        message_id: &[u8],
        content_type: Option<&[u8]>,
        body: ReceiveBody<'_>,
    ) -> Self {
        Self {
            version: incoming_version(),
            endpoint_id: endpoint_id.to_owned(),
            message_id: message_id.to_vec(),
            content_type: content_type.map(ToOwned::to_owned),
            body: match body {
                ReceiveBody::Borrowed(body) => Bytes::copy_from_slice(body),
                ReceiveBody::Owned(body) => body,
            },
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
    bindings: HashMap<String, Arc<dyn Consumer>>,
}

impl Consumers {
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
    pub fn bind(
        &mut self,
        endpoint_id: impl Into<String>,
        consumer: Arc<dyn Consumer>,
    ) -> Result<(), DuplicateConsumer> {
        match self.bindings.entry(endpoint_id.into()) {
            Entry::Occupied(bound) => Err(DuplicateConsumer {
                endpoint_id: bound.key().clone(),
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
        for endpoint_id in endpoint_ids {
            if !self.bindings.contains_key(endpoint_id) {
                return Err(MissingConsumer {
                    endpoint_id: endpoint_id.to_owned(),
                });
            }
        }
        Ok(())
    }

    fn get(&self, endpoint_id: &str) -> Option<&dyn Consumer> {
        self.bindings.get(endpoint_id).map(AsRef::as_ref)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("inbound webhook endpoint {endpoint_id} already has a consumer binding")]
pub struct DuplicateConsumer {
    pub endpoint_id: String,
}

#[derive(Debug, thiserror::Error)]
#[error("inbound webhook endpoint {endpoint_id} has no consumer binding")]
pub struct MissingConsumer {
    pub endpoint_id: String,
}

impl fmt::Debug for Consumers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Consumers")
            .field("binding_count", &self.bindings.len())
            .finish()
    }
}

/// The jobs handler that invokes one bound inbound consumer.
pub struct Processor {
    consumers: Consumers,
}

impl Processor {
    #[must_use]
    pub fn new(consumers: Consumers) -> Self {
        Self { consumers }
    }

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
    async fn run(&self, job: Job<Incoming>) -> Result<(), JobError> {
        let Some(consumer) = self.consumers.get(job.payload().endpoint_id()) else {
            tracing::warn!(
                webhook.endpoint = job.payload().endpoint_id(),
                "webhook_processor_missing_binding"
            );
            return Err(JobError::retryable(
                "inbound webhook consumer is unavailable",
            ));
        };
        let completed = in_tx(job.pool(), async |tx| -> Result<(), ProcessFailure> {
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

#[derive(Clone, Copy, Debug)]
enum CleanupDisposition {
    Completed,
    Cancelled,
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
            .bind("partner", Arc::new(Ignore))
            .expect("first binding");
        let duplicate = consumers
            .bind("partner", Arc::new(Ignore))
            .expect_err("second binding");
        assert_eq!(duplicate.endpoint_id, "partner");

        assert!(consumers.require(["partner"]).is_ok());
        let missing = consumers
            .require(["partner", "other"])
            .expect_err("unbound endpoint");
        assert_eq!(missing.endpoint_id, "other");
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
        let receiver = Receiver::new(pool, []);
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
