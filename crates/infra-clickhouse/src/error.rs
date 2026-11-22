use operation_context::Stopped;
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

pub(crate) fn classify_query_error(error: &clickhouse::error::Error) -> QueryError {
    match error {
        clickhouse::error::Error::RowNotFound => QueryError::NotFound,
        clickhouse::error::Error::InvalidParams(_) => QueryError::Rejected,
        clickhouse::error::Error::NotEnoughData
        | clickhouse::error::Error::InvalidUtf8Encoding(_)
        | clickhouse::error::Error::InvalidTagEncoding(_)
        | clickhouse::error::Error::VariantDiscriminatorIsOutOfBound(_)
        | clickhouse::error::Error::DeserializeAnyNotSupported
        | clickhouse::error::Error::InvalidColumnsHeader(_)
        | clickhouse::error::Error::SchemaMismatch(_)
        | clickhouse::error::Error::DataFormat(_)
        | clickhouse::error::Error::Decompression(_)
        | clickhouse::error::Error::Custom(_) => QueryError::Decode,
        clickhouse::error::Error::BadResponse(message) => {
            if let Some(code) = extract_exception_code(message) {
                match code {
                    158 | 307 | 396 => QueryError::TooLarge,
                    47 | 53 | 60 | 62 | 164 | 497 | 516 => QueryError::Rejected,
                    _ => QueryError::Unavailable,
                }
            } else {
                match extract_http_status_from_bad_response(message) {
                    Some(400 | 401 | 403) => QueryError::Rejected,
                    _ => QueryError::Unavailable,
                }
            }
        }
        _ => QueryError::Unavailable,
    }
}

pub(crate) fn classify_metadata_error(error: &clickhouse::error::Error) -> InsertFailure {
    match error {
        clickhouse::error::Error::SchemaMismatch(_)
        | clickhouse::error::Error::InvalidColumnsHeader(_)
        | clickhouse::error::Error::DataFormat(_)
        | clickhouse::error::Error::NotEnoughData => InsertFailure::Encode,
        clickhouse::error::Error::BadResponse(message) => {
            if let Some(code) = extract_exception_code(message) {
                match code {
                    47 | 60 | 164 | 497 | 516 => InsertFailure::Rejected,
                    _ => InsertFailure::Unavailable,
                }
            } else {
                match extract_http_status_from_bad_response(message) {
                    Some(400 | 401 | 403) => InsertFailure::Rejected,
                    _ => InsertFailure::Unavailable,
                }
            }
        }
        _ => InsertFailure::Unavailable,
    }
}

pub(crate) fn classify_post_dispatch_insert_error(error: &clickhouse::error::Error) -> InsertError {
    if is_hyper_connect_error(error) {
        return InsertError::NotApplied(InsertFailure::Unavailable);
    }
    if let clickhouse::error::Error::BadResponse(message) = error {
        if let Some(code) = extract_exception_code(message) {
            return match code {
                252 => InsertError::NotApplied(InsertFailure::TooManyParts),
                307 => InsertError::NotApplied(InsertFailure::TooLarge),
                16 | 47 | 53 | 60 | 164 | 497 | 516 => {
                    InsertError::NotApplied(InsertFailure::Rejected)
                }
                _ => InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
            };
        }
        return match extract_http_status_from_bad_response(message) {
            Some(413) => InsertError::NotApplied(InsertFailure::TooLarge),
            Some(400 | 401 | 403) => InsertError::NotApplied(InsertFailure::Rejected),
            _ => InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
        };
    }
    InsertError::OutcomeUnknown(UnknownOutcome::Unavailable)
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
