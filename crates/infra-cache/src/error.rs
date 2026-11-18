use operation_context::Stopped;

use crate::namespace::Operation;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OperationError {
    #[error("cache key exceeds the configured size limit")]
    KeyTooLarge,
    #[error("cache value exceeds the configured size limit")]
    ValueTooLarge,
    #[error("cache operation stopped by caller ({0:?})")]
    Stopped(Stopped),
    #[error("cache unavailable")]
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MutationError {
    #[error("cache mutation was not applied: {0}")]
    NotApplied(#[source] OperationError),
    #[error("cache mutation outcome is unknown: {0}")]
    OutcomeUnknown(#[source] OperationError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SetError {
    #[error("cache ttl is outside the supported expiration range")]
    InvalidTtl,
    #[error(transparent)]
    Mutation(#[from] MutationError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
pub(crate) enum ErrorType {
    Timeout,
    Io,
    Auth,
    ServerErrorReply,
    Parse,
    ValueTooLarge,
    Other,
}

#[derive(Clone, Copy)]
pub(crate) enum CommandFailure {
    Stopped(Stopped),
    Failed(ErrorType),
    PermissionDenied,
    InvalidTtl,
}

impl CommandFailure {
    fn operation_error(self) -> OperationError {
        match self {
            Self::Stopped(stopped) => OperationError::Stopped(stopped),
            Self::Failed(ErrorType::ValueTooLarge) => OperationError::ValueTooLarge,
            Self::Failed(_) | Self::PermissionDenied | Self::InvalidTtl => {
                OperationError::Unavailable
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum CommandError {
    NotApplied(CommandFailure),
    OutcomeUnknown(CommandFailure),
}

impl CommandError {
    pub(crate) fn failure(self) -> CommandFailure {
        match self {
            Self::NotApplied(failure) | Self::OutcomeUnknown(failure) => failure,
        }
    }

    pub(crate) fn from_reply(
        error: &redis::RedisError,
        operation: Operation,
        stopped: Option<CommandFailure>,
    ) -> Self {
        let code = error.code();
        if matches!(operation, Operation::Set)
            && code == Some("ERR")
            && error.detail().is_some_and(|detail| {
                detail.eq_ignore_ascii_case("invalid expire time in 'set' command")
            })
        {
            return Self::NotApplied(CommandFailure::InvalidTtl);
        }
        let failure = stopped.unwrap_or_else(|| match code {
            Some("NOPERM") => CommandFailure::PermissionDenied,
            _ => CommandFailure::Failed(error_type(error)),
        });
        match code {
            Some(
                "READONLY" | "NOAUTH" | "NOPERM" | "OOM" | "LOADING" | "MOVED" | "ASK"
                | "CLUSTERDOWN" | "MASTERDOWN" | "MISCONF" | "NOREPLICAS" | "BUSY" | "REDIRECT",
            ) => Self::NotApplied(failure),
            _ => Self::OutcomeUnknown(failure),
        }
    }
}

impl From<CommandError> for OperationError {
    fn from(error: CommandError) -> Self {
        error.failure().operation_error()
    }
}

impl From<CommandError> for MutationError {
    fn from(error: CommandError) -> Self {
        match error {
            CommandError::NotApplied(failure) => Self::NotApplied(failure.operation_error()),
            CommandError::OutcomeUnknown(failure) => {
                Self::OutcomeUnknown(failure.operation_error())
            }
        }
    }
}

impl From<CommandError> for SetError {
    fn from(error: CommandError) -> Self {
        match error {
            CommandError::NotApplied(CommandFailure::InvalidTtl) => Self::InvalidTtl,
            _ => Self::Mutation(error.into()),
        }
    }
}

impl From<ErrorType> for CommandError {
    fn from(error: ErrorType) -> Self {
        Self::NotApplied(CommandFailure::Failed(error))
    }
}

impl ErrorType {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Io => "io",
            Self::Auth => "auth",
            Self::ServerErrorReply => "response",
            Self::Parse => "parse",
            Self::ValueTooLarge => "value_too_large",
            Self::Other => "other",
        }
    }
}

pub(crate) fn error_type(error: &redis::RedisError) -> ErrorType {
    if error.is_timeout() {
        return ErrorType::Timeout;
    }
    if matches!(error.code(), Some("WRONGPASS" | "NOAUTH")) {
        return ErrorType::Auth;
    }
    match error.kind() {
        redis::ErrorKind::Io => ErrorType::Io,
        redis::ErrorKind::AuthenticationFailed => ErrorType::Auth,
        redis::ErrorKind::Server(_) | redis::ErrorKind::Extension => ErrorType::ServerErrorReply,
        redis::ErrorKind::Parse => ErrorType::Parse,
        _ => ErrorType::Other,
    }
}
