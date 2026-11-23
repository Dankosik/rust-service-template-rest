#[cfg(test)]
mod tests;

use std::fmt;
use std::sync::Arc;

use temporalio_client::WorkflowHistory;
use temporalio_common::WorkflowDefinition;
use temporalio_common::data_converters::PayloadCodec;
use temporalio_common::protos::temporal::api::history::v1::{History, history_event};
use temporalio_sdk::workflow_replayer::{
    WorkflowReplayError, WorkflowReplayFailure, WorkflowReplayWorkerError, WorkflowReplayer,
    WorkflowReplayerOptions,
};
use temporalio_sdk::workflows::WorkflowImplementation;
use temporalio_sdk_core::replay::HistoryInfo;

pub use crate::worker::{Preempted, WorkflowRegistrationError, run_with_preemption};

use crate::payload::build_data_converter;
use crate::worker::trace_workflow_interceptor_constructor;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReplayError {
    #[error("workflow replay registration failed")]
    Registration,
    #[error("invalid workflow history JSON")]
    InvalidHistoryJson,
    #[error("workflow replay nondeterminism: {0}")]
    Nondeterminism(String),
    #[error("workflow replay execution failed: {0}")]
    ExecutionFailed(String),
}

pub struct ReplaySuite {
    options: WorkflowReplayerOptions,
}

impl fmt::Debug for ReplaySuite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReplaySuite")
            .field("workflows", &self.options.workflows())
            .finish_non_exhaustive()
    }
}

impl ReplaySuite {
    #[must_use]
    pub fn new(
        max_payload_bytes: usize,
        codec: Option<Arc<dyn PayloadCodec + Send + Sync>>,
    ) -> Self {
        let data_converter = build_data_converter(max_payload_bytes, codec);
        let payload_converter = data_converter.payload_converter().clone();
        let options = WorkflowReplayerOptions::new()
            .data_converter(data_converter)
            .detect_nondeterministic_futures(true)
            .workflow_interceptor(trace_workflow_interceptor_constructor(payload_converter))
            .build();
        Self { options }
    }

    pub fn register_workflow<W>(&mut self) -> Result<&mut Self, WorkflowRegistrationError>
    where
        W: WorkflowImplementation,
        <W::Run as WorkflowDefinition>::Input: Send,
    {
        self.options.register_workflow::<W>()?;
        Ok(self)
    }

    pub async fn replay_json(&self, history_json: &[u8]) -> Result<(), ReplayError> {
        let replayer = WorkflowReplayer::new(self.options.clone()).map_err(map_replay_error)?;
        let history: History =
            serde_json::from_slice(history_json).map_err(|_| ReplayError::InvalidHistoryJson)?;
        validate_history_for_replay(&history)?;
        replayer
            .replay_workflow(WorkflowHistory::from(history))
            .await
            .map_err(map_replay_error)
    }
}

fn validate_history_for_replay(history: &History) -> Result<(), ReplayError> {
    let _ = HistoryInfo::new_from_history(history, None)
        .map_err(|_| ReplayError::InvalidHistoryJson)?;
    let _ = history
        .extract_run_id_from_start()
        .map_err(|_| ReplayError::InvalidHistoryJson)?;
    let Some(history_event::Attributes::WorkflowExecutionStartedEventAttributes(started)) =
        history.events.first().and_then(|ev| ev.attributes.as_ref())
    else {
        return Err(ReplayError::InvalidHistoryJson);
    };
    if started.task_queue.is_none() {
        return Err(ReplayError::InvalidHistoryJson);
    }
    Ok(())
}

fn map_replay_error(error: WorkflowReplayError) -> ReplayError {
    match error {
        WorkflowReplayError::Worker(WorkflowReplayWorkerError::NoWorkflowsRegistered) => {
            ReplayError::Registration
        }
        WorkflowReplayError::Replay(WorkflowReplayFailure::InvalidHistory { .. }) => {
            ReplayError::InvalidHistoryJson
        }
        WorkflowReplayError::Replay(WorkflowReplayFailure::Nondeterminism { message }) => {
            ReplayError::Nondeterminism(message)
        }
        WorkflowReplayError::Replay(WorkflowReplayFailure::WorkflowTaskFailure { message }) => {
            if message.contains("TMPRL1100") || message.contains("Nondeterministic") {
                ReplayError::Nondeterminism(message)
            } else {
                ReplayError::ExecutionFailed(message)
            }
        }
        WorkflowReplayError::Replay(WorkflowReplayFailure::Internal { reason, message }) => {
            ReplayError::ExecutionFailed(format!("{reason}: {message}"))
        }
        WorkflowReplayError::Worker(other) => ReplayError::ExecutionFailed(other.to_string()),
        WorkflowReplayError::History(other) => ReplayError::ExecutionFailed(other.to_string()),
        other => ReplayError::ExecutionFailed(other.to_string()),
    }
}
