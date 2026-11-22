mod client;
mod connection;
mod credentials;
mod error;
mod ids;
mod metrics;
mod options;
mod payload;
mod probe;
mod trace;

pub use temporalio_client::{Memo, WorkflowExecutionDescription, WorkflowExecutionStatus};
pub use temporalio_common::RetryPolicy;
pub use temporalio_common::data_converters::PayloadCodec;
pub use temporalio_common::search_attributes::SearchAttributes;
pub use zeroize::Zeroizing;

pub use crate::client::{
    MutationDedupe, RunningConflictPolicy, StartIdPolicy, StartRequest, TemporalClient,
    UpdateWithStartRequest, WorkflowHandle,
};
pub use crate::connection::{CloseOutcome, ConnectError, TemporalConnection, TemporalStartup};
pub use crate::error::{
    MutationError, MutationFailure, ReadError, StartFailure, StartWorkflowError, UnknownOutcome,
    UpdateFailure, UpdateWorkflowError, WorkflowResultError,
};
pub use crate::ids::{IdentifierError, NonBlankName, RequestKey, RunId, UpdateId, WorkflowId};
pub use crate::metrics::{HISTOGRAMS, METRIC_PREFIX, WorkspaceMetricsMeter, describe_metrics};
pub use crate::options::{
    TemporalAuth, TemporalMtlsOptions, TemporalOptions, TemporalTlsOptions, TlsRotationMode,
};
pub use crate::payload::PayloadError;
pub use crate::probe::{PROBE_NAME, TemporalProbe};

#[cfg(feature = "worker")]
pub mod worker {
    use temporalio_sdk::runtime::RuntimeOptions;
    use temporalio_sdk::{Runtime, RuntimeError, Worker, WorkerRunError};
    use temporalio_sdk_core::CoreRuntime;
    use tokio::runtime::Handle;
    use tokio_util::sync::CancellationToken;

    use crate::WorkspaceMetricsMeter;

    pub fn build_runtime(meter: &WorkspaceMetricsMeter) -> Result<Runtime, RuntimeError> {
        let options = RuntimeOptions::builder()
            .telemetry_options(meter.telemetry_options())
            .heartbeat_interval(None)
            .build()
            .map_err(|err| RuntimeError::Initialization(err.into()))?;
        Runtime::from_current_tokio(options)
    }

    pub fn spawn_worker_thread(
        name: String,
        handle: Handle,
        worker: Worker,
        shutdown: CancellationToken,
    ) -> std::io::Result<std::thread::JoinHandle<Result<(), WorkerRunError>>> {
        const fn assert_send<T: Send>() {}
        assert_send::<Worker>();
        assert_send::<Runtime>();
        assert_send::<CoreRuntime>();

        std::thread::Builder::new()
            .name(name)
            .spawn(move || handle.block_on(worker.run_until(shutdown.cancelled())))
    }
}

#[cfg(test)]
mod tests;
