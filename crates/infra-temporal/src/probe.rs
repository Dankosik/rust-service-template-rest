use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use health::{Probe, ProbeError};
use operation_context::OperationContext;
use temporalio_client::grpc::WorkflowService as _;
use temporalio_client::request_extensions::RetryConfigForCall;
use temporalio_client::{Client, RetryOptions};
use temporalio_common::protos::temporal::api::workflowservice::v1::DescribeNamespaceRequest;
use tonic::IntoRequest as _;

pub const PROBE_NAME: &str = "temporal";

#[derive(Clone)]
pub struct TemporalProbe {
    client: Client,
    namespace: String,
    probe_budget: Duration,
    draining: Arc<AtomicBool>,
    worker_failed: Option<Arc<AtomicBool>>,
}

impl fmt::Debug for TemporalProbe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TemporalProbe")
            .field("namespace", &self.namespace)
            .field("probe_budget", &self.probe_budget)
            .field("draining", &self.draining.load(Ordering::Acquire))
            .field(
                "worker_failed",
                &self
                    .worker_failed
                    .as_ref()
                    .map(|flag| flag.load(Ordering::Acquire)),
            )
            .finish_non_exhaustive()
    }
}

impl TemporalProbe {
    pub(crate) fn new(
        client: Client,
        namespace: String,
        probe_budget: Duration,
        draining: Arc<AtomicBool>,
        worker_failed: Option<Arc<AtomicBool>>,
    ) -> Self {
        Self {
            client,
            namespace,
            probe_budget,
            draining,
            worker_failed,
        }
    }

    fn check_local_state(&self) -> Result<(), ProbeError> {
        if self.draining.load(Ordering::Acquire) {
            return Err(ProbeError::from_static("temporal adapter is draining"));
        }
        if self
            .worker_failed
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            return Err(ProbeError::from_static("temporal worker failed"));
        }
        Ok(())
    }
}

impl Probe for TemporalProbe {
    fn name(&self) -> &'static str {
        PROBE_NAME
    }

    async fn check(&self) -> Result<(), ProbeError> {
        self.check_local_state()?;

        let mut request = DescribeNamespaceRequest {
            namespace: self.namespace.clone(),
            ..Default::default()
        }
        .into_request();
        request.set_timeout(self.probe_budget);
        request
            .extensions_mut()
            .insert(RetryConfigForCall(RetryOptions::no_retries()));

        let mut client = self.client.clone();
        let operation = OperationContext::with_timeout(self.probe_budget);
        match operation.run(client.describe_namespace(request)).await {
            Ok(Ok(_)) => self.check_local_state(),
            Ok(Err(status)) => {
                tracing::warn!(
                    grpc_code = ?status.code(),
                    "temporal_readiness_probe_failed"
                );
                Err(ProbeError::from_static("temporal namespace probe failed"))
            }
            Err(_) => {
                tracing::warn!(reason = "timeout", "temporal_readiness_probe_failed");
                Err(ProbeError::from_static(
                    "temporal readiness probe timed out",
                ))
            }
        }
    }
}
