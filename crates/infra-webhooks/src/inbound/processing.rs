//! The worker side: endpoint-to-consumer bindings and the jobs handler that
//! applies one retained delivery.

use std::collections::hash_map::Entry;
use std::fmt;
use std::sync::Arc;

use foldhash::HashMap;
use infra_jobs::{CompleteError, Handler, Job, JobError, KindsBuilder, Policy};
use infra_postgres::{Tx, TxError, TxOptions, in_tx_with_acquire_deadline};

use super::{Incoming, async_trait};

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
    ///
    /// # Errors
    ///
    /// - An `Err` rolls back every effect made on `tx`.
    /// - The [`JobError`] class decides whether the job is retried.
    /// - Do not complete the job: [`Processor`] completes it in the same
    ///   transaction after `Ok`.
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

    pub fn register(self, kinds: &mut KindsBuilder) -> &mut KindsBuilder {
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
        let endpoint_id = job.payload().endpoint_id();
        let Some(consumer) = self.consumers.get(endpoint_id) else {
            tracing::warn!(
                webhook.endpoint = endpoint_id,
                webhook.reason = "missing_binding",
                "webhook_processor_missing_binding"
            );
            return Err(JobError::retryable(
                "inbound webhook consumer is unavailable",
            ));
        };
        let completed = in_tx_with_acquire_deadline(
            job.pool(),
            job.deadline(),
            TxOptions::default(),
            async |tx| -> Result<(), ProcessFailure> {
                consumer
                    .process(tx, job.payload())
                    .await
                    .map_err(ProcessFailure::Consumer)?;
                job.complete_in_tx(tx)
                    .await
                    .map_err(ProcessFailure::Completion)?;
                Ok(())
            },
        )
        .await;
        match completed {
            Ok(()) => Ok(()),
            Err(ProcessFailure::Consumer(error)) => {
                // Every endpoint shares one job kind, so the attempt's own
                // event that follows cannot say whose consumer this was.
                tracing::info!(
                    webhook.endpoint = endpoint_id,
                    permanent = error.is_permanent(),
                    "webhook_consumer_incomplete"
                );
                Err(error)
            }
            // `From` keeps the cause chain in the job's error summary and
            // turns a pool acquire timeout into a refunded snooze.
            Err(other) => Err(JobError::from(other)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ProcessFailure {
    #[error("inbound webhook consumer failed")]
    Consumer(JobError),
    #[error("inbound webhook completion failed")]
    Completion(#[source] CompleteError),
    #[error("inbound webhook processing is unavailable")]
    Transaction(#[from] TxError),
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_processing_failure_summary_keeps_its_cause() {
        let summary = JobError::from(ProcessFailure::Transaction(TxError::CommitUnknown(
            sqlx::Error::PoolClosed,
        )))
        .to_string();
        assert!(
            summary.contains("commit outcome unknown"),
            "summary names the cause: {summary}"
        );
    }
}
