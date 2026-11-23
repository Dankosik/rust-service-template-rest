use std::fmt;

use operation_context::{BoundedStop, Stopped};
use shared_rules::error_chain;

const MAX_BAD_RESPONSE_SCAN_BYTES: usize = 256;

const CODE_NO_SUCH_COLUMN_IN_TABLE: u32 = 16;
const CODE_BAD_ARGUMENTS: u32 = 36;
const CODE_ILLEGAL_TYPE_OF_ARGUMENT: u32 = 43;
const CODE_UNKNOWN_FUNCTION: u32 = 46;
const CODE_UNKNOWN_IDENTIFIER: u32 = 47;
const CODE_TYPE_MISMATCH: u32 = 53;
const CODE_UNKNOWN_TABLE: u32 = 60;
const CODE_SYNTAX_ERROR: u32 = 62;
const CODE_CANNOT_CONVERT_TYPE: u32 = 70;
const CODE_UNKNOWN_DATABASE: u32 = 81;
const CODE_INCORRECT_DATA: u32 = 117;
const CODE_CANNOT_READ_ARRAY_FROM_TEXT: u32 = 130;
const CODE_ILLEGAL_DIVISION: u32 = 153;
const CODE_TOO_MANY_ROWS: u32 = 158;
const CODE_TIMEOUT_EXCEEDED: u32 = 159;
const CODE_READONLY: u32 = 164;
const CODE_NOT_AN_AGGREGATE: u32 = 215;
const CODE_MEMORY_LIMIT_EXCEEDED: u32 = 241;
const CODE_TOO_MANY_PARTS: u32 = 252;
const CODE_TOO_MANY_BYTES: u32 = 307;
const CODE_UNKNOWN_STATUS_OF_INSERT: u32 = 319;
const CODE_NO_COMMON_TYPE: u32 = 386;
const CODE_TOO_MANY_ROWS_OR_BYTES: u32 = 396;
const CODE_UNKNOWN_QUERY_PARAMETER: u32 = 456;
const CODE_BAD_QUERY_PARAMETER: u32 = 457;
const CODE_ACCESS_DENIED: u32 = 497;
const CODE_AUTHENTICATION_FAILED: u32 = 516;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error(transparent)]
pub struct StartError(pub(crate) StartErrorKind);

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum StartErrorKind {
    #[error("clickhouse url is invalid")]
    InvalidUrl,
    #[error("clickhouse plaintext http is refused")]
    PlaintextRefused,
    #[error("clickhouse plaintext transport requires an http:// url")]
    PlaintextUrlRequired,
    #[error("clickhouse database name is invalid")]
    InvalidDatabase,
    #[error("clickhouse credentials are invalid")]
    InvalidCredentials,
    #[error("clickhouse password_file could not be read ({kind})")]
    PasswordFile { kind: std::io::ErrorKind },
    #[error("clickhouse password_file is empty or invalid")]
    PasswordFileEmpty,
    #[error("clickhouse root CA file could not be read ({kind})")]
    CaFile { kind: std::io::ErrorKind },
    #[error("clickhouse root CA is invalid")]
    InvalidCa,
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

impl ObservedFailure<QueryError> {
    pub(crate) const fn from_stop(stop: BoundedStop) -> Self {
        let error = match stop {
            BoundedStop::Parent(stopped) => QueryError::Stopped(stopped),
            BoundedStop::Timeout => QueryError::Unavailable,
        };
        Self::with_reason(error, bounded_stop_reason(stop))
    }
}

impl ObservedFailure<InsertError> {
    pub(crate) const fn pre_dispatch_stop(stop: BoundedStop) -> Self {
        let error = match stop {
            BoundedStop::Parent(stopped) => {
                InsertError::NotApplied(InsertFailure::Stopped(stopped))
            }
            BoundedStop::Timeout => InsertError::NotApplied(InsertFailure::Unavailable),
        };
        Self::with_reason(error, bounded_stop_reason(stop))
    }

    pub(crate) const fn post_dispatch_stop(stop: BoundedStop) -> Self {
        let error = match stop {
            BoundedStop::Parent(stopped) => {
                InsertError::OutcomeUnknown(UnknownOutcome::Stopped(stopped))
            }
            BoundedStop::Timeout => InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
        };
        Self::with_reason(error, bounded_stop_reason(stop))
    }
}

const fn bounded_stop_reason(stop: BoundedStop) -> FailureReason {
    match stop {
        BoundedStop::Parent(Stopped::Deadline) => FailureReason::ParentDeadline,
        BoundedStop::Parent(Stopped::Cancelled) => FailureReason::ParentCancelled,
        BoundedStop::Timeout => FailureReason::ClientTimeout,
    }
}

const fn clickhouse_code_reason(code: u32) -> FailureReason {
    match code {
        CODE_AUTHENTICATION_FAILED => FailureReason::Auth,
        CODE_ACCESS_DENIED => FailureReason::Access,
        CODE_READONLY => FailureReason::Readonly,
        CODE_NO_SUCH_COLUMN_IN_TABLE
        | CODE_UNKNOWN_IDENTIFIER
        | CODE_TYPE_MISMATCH
        | CODE_UNKNOWN_TABLE
        | CODE_UNKNOWN_DATABASE => FailureReason::Schema,
        CODE_BAD_ARGUMENTS
        | CODE_ILLEGAL_TYPE_OF_ARGUMENT
        | CODE_UNKNOWN_FUNCTION
        | CODE_SYNTAX_ERROR
        | CODE_CANNOT_CONVERT_TYPE
        | CODE_INCORRECT_DATA
        | CODE_CANNOT_READ_ARRAY_FROM_TEXT
        | CODE_ILLEGAL_DIVISION
        | CODE_NOT_AN_AGGREGATE
        | CODE_NO_COMMON_TYPE
        | CODE_UNKNOWN_QUERY_PARAMETER
        | CODE_BAD_QUERY_PARAMETER => FailureReason::QueryInvalid,
        CODE_TOO_MANY_ROWS | CODE_TOO_MANY_BYTES | CODE_TOO_MANY_ROWS_OR_BYTES => {
            FailureReason::ResultLimit
        }
        CODE_TIMEOUT_EXCEEDED => FailureReason::ServerTimeout,
        CODE_MEMORY_LIMIT_EXCEEDED => FailureReason::MemoryLimit,
        CODE_TOO_MANY_PARTS => FailureReason::TooManyParts,
        CODE_UNKNOWN_STATUS_OF_INSERT => FailureReason::ServerUnknownStatus,
        _ => FailureReason::ServerError,
    }
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

enum ParsedBadResponse {
    ClickHouse {
        code: u32,
        reason: FailureReason,
        header_mismatch: bool,
        result_rows_limit: bool,
    },
    Http {
        status: u16,
        reason: FailureReason,
    },
    Opaque,
}

fn parse_bad_response(message: &str) -> ParsedBadResponse {
    if let Some(code) = extract_exception_code(message) {
        let header_mismatch =
            code == CODE_INCORRECT_DATA && message.contains("while reading header");
        let result_rows_limit = code == CODE_TOO_MANY_ROWS_OR_BYTES
            && message.contains("Limit for result exceeded")
            && message.contains("max rows");
        return ParsedBadResponse::ClickHouse {
            code,
            reason: clickhouse_code_reason(code),
            header_mismatch,
            result_rows_limit,
        };
    }
    if let Some(status) = extract_http_status_from_bad_response(message) {
        return ParsedBadResponse::Http {
            status,
            reason: http_status_reason(status),
        };
    }
    ParsedBadResponse::Opaque
}

pub(crate) const fn is_auth_failure(detail: FailureDetail) -> bool {
    matches!(detail.reason, FailureReason::Auth)
}

pub(crate) fn is_schema_drift_error(error: &clickhouse::error::Error) -> bool {
    match error {
        clickhouse::error::Error::SchemaMismatch(_) => true,
        clickhouse::error::Error::BadResponse(message) => matches!(
            parse_bad_response(message),
            ParsedBadResponse::ClickHouse {
                code: CODE_NO_SUCH_COLUMN_IN_TABLE | CODE_UNKNOWN_IDENTIFIER | CODE_TYPE_MISMATCH,
                ..
            } | ParsedBadResponse::ClickHouse {
                header_mismatch: true,
                ..
            }
        ),
        _ => false,
    }
}

fn extract_exception_code(message: &str) -> Option<u32> {
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

const fn query_error_from_clickhouse_reason(reason: FailureReason) -> QueryError {
    match reason {
        FailureReason::ResultLimit => QueryError::TooLarge,
        FailureReason::ServerTimeout | FailureReason::MemoryLimit => QueryError::ResourceLimit,
        FailureReason::Auth
        | FailureReason::Access
        | FailureReason::Readonly
        | FailureReason::Schema
        | FailureReason::QueryInvalid => QueryError::Rejected,
        _ => QueryError::Unavailable,
    }
}

const fn is_http_rejection(reason: FailureReason) -> bool {
    matches!(
        reason,
        FailureReason::QueryInvalid | FailureReason::Auth | FailureReason::Access
    )
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
        clickhouse::error::Error::BadResponse(message) => match parse_bad_response(message) {
            ParsedBadResponse::ClickHouse {
                code,
                result_rows_limit: true,
                ..
            } if cardinality_capped => ObservedFailure::new(
                QueryError::TooManyRows,
                FailureDetail::with_clickhouse_code(FailureReason::TooManyRows, code),
            ),
            ParsedBadResponse::ClickHouse { code, reason, .. } => ObservedFailure::new(
                query_error_from_clickhouse_reason(reason),
                FailureDetail::with_clickhouse_code(reason, code),
            ),
            ParsedBadResponse::Http { status, reason } => {
                let error = if is_http_rejection(reason) {
                    QueryError::Rejected
                } else {
                    QueryError::Unavailable
                };
                ObservedFailure::new(error, FailureDetail::with_http_status(reason, status))
            }
            ParsedBadResponse::Opaque => {
                ObservedFailure::with_reason(QueryError::Unavailable, FailureReason::ServerError)
            }
        },
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
        clickhouse::error::Error::BadResponse(message) => match parse_bad_response(message) {
            ParsedBadResponse::ClickHouse { code, reason, .. } => {
                let failure = match code {
                    CODE_UNKNOWN_IDENTIFIER
                    | CODE_UNKNOWN_TABLE
                    | CODE_UNKNOWN_DATABASE
                    | CODE_READONLY
                    | CODE_ACCESS_DENIED
                    | CODE_AUTHENTICATION_FAILED => InsertFailure::Rejected,
                    _ => InsertFailure::Unavailable,
                };
                ObservedFailure::new(
                    InsertError::NotApplied(failure),
                    FailureDetail::with_clickhouse_code(reason, code),
                )
            }
            ParsedBadResponse::Http { status, reason } => {
                let failure = if is_http_rejection(reason) {
                    InsertFailure::Rejected
                } else {
                    InsertFailure::Unavailable
                };
                ObservedFailure::new(
                    InsertError::NotApplied(failure),
                    FailureDetail::with_http_status(reason, status),
                )
            }
            ParsedBadResponse::Opaque => ObservedFailure::with_reason(
                InsertError::NotApplied(InsertFailure::Unavailable),
                FailureReason::ServerError,
            ),
        },
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
        return match parse_bad_response(message) {
            ParsedBadResponse::ClickHouse {
                code,
                reason,
                header_mismatch,
                ..
            } => {
                let mapped = match code {
                    CODE_TOO_MANY_PARTS if single_row => {
                        InsertError::NotApplied(InsertFailure::TooManyParts)
                    }
                    CODE_TOO_MANY_BYTES => InsertError::NotApplied(InsertFailure::TooLarge),
                    CODE_INCORRECT_DATA if header_mismatch || single_row => {
                        InsertError::NotApplied(InsertFailure::Rejected)
                    }
                    _ if matches!(
                        reason,
                        FailureReason::Schema
                            | FailureReason::Readonly
                            | FailureReason::Access
                            | FailureReason::Auth
                    ) =>
                    {
                        InsertError::NotApplied(InsertFailure::Rejected)
                    }
                    _ => InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
                };
                let effective_reason = if header_mismatch {
                    FailureReason::Schema
                } else {
                    reason
                };
                ObservedFailure::new(
                    mapped,
                    FailureDetail::with_clickhouse_code(effective_reason, code),
                )
            }
            ParsedBadResponse::Http { status, reason } => {
                let mapped = match reason {
                    FailureReason::ResultLimit => InsertError::NotApplied(InsertFailure::TooLarge),
                    _ if is_http_rejection(reason) => {
                        InsertError::NotApplied(InsertFailure::Rejected)
                    }
                    _ => InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
                };
                ObservedFailure::new(mapped, FailureDetail::with_http_status(reason, status))
            }
            ParsedBadResponse::Opaque => ObservedFailure::with_reason(
                InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
                FailureReason::ServerError,
            ),
        };
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
