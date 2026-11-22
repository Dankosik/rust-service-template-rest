use operation_context::{BoundedStop, Stopped};
use temporalio_client::MESSAGE_TOO_LARGE_KEY;
use temporalio_client::errors::{
    WorkflowGetResultError, WorkflowInteractionError, WorkflowQueryError, WorkflowStartError,
    WorkflowUpdateError, WorkflowUpdateWithStartError,
};
use temporalio_common::data_converters::PayloadConversionError;
use tonic::{Code, Status};

use crate::payload::{PayloadError, classify_payload_conversion};

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    #[error("temporal workflow not found")]
    NotFound,
    #[error("temporal workflow state conflict")]
    Conflict,
    #[error("temporal workflow query rejected")]
    QueryRejected,
    #[error("invalid temporal request input")]
    InvalidInput,
    #[error("temporal payload decode failed")]
    PayloadDecode,
    #[error("temporal authentication failed")]
    Unauthenticated,
    #[error("temporal permission denied")]
    PermissionDenied,
    #[error("temporal service is rate limited")]
    Busy,
    #[error("temporal service unavailable")]
    Unavailable,
    #[error("temporal operation stopped")]
    Stopped(Stopped),
    #[error("temporal internal failure")]
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MutationFailure {
    #[error("temporal workflow not found")]
    NotFound,
    #[error("invalid temporal request input")]
    InvalidInput,
    #[error("temporal payload exceeds maximum size")]
    TooLarge,
    #[error("temporal authentication failed")]
    Unauthenticated,
    #[error("temporal permission denied")]
    PermissionDenied,
    #[error("temporal service is rate limited")]
    Busy,
    #[error("temporal service unavailable")]
    Unavailable,
    #[error("temporal operation stopped before dispatch")]
    Stopped(Stopped),
    #[error("temporal internal failure")]
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartFailure {
    #[error("temporal workflow already started")]
    AlreadyStarted,
    #[error("temporal workflow start conflict")]
    Conflict,
    #[error("invalid temporal request input")]
    InvalidInput,
    #[error("temporal payload exceeds maximum size")]
    TooLarge,
    #[error("temporal authentication failed")]
    Unauthenticated,
    #[error("temporal permission denied")]
    PermissionDenied,
    #[error("temporal service is rate limited")]
    Busy,
    #[error("temporal service unavailable")]
    Unavailable,
    #[error("temporal operation stopped before dispatch")]
    Stopped(Stopped),
    #[error("temporal internal failure")]
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UpdateFailure {
    #[error("temporal workflow already started")]
    AlreadyStarted,
    #[error("temporal workflow update conflict")]
    Conflict,
    #[error("temporal workflow not found")]
    NotFound,
    #[error("temporal workflow update rejected")]
    UpdateRejected,
    #[error("invalid temporal request input")]
    InvalidInput,
    #[error("temporal payload exceeds maximum size")]
    TooLarge,
    #[error("temporal authentication failed")]
    Unauthenticated,
    #[error("temporal permission denied")]
    PermissionDenied,
    #[error("temporal service is rate limited")]
    Busy,
    #[error("temporal service unavailable")]
    Unavailable,
    #[error("temporal operation stopped before dispatch")]
    Stopped(Stopped),
    #[error("temporal internal failure")]
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UnknownOutcome {
    #[error("temporal operation stopped with unconfirmed outcome")]
    Stopped(Stopped),
    #[error("temporal transport failed with unconfirmed outcome")]
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MutationError {
    #[error("temporal mutation not applied: {0}")]
    NotApplied(MutationFailure),
    #[error("temporal mutation outcome unknown: {0}")]
    OutcomeUnknown(UnknownOutcome),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartWorkflowError {
    #[error("temporal workflow start not applied: {0}")]
    NotApplied(StartFailure),
    #[error("temporal workflow start outcome unknown: {0}")]
    OutcomeUnknown(UnknownOutcome),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UpdateWorkflowError {
    #[error("temporal workflow update not applied: {0}")]
    NotApplied(UpdateFailure),
    #[error("temporal workflow update outcome unknown: {0}")]
    OutcomeUnknown(UnknownOutcome),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorkflowResultError {
    #[error("temporal workflow not found")]
    NotFound,
    #[error("temporal workflow failed")]
    WorkflowFailed,
    #[error("temporal workflow cancelled")]
    Cancelled,
    #[error("temporal workflow terminated")]
    Terminated,
    #[error("temporal workflow timed out")]
    TimedOut,
    #[error("temporal workflow continued as new")]
    ContinuedAsNew,
    #[error("invalid temporal request input")]
    InvalidInput,
    #[error("temporal payload decode failed")]
    PayloadDecode,
    #[error("temporal authentication failed")]
    Unauthenticated,
    #[error("temporal permission denied")]
    PermissionDenied,
    #[error("temporal service is rate limited")]
    Busy,
    #[error("temporal service unavailable")]
    Unavailable,
    #[error("temporal operation stopped")]
    Stopped(Stopped),
    #[error("temporal internal failure")]
    Internal,
}

pub(crate) const fn bounded_stop_to_stopped(stop: BoundedStop) -> Stopped {
    match stop {
        BoundedStop::Parent(stopped) => stopped,
        BoundedStop::Timeout => Stopped::Deadline,
    }
}

fn is_message_too_large(status: &Status) -> bool {
    status.code() == Code::ResourceExhausted
        && (status.metadata().contains_key(MESSAGE_TOO_LARGE_KEY)
            || status
                .message()
                .starts_with("grpc: received message larger than max")
            || status
                .message()
                .starts_with("grpc: message after decompression larger than max")
            || status
                .message()
                .starts_with("grpc: received message after decompression larger than max"))
}

pub(crate) fn map_start_status(status: &Status) -> StartWorkflowError {
    match status.code() {
        Code::AlreadyExists => StartWorkflowError::NotApplied(StartFailure::AlreadyStarted),
        Code::FailedPrecondition | Code::Aborted => {
            StartWorkflowError::NotApplied(StartFailure::Conflict)
        }
        Code::InvalidArgument | Code::OutOfRange => {
            StartWorkflowError::NotApplied(StartFailure::InvalidInput)
        }
        Code::Unauthenticated => StartWorkflowError::NotApplied(StartFailure::Unauthenticated),
        Code::PermissionDenied => StartWorkflowError::NotApplied(StartFailure::PermissionDenied),
        Code::ResourceExhausted if is_message_too_large(status) => {
            StartWorkflowError::NotApplied(StartFailure::TooLarge)
        }
        Code::ResourceExhausted => StartWorkflowError::NotApplied(StartFailure::Busy),
        Code::DeadlineExceeded => {
            StartWorkflowError::OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Deadline))
        }
        Code::Cancelled => {
            StartWorkflowError::OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Cancelled))
        }
        Code::Unavailable | Code::Unknown | Code::Internal | Code::DataLoss => {
            StartWorkflowError::OutcomeUnknown(UnknownOutcome::Unavailable)
        }
        _ => StartWorkflowError::NotApplied(StartFailure::Internal),
    }
}

pub(crate) fn map_start_payload_error(error: &PayloadConversionError) -> StartWorkflowError {
    match classify_payload_conversion(error, PayloadError::Serialize).0 {
        PayloadError::TooLarge { .. } => StartWorkflowError::NotApplied(StartFailure::TooLarge),
        PayloadError::Serialize | PayloadError::InvalidEncoding => {
            StartWorkflowError::NotApplied(StartFailure::InvalidInput)
        }
        PayloadError::Decode => StartWorkflowError::NotApplied(StartFailure::Internal),
    }
}

pub(crate) fn map_start_error(error: &WorkflowStartError) -> StartWorkflowError {
    match error {
        WorkflowStartError::AlreadyStarted { .. } => {
            StartWorkflowError::NotApplied(StartFailure::AlreadyStarted)
        }
        WorkflowStartError::PayloadConversion(err) => map_start_payload_error(err),
        WorkflowStartError::Rpc(status) => map_start_status(status),
        _ => StartWorkflowError::NotApplied(StartFailure::Internal),
    }
}

pub(crate) fn map_mutation_status(status: &Status) -> MutationError {
    match status.code() {
        Code::NotFound => MutationError::NotApplied(MutationFailure::NotFound),
        Code::InvalidArgument | Code::OutOfRange => {
            MutationError::NotApplied(MutationFailure::InvalidInput)
        }
        Code::Unauthenticated => MutationError::NotApplied(MutationFailure::Unauthenticated),
        Code::PermissionDenied => MutationError::NotApplied(MutationFailure::PermissionDenied),
        Code::ResourceExhausted if is_message_too_large(status) => {
            MutationError::NotApplied(MutationFailure::TooLarge)
        }
        Code::ResourceExhausted => MutationError::NotApplied(MutationFailure::Busy),
        Code::DeadlineExceeded => {
            MutationError::OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Deadline))
        }
        Code::Cancelled => {
            MutationError::OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Cancelled))
        }
        Code::Unavailable | Code::Unknown | Code::Internal | Code::DataLoss => {
            MutationError::OutcomeUnknown(UnknownOutcome::Unavailable)
        }
        _ => MutationError::NotApplied(MutationFailure::Internal),
    }
}

pub(crate) fn map_mutation_error(error: &WorkflowInteractionError) -> MutationError {
    match error {
        WorkflowInteractionError::NotFound(_) => {
            MutationError::NotApplied(MutationFailure::NotFound)
        }
        WorkflowInteractionError::PayloadConversion(err) => {
            match classify_payload_conversion(err, PayloadError::Serialize).0 {
                PayloadError::TooLarge { .. } => {
                    MutationError::NotApplied(MutationFailure::TooLarge)
                }
                PayloadError::Serialize | PayloadError::InvalidEncoding => {
                    MutationError::NotApplied(MutationFailure::InvalidInput)
                }
                PayloadError::Decode => MutationError::NotApplied(MutationFailure::Internal),
            }
        }
        WorkflowInteractionError::Rpc(status) => map_mutation_status(status),
        _ => MutationError::NotApplied(MutationFailure::Internal),
    }
}

pub(crate) fn map_update_status(status: &Status) -> UpdateWorkflowError {
    match status.code() {
        Code::AlreadyExists => UpdateWorkflowError::NotApplied(UpdateFailure::AlreadyStarted),
        Code::FailedPrecondition | Code::Aborted => {
            UpdateWorkflowError::NotApplied(UpdateFailure::Conflict)
        }
        Code::NotFound => UpdateWorkflowError::NotApplied(UpdateFailure::NotFound),
        Code::InvalidArgument | Code::OutOfRange => {
            UpdateWorkflowError::NotApplied(UpdateFailure::InvalidInput)
        }
        Code::Unauthenticated => UpdateWorkflowError::NotApplied(UpdateFailure::Unauthenticated),
        Code::PermissionDenied => UpdateWorkflowError::NotApplied(UpdateFailure::PermissionDenied),
        Code::ResourceExhausted if is_message_too_large(status) => {
            UpdateWorkflowError::NotApplied(UpdateFailure::TooLarge)
        }
        Code::ResourceExhausted => UpdateWorkflowError::NotApplied(UpdateFailure::Busy),
        Code::DeadlineExceeded => {
            UpdateWorkflowError::OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Deadline))
        }
        Code::Cancelled => {
            UpdateWorkflowError::OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Cancelled))
        }
        Code::Unavailable | Code::Unknown | Code::Internal | Code::DataLoss => {
            UpdateWorkflowError::OutcomeUnknown(UnknownOutcome::Unavailable)
        }
        _ => UpdateWorkflowError::NotApplied(UpdateFailure::Internal),
    }
}

fn map_update_payload_error(error: &PayloadConversionError) -> UpdateWorkflowError {
    match classify_payload_conversion(error, PayloadError::Serialize) {
        (PayloadError::TooLarge { .. }, false) => {
            UpdateWorkflowError::NotApplied(UpdateFailure::TooLarge)
        }
        (PayloadError::Serialize | PayloadError::InvalidEncoding, false) => {
            UpdateWorkflowError::NotApplied(UpdateFailure::InvalidInput)
        }
        (_, true) | (PayloadError::Decode, false) => {
            UpdateWorkflowError::NotApplied(UpdateFailure::Internal)
        }
    }
}

pub(crate) fn map_update_error(error: &WorkflowUpdateError) -> UpdateWorkflowError {
    match error {
        WorkflowUpdateError::NotFound(_) => {
            UpdateWorkflowError::NotApplied(UpdateFailure::NotFound)
        }
        WorkflowUpdateError::Failed(_) => {
            UpdateWorkflowError::NotApplied(UpdateFailure::UpdateRejected)
        }
        WorkflowUpdateError::PayloadConversion(err) => map_update_payload_error(err),
        WorkflowUpdateError::Rpc(status) => map_update_status(status),
        _ => UpdateWorkflowError::NotApplied(UpdateFailure::Internal),
    }
}

pub(crate) fn map_update_with_start_error(
    error: &WorkflowUpdateWithStartError,
) -> UpdateWorkflowError {
    match error {
        WorkflowUpdateWithStartError::Start(start) => match map_start_error(start) {
            StartWorkflowError::NotApplied(reason) => {
                UpdateWorkflowError::NotApplied(match reason {
                    StartFailure::AlreadyStarted => UpdateFailure::AlreadyStarted,
                    StartFailure::Conflict => UpdateFailure::Conflict,
                    StartFailure::InvalidInput => UpdateFailure::InvalidInput,
                    StartFailure::TooLarge => UpdateFailure::TooLarge,
                    StartFailure::Unauthenticated => UpdateFailure::Unauthenticated,
                    StartFailure::PermissionDenied => UpdateFailure::PermissionDenied,
                    StartFailure::Busy => UpdateFailure::Busy,
                    StartFailure::Unavailable => UpdateFailure::Unavailable,
                    StartFailure::Stopped(stopped) => UpdateFailure::Stopped(stopped),
                    StartFailure::Internal => UpdateFailure::Internal,
                })
            }
            StartWorkflowError::OutcomeUnknown(unknown) => {
                UpdateWorkflowError::OutcomeUnknown(unknown)
            }
        },
        WorkflowUpdateWithStartError::Update(update) => map_update_error(update),
        WorkflowUpdateWithStartError::PayloadConversion(err) => map_update_payload_error(err),
        WorkflowUpdateWithStartError::Rpc(status) => map_update_status(status),
        _ => UpdateWorkflowError::NotApplied(UpdateFailure::Internal),
    }
}

pub(crate) fn map_read_status(status: &Status) -> ReadError {
    match status.code() {
        Code::NotFound => ReadError::NotFound,
        Code::FailedPrecondition | Code::Aborted => ReadError::Conflict,
        Code::InvalidArgument | Code::OutOfRange => ReadError::InvalidInput,
        Code::Unauthenticated => ReadError::Unauthenticated,
        Code::PermissionDenied => ReadError::PermissionDenied,
        Code::ResourceExhausted if is_message_too_large(status) => ReadError::PayloadDecode,
        Code::ResourceExhausted => ReadError::Busy,
        Code::Unavailable => ReadError::Unavailable,
        Code::DeadlineExceeded => ReadError::Stopped(Stopped::Deadline),
        Code::Cancelled => ReadError::Stopped(Stopped::Cancelled),
        _ => ReadError::Internal,
    }
}

pub(crate) fn map_query_error(error: &WorkflowQueryError) -> ReadError {
    match error {
        WorkflowQueryError::NotFound(_) => ReadError::NotFound,
        WorkflowQueryError::Rejected { .. } => ReadError::QueryRejected,
        WorkflowQueryError::PayloadConversion(err) => {
            match classify_payload_conversion(err, PayloadError::Decode) {
                (PayloadError::Serialize | PayloadError::TooLarge { .. }, false) => {
                    ReadError::InvalidInput
                }
                (_, true) | (PayloadError::Decode | PayloadError::InvalidEncoding, false) => {
                    ReadError::PayloadDecode
                }
            }
        }
        WorkflowQueryError::Rpc(status) => map_read_status(status),
        _ => ReadError::Internal,
    }
}

pub(crate) fn map_describe_error(error: &WorkflowInteractionError) -> ReadError {
    match error {
        WorkflowInteractionError::NotFound(_) => ReadError::NotFound,
        WorkflowInteractionError::PayloadConversion(_) => ReadError::PayloadDecode,
        WorkflowInteractionError::Rpc(status) => map_read_status(status),
        _ => ReadError::Internal,
    }
}

pub(crate) fn map_result_status(status: &Status) -> WorkflowResultError {
    match status.code() {
        Code::NotFound => WorkflowResultError::NotFound,
        Code::InvalidArgument | Code::OutOfRange => WorkflowResultError::InvalidInput,
        Code::Unauthenticated => WorkflowResultError::Unauthenticated,
        Code::PermissionDenied => WorkflowResultError::PermissionDenied,
        Code::ResourceExhausted if is_message_too_large(status) => {
            WorkflowResultError::PayloadDecode
        }
        Code::ResourceExhausted => WorkflowResultError::Busy,
        Code::Unavailable => WorkflowResultError::Unavailable,
        Code::DeadlineExceeded => WorkflowResultError::Stopped(Stopped::Deadline),
        Code::Cancelled => WorkflowResultError::Stopped(Stopped::Cancelled),
        _ => WorkflowResultError::Internal,
    }
}

pub(crate) fn map_result_error(error: &WorkflowGetResultError) -> WorkflowResultError {
    match error {
        WorkflowGetResultError::Failed(_) => WorkflowResultError::WorkflowFailed,
        WorkflowGetResultError::Cancelled { .. } => WorkflowResultError::Cancelled,
        WorkflowGetResultError::Terminated { .. } => WorkflowResultError::Terminated,
        WorkflowGetResultError::TimedOut => WorkflowResultError::TimedOut,
        WorkflowGetResultError::ContinuedAsNew => WorkflowResultError::ContinuedAsNew,
        WorkflowGetResultError::NotFound(_) => WorkflowResultError::NotFound,
        WorkflowGetResultError::PayloadConversion(_) => WorkflowResultError::PayloadDecode,
        WorkflowGetResultError::Rpc(status) => map_result_status(status),
        _ => WorkflowResultError::Internal,
    }
}
