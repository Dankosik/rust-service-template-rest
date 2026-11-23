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
pub mod worker;

#[cfg(feature = "test-support")]
pub mod test_support;

#[cfg(test)]
mod tests;
