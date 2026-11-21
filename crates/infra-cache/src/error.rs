use operation_context::{BoundedStop, Stopped};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error(transparent)]
pub struct StartError(pub(crate) StartErrorKind);

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum StartErrorKind {
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
pub enum MutationFailure {
    #[error("cache key exceeds the configured size limit")]
    KeyTooLarge,
    #[error("cache value exceeds the configured size limit")]
    ValueTooLarge,
    #[error("cache ttl is outside the supported expiration range")]
    InvalidTtl,
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
    NotApplied(#[source] MutationFailure),
    #[error("cache mutation outcome is unknown: {0}")]
    OutcomeUnknown(#[source] UnknownOutcome),
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

    fn mutation_failure(self) -> MutationFailure {
        match self {
            Self::Stopped(stopped) => MutationFailure::Stopped(stopped),
            Self::Failed(_) => MutationFailure::Unavailable,
        }
    }

    fn unknown_outcome(self) -> UnknownOutcome {
        match self {
            Self::Stopped(stopped) => UnknownOutcome::Stopped(stopped),
            Self::Failed(_) => UnknownOutcome::Unavailable,
        }
    }
}

impl From<BoundedStop> for UnknownCause {
    fn from(stop: BoundedStop) -> Self {
        match stop {
            BoundedStop::Parent(stopped) => Self::Stopped(stopped),
            BoundedStop::Timeout => Self::Failed(ErrorType::Timeout),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ReadFailure {
    Cause(UnknownCause),
    PermissionDenied,
    ValueTooLarge,
}

impl ReadFailure {
    pub(crate) fn from_reply(error: &redis::RedisError, stopped: Option<UnknownCause>) -> Self {
        if error.code() == Some("NOPERM") {
            return stopped.map_or(Self::PermissionDenied, Self::Cause);
        }
        Self::Cause(stopped.unwrap_or_else(|| UnknownCause::Failed(error_type(error))))
    }
}

impl From<ReadFailure> for OperationError {
    fn from(failure: ReadFailure) -> Self {
        match failure {
            ReadFailure::Cause(cause) => cause.operation_error(),
            ReadFailure::ValueTooLarge => Self::ValueTooLarge,
            ReadFailure::PermissionDenied => Self::Unavailable,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum MutationKind {
    Set,
    Delete,
}

#[derive(Clone, Copy)]
pub(crate) enum MutationRefusal {
    Cause(UnknownCause),
    PermissionDenied,
    InvalidTtl,
}

impl MutationRefusal {
    fn mutation_failure(self) -> MutationFailure {
        match self {
            Self::Cause(cause) => cause.mutation_failure(),
            Self::InvalidTtl => MutationFailure::InvalidTtl,
            Self::PermissionDenied => MutationFailure::Unavailable,
        }
    }
}

impl From<UnknownCause> for MutationRefusal {
    fn from(cause: UnknownCause) -> Self {
        Self::Cause(cause)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum MutationCommandError {
    NotApplied(MutationRefusal),
    OutcomeUnknown(UnknownCause),
}

impl MutationCommandError {
    pub(crate) fn from_reply(
        error: &redis::RedisError,
        kind: MutationKind,
        stopped: Option<UnknownCause>,
    ) -> Self {
        let code = error.code();
        if matches!(kind, MutationKind::Set)
            && code == Some("ERR")
            && error.detail().is_some_and(|detail| {
                detail.eq_ignore_ascii_case("invalid expire time in 'set' command")
            })
        {
            return Self::NotApplied(MutationRefusal::InvalidTtl);
        }
        if code == Some("NOPERM") {
            return Self::NotApplied(
                stopped.map_or(MutationRefusal::PermissionDenied, MutationRefusal::from),
            );
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

impl From<MutationCommandError> for MutationError {
    fn from(error: MutationCommandError) -> Self {
        match error {
            MutationCommandError::NotApplied(refusal) => {
                Self::NotApplied(refusal.mutation_failure())
            }
            MutationCommandError::OutcomeUnknown(cause) => {
                Self::OutcomeUnknown(cause.unknown_outcome())
            }
        }
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
