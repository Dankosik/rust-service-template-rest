//! The closed failure set and its mapping from SDK results.

use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};

/// Why an operation failed. Each variant tells the caller what it may do next.
///
/// Display and Debug never contain a key, bucket, endpoint, URL, or provider text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ObjectStorageError {
    /// The object does not exist.
    #[error("object not found")]
    NotFound,
    /// A create-only put found the key already present.
    #[error("object already exists")]
    AlreadyExists,
    /// A declared or stored size exceeds `max_object_bytes`. Nothing was sent.
    #[error("object exceeds the configured size limit")]
    TooLarge,
    /// The process admission limit is full. Nothing was sent.
    #[error("object storage admission limit reached")]
    Busy,
    /// Transient failure; retrying the same operation is safe. A read failed,
    /// or the provider refused a mutation before applying it (409, 429, 503,
    /// or S3's `RequestTimeout`). A mutation makes one attempt, so no earlier
    /// attempt can have applied it.
    #[error("object storage unavailable")]
    Unavailable,
    /// Permanent refusal, such as 400, 403, or a missing bucket: credentials,
    /// configuration, or input are wrong. Retrying will not help.
    #[error("object storage rejected the request")]
    Rejected,
    /// A mutation may or may not have taken effect: a timeout, a lost
    /// response, or a 500, 502, or 504. Reconcile before relying on either
    /// state; a create-only retry may find this call's own object.
    #[error("object storage mutation outcome is unknown")]
    OutcomeUnknown,
    /// A checksum mismatch, a partial or range response, or a malformed response.
    #[error("object storage integrity check failed")]
    Integrity,
}

impl ObjectStorageError {
    /// The bounded `outcome` metric label and span field.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::AlreadyExists => "already_exists",
            Self::TooLarge => "too_large",
            Self::Busy => "busy",
            Self::Unavailable => "unavailable",
            Self::Rejected => "rejected",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Integrity => "integrity",
        }
    }
}

/// What the failed call was, which decides how an ambiguous failure reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Call {
    /// Get, head, or the bucket probe.
    Read,
    /// Put or delete.
    Mutation,
    /// A put with `If-None-Match: *`, sent once.
    CreateOnly,
}

/// The transport-level class of an SDK failure, before the call decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reply<'a> {
    /// The request was never sent: it could not be built or signed.
    NotSent,
    /// No usable response: a timeout, a dispatch failure, or an unreadable response.
    Lost,
    /// A provider error response.
    Status { status: u16, code: Option<&'a str> },
}

/// A classified SDK failure with its span `error.type`.
#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) error: ObjectStorageError,
    /// The provider's error code when it is a short identifier, otherwise the
    /// HTTP status or the transport class. Never provider message text.
    pub(crate) error_type: String,
}

pub(crate) fn from_sdk<E: ProvideErrorMetadata, R>(
    call: Call,
    error: &SdkError<E, R>,
    status: impl Fn(&R) -> u16,
) -> Failure {
    let (reply, error_type) = match error {
        SdkError::ConstructionFailure(_) => (Reply::NotSent, "construction".to_owned()),
        SdkError::ServiceError(context) => {
            let status = status(context.raw());
            let code = context.err().code();
            let error_type = match code {
                Some(code) if is_identifier(code) => code.to_owned(),
                _ => status.to_string(),
            };
            (Reply::Status { status, code }, error_type)
        }
        SdkError::TimeoutError(_) => (Reply::Lost, "timeout".to_owned()),
        SdkError::DispatchFailure(failure) if failure.is_timeout() => {
            (Reply::Lost, "timeout".to_owned())
        }
        SdkError::DispatchFailure(_) => (Reply::Lost, "dispatch".to_owned()),
        // A response that could not be read, and any future variant.
        _ => (Reply::Lost, "response".to_owned()),
    };
    Failure {
        error: classify(call, reply),
        error_type,
    }
}

/// S3 error codes are short `PascalCase` identifiers such as `SlowDown`.
fn is_identifier(code: &str) -> bool {
    !code.is_empty() && code.len() <= 64 && code.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// The one mapping from a provider reply to the failure a caller sees.
pub(crate) fn classify(call: Call, reply: Reply<'_>) -> ObjectStorageError {
    use ObjectStorageError as E;
    match reply {
        Reply::NotSent => E::Rejected,
        Reply::Lost => match call {
            Call::Read => E::Unavailable,
            Call::Mutation | Call::CreateOnly => E::OutcomeUnknown,
        },
        Reply::Status { status, code } => match (status, code) {
            // A missing bucket is configuration, not a missing object. S3
            // answers a delete of a missing key with 204, so a delete 404 is
            // the bucket too.
            (_, Some("NoSuchBucket")) => E::Rejected,
            (404, _) if call == Call::Read => E::NotFound,
            (412, _) if call == Call::CreateOnly => E::AlreadyExists,
            // Refused before applying: a conflicting concurrent write,
            // throttling, a provider shedding load, or a body the provider
            // stopped waiting for (S3's `400 RequestTimeout`). A mutation
            // makes one attempt, so this reply is the only one.
            (409 | 429 | 503, _) | (_, Some("RequestTimeout")) => E::Unavailable,
            // An unsupported header or operation is permanent.
            (501, _) => E::Rejected,
            (500..=599, _) => match call {
                Call::Read => E::Unavailable,
                Call::Mutation | Call::CreateOnly => E::OutcomeUnknown,
            },
            _ => E::Rejected,
        },
    }
}

/// Whether a download body error is a checksum mismatch rather than transport.
pub(crate) fn is_checksum_mismatch(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error
            .downcast_ref::<aws_smithy_checksums::body::validate::Error>()
            .is_some()
        {
            return true;
        }
        current = error.source();
    }
    false
}
