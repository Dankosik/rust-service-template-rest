use std::fmt;

use operation_context::{BoundedStop, Stopped};
use shared_rules::error_chain;

const MAX_BAD_RESPONSE_SCAN_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error(transparent)]
pub struct StartError(pub(crate) StartErrorKind);

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum StartErrorKind {
    #[error("clickhouse url is invalid")]
    InvalidUrl,
    #[error("clickhouse plaintext http is refused")]
    PlaintextRefused,
    #[error("clickhouse unauthenticated access is refused")]
    UnauthenticatedRefused,
    #[error("clickhouse database name is invalid")]
    InvalidDatabase,
    #[error("clickhouse credentials are invalid")]
    InvalidCredentials,
    #[error("clickhouse password and password_file cannot both be set")]
    PasswordConflict,
    #[error("clickhouse password_file could not be read ({kind})")]
    PasswordFile { kind: std::io::ErrorKind },
    #[error("clickhouse password_file is empty or invalid")]
    PasswordFileEmpty,
    #[error("clickhouse root CA requires https")]
    CaRequiresTls,
    #[error("clickhouse root CA file could not be read ({kind})")]
    CaFile { kind: std::io::ErrorKind },
    #[error("clickhouse root CA is invalid")]
    InvalidCa,
    #[error("clickhouse client certificate requires https")]
    ClientCertificateRequiresTls,
    #[error("clickhouse client certificate file could not be read ({kind})")]
    ClientCertificateFile { kind: std::io::ErrorKind },
    #[error("clickhouse client certificate is invalid")]
    InvalidClientCertificate,
    #[error("clickhouse client key file could not be read ({kind})")]
    ClientKeyFile { kind: std::io::ErrorKind },
    #[error("clickhouse client key is invalid or does not match the client certificate")]
    InvalidClientKey,
    #[error("clickhouse concurrency limit is outside 1..=1024")]
    InvalidConcurrency,
    #[error("clickhouse limit or timeout is outside the supported range")]
    InvalidLimits,
    #[error("clickhouse client could not be built")]
    ClientBuild,
    #[error("clickhouse startup admission timed out")]
    AdmissionTimedOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QueryError {
    #[error("clickhouse query returned no rows")]
    NotFound,
    #[error("clickhouse query returned more than one row")]
    TooManyRows,
    #[error("clickhouse query result exceeded the configured row or byte limit")]
    TooLarge,
    #[error("clickhouse client concurrency limit reached")]
    Busy,
    #[error("clickhouse query failed because the service is unavailable")]
    Unavailable,
    #[error("clickhouse query exceeded a server time or memory limit")]
    ResourceLimit,
    #[error("clickhouse rejected the query")]
    Rejected,
    #[error("clickhouse response could not be decoded")]
    Decode,
    #[error("clickhouse query stopped by caller ({0:?})")]
    Stopped(Stopped),
}

impl QueryError {
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Busy | Self::Unavailable)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InsertFailure {
    #[error("clickhouse insert input is invalid")]
    InvalidInput,
    #[error("clickhouse client concurrency limit reached")]
    Busy,
    #[error("clickhouse rejected the insert due to too many active parts")]
    TooManyParts,
    #[error("clickhouse insert exceeded the maximum payload size")]
    TooLarge,
    #[error("clickhouse row encoding or schema validation failed")]
    Encode,
    #[error("clickhouse rejected the insert")]
    Rejected,
    #[error("clickhouse insert failed before dispatch")]
    Unavailable,
    #[error("clickhouse insert stopped by caller before dispatch ({0:?})")]
    Stopped(Stopped),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum UnknownOutcome {
    #[error("clickhouse insert failed after dispatch")]
    Unavailable,
    #[error("clickhouse insert stopped by caller after dispatch ({0:?})")]
    Stopped(Stopped),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InsertError {
    #[error("clickhouse insert was not applied: {0}")]
    NotApplied(#[source] InsertFailure),
    #[error("clickhouse insert outcome is unknown: {0}")]
    OutcomeUnknown(#[source] UnknownOutcome),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResponseStatusCode {
    ClickHouse(u32),
    Http(u16),
}

impl fmt::Display for ResponseStatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClickHouse(code) => write!(f, "{code}"),
            Self::Http(status) => write!(f, "{status}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureReason {
    NotFound,
    TooManyRows,
    ResultLimit,
    TooManyParts,
    Auth,
    Access,
    Readonly,
    Schema,
    QueryInvalid,
    ServerTimeout,
    MemoryLimit,
    ServerUnknownStatus,
    ServerError,
    ClientTimeout,
    ParentDeadline,
    ParentCancelled,
    FutureDropped,
    Connect,
    Transport,
    Decode,
    Encode,
}

impl FailureReason {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::TooManyRows => "too_many_rows",
            Self::ResultLimit => "result_limit",
            Self::TooManyParts => "too_many_parts",
            Self::Auth => "auth",
            Self::Access => "access",
            Self::Readonly => "readonly",
            Self::Schema => "schema",
            Self::QueryInvalid => "query_invalid",
            Self::ServerTimeout => "server_timeout",
            Self::MemoryLimit => "memory_limit",
            Self::ServerUnknownStatus => "server_unknown_status",
            Self::ServerError => "server_error",
            Self::ClientTimeout => "client_timeout",
            Self::ParentDeadline => "parent_deadline",
            Self::ParentCancelled => "parent_cancelled",
            Self::FutureDropped => "future_dropped",
            Self::Connect => "connect",
            Self::Transport => "transport",
            Self::Decode => "decode",
            Self::Encode => "encode",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FailureDetail {
    pub(crate) reason: FailureReason,
    pub(crate) status_code: Option<ResponseStatusCode>,
}

impl FailureDetail {
    pub(crate) const fn reason(reason: FailureReason) -> Self {
        Self {
            reason,
            status_code: None,
        }
    }

    pub(crate) const fn with_clickhouse_code(reason: FailureReason, code: u32) -> Self {
        Self {
            reason,
            status_code: Some(ResponseStatusCode::ClickHouse(code)),
        }
    }

    pub(crate) const fn with_http_status(reason: FailureReason, status: u16) -> Self {
        Self {
            reason,
            status_code: Some(ResponseStatusCode::Http(status)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ObservedFailure<E> {
    pub(crate) error: E,
    pub(crate) detail: FailureDetail,
}

impl<E> ObservedFailure<E> {
    pub(crate) const fn new(error: E, detail: FailureDetail) -> Self {
        Self { error, detail }
    }

    pub(crate) const fn with_reason(error: E, reason: FailureReason) -> Self {
        Self::new(error, FailureDetail::reason(reason))
    }
}

pub(crate) const fn bounded_stop_reason(stop: BoundedStop) -> FailureReason {
    match stop {
        BoundedStop::Parent(Stopped::Deadline) => FailureReason::ParentDeadline,
        BoundedStop::Parent(Stopped::Cancelled) => FailureReason::ParentCancelled,
        BoundedStop::Timeout => FailureReason::ClientTimeout,
    }
}

const fn clickhouse_code_reason(code: u32) -> FailureReason {
    match code {
        516 => FailureReason::Auth,
        497 => FailureReason::Access,
        164 => FailureReason::Readonly,
        16 | 47 | 53 | 60 | 81 => FailureReason::Schema,
        36 | 43 | 46 | 62 | 70 | 117 | 130 | 153 | 215 | 386 | 456 | 457 => {
            FailureReason::QueryInvalid
        }
        158 | 307 | 396 => FailureReason::ResultLimit,
        159 => FailureReason::ServerTimeout,
        241 => FailureReason::MemoryLimit,
        252 => FailureReason::TooManyParts,
        319 => FailureReason::ServerUnknownStatus,
        _ => FailureReason::ServerError,
    }
}

pub(crate) const fn is_auth_failure(detail: FailureDetail) -> bool {
    matches!(
        detail.status_code,
        Some(ResponseStatusCode::ClickHouse(516) | ResponseStatusCode::Http(401))
    )
}

fn is_result_rows_limit_message(message: &str) -> bool {
    message.contains("Limit for result exceeded") && message.contains("max rows")
}

pub(crate) fn is_header_schema_mismatch(message: &str) -> bool {
    extract_exception_code(message) == Some(117) && message.contains("while reading header")
}

const fn http_status_reason(status: u16) -> FailureReason {
    match status {
        400 => FailureReason::QueryInvalid,
        401 => FailureReason::Auth,
        403 => FailureReason::Access,
        413 => FailureReason::ResultLimit,
        _ => FailureReason::ServerError,
    }
}

pub(crate) fn extract_exception_code(message: &str) -> Option<u32> {
    let bytes = message.as_bytes();
    let end = bytes.len().min(MAX_BAD_RESPONSE_SCAN_BYTES);
    let prefix = bytes.get(..end)?.strip_prefix(b"Code:")?;
    let trimmed = prefix.trim_ascii_start();
    let digits_len = trimmed.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits_len == 0 {
        return None;
    }
    let digits = std::str::from_utf8(trimmed.get(..digits_len)?).ok()?;
    digits.parse::<u32>().ok()
}

pub(crate) fn extract_http_status_from_bad_response(message: &str) -> Option<u16> {
    match message {
        "400 Bad Request" => Some(400),
        "401 Unauthorized" => Some(401),
        "403 Forbidden" => Some(403),
        "413 Payload Too Large" => Some(413),
        _ => None,
    }
}

pub(crate) fn is_hyper_connect_error(error: &clickhouse::error::Error) -> bool {
    error_chain::causes(error).any(|cause| {
        cause
            .downcast_ref::<hyper_util::client::legacy::Error>()
            .is_some_and(hyper_util::client::legacy::Error::is_connect)
    })
}

pub(crate) fn classify_query_error(
    error: &clickhouse::error::Error,
    cardinality_capped: bool,
) -> ObservedFailure<QueryError> {
    match error {
        clickhouse::error::Error::RowNotFound => {
            ObservedFailure::with_reason(QueryError::NotFound, FailureReason::NotFound)
        }
        clickhouse::error::Error::ResultBytesLimitExceeded => {
            ObservedFailure::with_reason(QueryError::TooLarge, FailureReason::ResultLimit)
        }
        clickhouse::error::Error::InvalidParams(_) => {
            ObservedFailure::with_reason(QueryError::Rejected, FailureReason::QueryInvalid)
        }
        clickhouse::error::Error::NotEnoughData
        | clickhouse::error::Error::InvalidUtf8Encoding(_)
        | clickhouse::error::Error::InvalidTagEncoding(_)
        | clickhouse::error::Error::VariantDiscriminatorIsOutOfBound(_)
        | clickhouse::error::Error::DeserializeAnyNotSupported
        | clickhouse::error::Error::InvalidColumnsHeader(_)
        | clickhouse::error::Error::SchemaMismatch(_)
        | clickhouse::error::Error::DataFormat(_)
        | clickhouse::error::Error::Decompression(_)
        | clickhouse::error::Error::Custom(_) => {
            ObservedFailure::with_reason(QueryError::Decode, FailureReason::Decode)
        }
        clickhouse::error::Error::BadResponse(message) => {
            if let Some(code) = extract_exception_code(message) {
                if cardinality_capped && code == 396 && is_result_rows_limit_message(message) {
                    return ObservedFailure::new(
                        QueryError::TooManyRows,
                        FailureDetail::with_clickhouse_code(FailureReason::TooManyRows, code),
                    );
                }
                let error = match code {
                    158 | 307 | 396 => QueryError::TooLarge,
                    159 | 241 => QueryError::ResourceLimit,
                    16 | 36 | 43 | 46 | 47 | 53 | 60 | 62 | 70 | 81 | 117 | 130 | 153 | 164
                    | 215 | 386 | 456 | 457 | 497 | 516 => QueryError::Rejected,
                    _ => QueryError::Unavailable,
                };
                ObservedFailure::new(
                    error,
                    FailureDetail::with_clickhouse_code(clickhouse_code_reason(code), code),
                )
            } else if let Some(status) = extract_http_status_from_bad_response(message) {
                let error = match status {
                    400 | 401 | 403 => QueryError::Rejected,
                    _ => QueryError::Unavailable,
                };
                ObservedFailure::new(
                    error,
                    FailureDetail::with_http_status(http_status_reason(status), status),
                )
            } else {
                ObservedFailure::with_reason(QueryError::Unavailable, FailureReason::ServerError)
            }
        }
        _ => {
            let reason = if is_hyper_connect_error(error) {
                FailureReason::Connect
            } else {
                FailureReason::Transport
            };
            ObservedFailure::with_reason(QueryError::Unavailable, reason)
        }
    }
}

pub(crate) fn classify_metadata_error(
    error: &clickhouse::error::Error,
) -> ObservedFailure<InsertError> {
    match error {
        clickhouse::error::Error::SchemaMismatch(_)
        | clickhouse::error::Error::InvalidColumnsHeader(_)
        | clickhouse::error::Error::DataFormat(_)
        | clickhouse::error::Error::NotEnoughData => ObservedFailure::with_reason(
            InsertError::NotApplied(InsertFailure::Encode),
            FailureReason::Encode,
        ),
        clickhouse::error::Error::BadResponse(message) => {
            if let Some(code) = extract_exception_code(message) {
                let failure = match code {
                    47 | 60 | 81 | 164 | 497 | 516 => InsertFailure::Rejected,
                    _ => InsertFailure::Unavailable,
                };
                ObservedFailure::new(
                    InsertError::NotApplied(failure),
                    FailureDetail::with_clickhouse_code(clickhouse_code_reason(code), code),
                )
            } else if let Some(status) = extract_http_status_from_bad_response(message) {
                let failure = match status {
                    400 | 401 | 403 => InsertFailure::Rejected,
                    _ => InsertFailure::Unavailable,
                };
                ObservedFailure::new(
                    InsertError::NotApplied(failure),
                    FailureDetail::with_http_status(http_status_reason(status), status),
                )
            } else {
                ObservedFailure::with_reason(
                    InsertError::NotApplied(InsertFailure::Unavailable),
                    FailureReason::ServerError,
                )
            }
        }
        _ => {
            let reason = if is_hyper_connect_error(error) {
                FailureReason::Connect
            } else {
                FailureReason::Transport
            };
            ObservedFailure::with_reason(
                InsertError::NotApplied(InsertFailure::Unavailable),
                reason,
            )
        }
    }
}

pub(crate) fn classify_post_dispatch_insert_error(
    error: &clickhouse::error::Error,
    single_row: bool,
) -> ObservedFailure<InsertError> {
    if is_hyper_connect_error(error) {
        return ObservedFailure::with_reason(
            InsertError::NotApplied(InsertFailure::Unavailable),
            FailureReason::Connect,
        );
    }
    if let clickhouse::error::Error::BadResponse(message) = error {
        if let Some(code) = extract_exception_code(message) {
            let header_mismatch = is_header_schema_mismatch(message);
            let mapped = match code {
                252 if single_row => InsertError::NotApplied(InsertFailure::TooManyParts),
                307 => InsertError::NotApplied(InsertFailure::TooLarge),
                c if matches!(c, 16 | 47 | 53 | 60 | 81 | 164 | 497 | 516)
                    || (c == 117 && (header_mismatch || single_row)) =>
                {
                    InsertError::NotApplied(InsertFailure::Rejected)
                }
                _ => InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
            };
            let reason = if header_mismatch {
                FailureReason::Schema
            } else {
                clickhouse_code_reason(code)
            };
            return ObservedFailure::new(mapped, FailureDetail::with_clickhouse_code(reason, code));
        }
        if let Some(status) = extract_http_status_from_bad_response(message) {
            let mapped = match status {
                413 => InsertError::NotApplied(InsertFailure::TooLarge),
                400 | 401 | 403 => InsertError::NotApplied(InsertFailure::Rejected),
                _ => InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
            };
            return ObservedFailure::new(
                mapped,
                FailureDetail::with_http_status(http_status_reason(status), status),
            );
        }
        return ObservedFailure::with_reason(
            InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
            FailureReason::ServerError,
        );
    }
    ObservedFailure::with_reason(
        InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
        FailureReason::Transport,
    )
}

pub(crate) fn is_local_serialization_error(error: &clickhouse::error::Error) -> bool {
    matches!(
        error,
        clickhouse::error::Error::Custom(_)
            | clickhouse::error::Error::SequenceMustHaveLength
            | clickhouse::error::Error::InvalidTagEncoding(_)
            | clickhouse::error::Error::VariantDiscriminatorIsOutOfBound(_)
            | clickhouse::error::Error::SchemaMismatch(_)
    )
}
