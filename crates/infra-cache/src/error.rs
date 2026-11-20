use operation_context::Stopped;

use crate::namespace::Operation;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StartError {
    #[error("cache.max_concurrency must allow application commands and leave one maintenance slot")]
    InvalidConcurrency,
    #[error("cache key or value size limit is outside the supported range")]
    InvalidSizeLimit,
    #[error("cache DSN is invalid")]
    InvalidDsn,
    #[error("cache address is unsupported")]
    UnsupportedAddress,
    #[error("cache plaintext is refused")]
    PlaintextRefused,
    #[error("cache unauthenticated connection is refused")]
    UnauthenticatedRefused,
    #[error("cache DSN must not carry a password when cache.password_file is set")]
    PasswordInDsn,
    #[error("cache password file could not be read ({kind})")]
    PasswordFile { kind: std::io::ErrorKind },
    #[error("cache password file is empty")]
    PasswordFileEmpty,
    #[error("cache insecure TLS is refused")]
    InsecureTlsRefused,
    #[error("cache root CA requires TLS")]
    CaRequiresTls,
    #[error("cache root CA file could not be read ({kind})")]
    CaFile { kind: std::io::ErrorKind },
    #[error("cache root CA is invalid")]
    InvalidCa,
    #[error("cache client certificate requires TLS")]
    ClientCertificateRequiresTls,
    #[error("cache client certificate file could not be read ({kind})")]
    ClientCertificateFile { kind: std::io::ErrorKind },
    #[error("cache client certificate is invalid")]
    InvalidClientCertificate,
    #[error("cache client key file could not be read ({kind})")]
    ClientKeyFile { kind: std::io::ErrorKind },
    #[error("cache client key is invalid or does not match the client certificate")]
    InvalidClientKey,
    #[error("cache client could not be built")]
    ClientBuild,
    #[error("cache startup admission timed out")]
    AdmissionTimedOut,
}

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
pub enum UnknownOutcome {
    #[error("cache unavailable")]
    Unavailable,
    #[error("cache operation stopped by caller ({0:?})")]
    Stopped(Stopped),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MutationError {
    #[error("cache mutation was not applied: {0}")]
    NotApplied(#[source] OperationError),
    #[error("cache mutation outcome is unknown: {0}")]
    OutcomeUnknown(#[source] UnknownOutcome),
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
    Other,
}

#[derive(Clone, Copy)]
pub(crate) enum UnknownCause {
    Stopped(Stopped),
    Failed(ErrorType),
}

impl UnknownCause {
    fn operation_error(self) -> OperationError {
        match self {
            Self::Stopped(stopped) => OperationError::Stopped(stopped),
            Self::Failed(_) => OperationError::Unavailable,
        }
    }

    fn unknown_outcome(self) -> UnknownOutcome {
        match self {
            Self::Stopped(stopped) => UnknownOutcome::Stopped(stopped),
            Self::Failed(_) => UnknownOutcome::Unavailable,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Refusal {
    Stopped(Stopped),
    Failed(ErrorType),
    PermissionDenied,
    InvalidTtl,
    ValueTooLarge,
}

impl Refusal {
    fn operation_error(self) -> OperationError {
        match self {
            Self::Stopped(stopped) => OperationError::Stopped(stopped),
            Self::ValueTooLarge => OperationError::ValueTooLarge,
            Self::Failed(_) | Self::PermissionDenied | Self::InvalidTtl => {
                OperationError::Unavailable
            }
        }
    }
}

impl From<UnknownCause> for Refusal {
    fn from(cause: UnknownCause) -> Self {
        match cause {
            UnknownCause::Stopped(stopped) => Self::Stopped(stopped),
            UnknownCause::Failed(error) => Self::Failed(error),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum CommandError {
    NotApplied(Refusal),
    OutcomeUnknown(UnknownCause),
}

impl CommandError {
    pub(crate) fn from_reply(
        error: &redis::RedisError,
        operation: Operation,
        stopped: Option<UnknownCause>,
    ) -> Self {
        let code = error.code();
        if matches!(operation, Operation::Set)
            && code == Some("ERR")
            && error.detail().is_some_and(|detail| {
                detail.eq_ignore_ascii_case("invalid expire time in 'set' command")
            })
        {
            return Self::NotApplied(Refusal::InvalidTtl);
        }
        if code == Some("NOPERM") {
            return Self::NotApplied(stopped.map_or(Refusal::PermissionDenied, Refusal::from));
        }
        let cause = stopped.unwrap_or_else(|| UnknownCause::Failed(error_type(error)));
        match code {
            Some(
                "READONLY" | "NOAUTH" | "OOM" | "LOADING" | "MOVED" | "ASK" | "CLUSTERDOWN"
                | "MASTERDOWN" | "MISCONF" | "NOREPLICAS" | "BUSY" | "REDIRECT",
            ) => Self::NotApplied(cause.into()),
            _ => Self::OutcomeUnknown(cause),
        }
    }
}

impl From<CommandError> for OperationError {
    fn from(error: CommandError) -> Self {
        match error {
            CommandError::NotApplied(refusal) => refusal.operation_error(),
            CommandError::OutcomeUnknown(cause) => cause.operation_error(),
        }
    }
}

impl From<CommandError> for MutationError {
    fn from(error: CommandError) -> Self {
        match error {
            CommandError::NotApplied(refusal) => Self::NotApplied(refusal.operation_error()),
            CommandError::OutcomeUnknown(cause) => Self::OutcomeUnknown(cause.unknown_outcome()),
        }
    }
}

impl From<CommandError> for SetError {
    fn from(error: CommandError) -> Self {
        match error {
            CommandError::NotApplied(Refusal::InvalidTtl) => Self::InvalidTtl,
            _ => Self::Mutation(error.into()),
        }
    }
}

impl From<ErrorType> for CommandError {
    fn from(error: ErrorType) -> Self {
        Self::NotApplied(Refusal::Failed(error))
    }
}

impl From<Refusal> for CommandError {
    fn from(refusal: Refusal) -> Self {
        Self::NotApplied(refusal)
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
