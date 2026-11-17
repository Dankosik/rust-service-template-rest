use std::collections::hash_map::Entry;
use std::fmt;
use std::sync::Arc;

use foldhash::HashMap;
use infra_jobs::{JobError, KindsBuilder, Policy, TransactionalHandler, TransactionalJob};
use infra_postgres::Tx;

use super::{Incoming, async_trait};

#[async_trait]
pub trait Consumer: Send + Sync + 'static {
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
        kinds.register_transactional(Policy::default(), self)
    }
}

impl TransactionalHandler<Incoming> for Processor {
    async fn run(&self, job: TransactionalJob<Incoming>, tx: &mut Tx<'_>) -> Result<(), JobError> {
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
        consumer
            .process(tx, job.payload())
            .await
            .inspect_err(|error| {
                tracing::info!(
                    webhook.endpoint = endpoint_id,
                    permanent = error.is_permanent(),
                    "webhook_consumer_incomplete"
                );
            })
    }
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
}
