use std::collections::hash_map::Entry;
use std::fmt;
use std::sync::Arc;

use foldhash::HashMap;
use infra_jobs::{CompleteError, Handler, Job, JobError, KindsBuilder, Policy};
use infra_postgres::{Tx, TxError, TxOptions, in_tx_with_acquire_deadline};

use super::{Incoming, async_trait};

/// `tx` stays open, and holds a pooled connection, until `process` returns
/// and the job completes in it. Keep `process` to database effects. For an
/// effect outside PostgreSQL, enqueue a job on `tx` and let that job make the
/// call, so a slow recipient holds neither a transaction nor a connection.
#[async_trait]
pub trait Consumer: Send + Sync + 'static {
    /// An `Err` rolls back every effect made on `tx`, and its [`JobError`] class
    /// decides whether the job is retried. Do not complete the job: [`Processor`]
    /// completes it in the same transaction after `Ok`.
    async fn process(&self, tx: &mut Tx<'_>, incoming: &Incoming) -> Result<(), JobError>;
}

#[derive(Default)]
pub struct Consumers {
    bindings: HashMap<String, Arc<dyn Consumer>>,
}

impl Consumers {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind(
        &mut self,
        endpoint_id: impl Into<String>,
        consumer: Arc<dyn Consumer>,
    ) -> Result<(), EndpointAlreadyBound> {
        match self.bindings.entry(endpoint_id.into()) {
            Entry::Occupied(bound) => Err(EndpointAlreadyBound {
                endpoint_id: bound.key().clone(),
            }),
            Entry::Vacant(unbound) => {
                unbound.insert(consumer);
                Ok(())
            }
        }
    }

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
pub struct EndpointAlreadyBound {
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

#[derive(Debug)]
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
    fn processor_debug_shows_binding_count_only() {
        let processor = Processor::new(Consumers::new());
        assert_eq!(
            format!("{processor:?}"),
            "Processor { consumers: Consumers { binding_count: 0 } }"
        );
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
