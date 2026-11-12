//! The closed failure sets, for construction and for operations, and the
//! mapping from SDK results.

use std::borrow::Cow;

use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::operation::delete_object::{DeleteObjectError, DeleteObjectOutput};
use operation_context::Stopped;

/// Why an operation failed. Each variant tells the caller what it may do next.
///
/// Display and Debug never contain a key, bucket, endpoint, URL, or provider text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, enum_map::Enum, thiserror::Error)]
pub enum ObjectStorageError {
    #[error("object not found")]
    NotFound,
    #[error("object already exists")]
    AlreadyExists,
    /// A declared or stored size exceeds `max_object_bytes`. A put sends
    /// nothing; a get refuses the body after receiving its headers.
    #[error("object exceeds the configured size limit")]
    TooLarge,
    /// The process admission limit is full. Nothing was sent.
    #[error("object storage admission limit reached")]
    Busy,
    /// Transient failure; retrying the same operation is safe. A read failed,
    /// the workload identity yielded no credentials (nothing was sent), or
    /// the provider refused a mutation before applying it (409, 429, 503, or
    /// S3's `RequestTimeout`). A mutation makes one attempt, so no earlier
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
    /// A checksum mismatch detected by the SDK, a range or unsized response,
    /// or a download body whose length differs from its declared size.
    /// Absent or skipped checksums do not produce this error.
    #[error("object storage integrity check failed")]
    Integrity,
}

impl ObjectStorageError {
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

/// Why the options were refused. Display names the key, never its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("object_storage.bucket is not a valid dotless bucket name")]
    Bucket,
    /// Not a region this provider accepts.
    #[error("object_storage.region is not valid for the provider")]
    Region,
    /// Missing, malformed, or not the origin shape this provider requires.
    #[error("object_storage.endpoint is not valid for the provider")]
    Endpoint,
    #[error("object_storage.expected_bucket_owner must be a 12-digit account id")]
    ExpectedBucketOwner,
    /// The provider does not take this credential source, or its credential
    /// chain could not be built.
    #[error("object_storage.credentials is not usable for the selected provider")]
    Credentials,
    #[error("object_storage.access_key_id is required")]
    AccessKeyId,
    #[error("object_storage.secret_access_key is required")]
    SecretAccessKey,
    /// No operation could be admitted, or more than a semaphore can count.
    #[error("object_storage.max_concurrency is out of range")]
    MaxConcurrency,
}

/// Whether a call reads or mutates, which decides its timeout and retry policy
/// and how an ambiguous failure reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Call {
    /// Get, head, presign, or the bucket probe.
    Read,
    /// A replacing put or a delete.
    UnconditionalMutation,
    /// A put with `If-None-Match: *`, sent once.
    CreateOnly,
}

impl Call {
    /// A failure that may have reached the provider: a read retries safely,
    /// a mutation may have applied.
    fn ambiguous(self) -> ObjectStorageError {
        match self {
            Self::Read => ObjectStorageError::Unavailable,
            Self::UnconditionalMutation | Self::CreateOnly => ObjectStorageError::OutcomeUnknown,
        }
    }
}

/// The transport-level class of an SDK failure, before the call decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind<'a> {
    /// The request was never sent: it could not be built or signed.
    NotSent,
    /// The request was never sent: the credential source yielded none.
    NoCredentials,
    /// No usable response: a timeout, a dispatch failure, or an unreadable response.
    Lost,
    /// A provider error response.
    Status { status: u16, code: Option<&'a str> },
}

/// A classified failure with its span `error.type`.
#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) error: ObjectStorageError,
    /// The provider's error code when it is a short identifier, otherwise the
    /// HTTP status, the transport class, or a fixed local label. Never
    /// provider message text.
    pub(crate) error_type: Cow<'static, str>,
}

impl Failure {
    fn with_label(error: ObjectStorageError, error_type: &'static str) -> Self {
        Self {
            error,
            error_type: Cow::Borrowed(error_type),
        }
    }

    /// The stopped context maps to unavailable, including when a read has
    /// already received headers or started streaming.
    pub(crate) fn stopped(stopped: Stopped) -> Self {
        Self::with_label(ObjectStorageError::Unavailable, stop_label(stopped))
    }

    pub(crate) fn stopped_after_poll(call: Call, stopped: Stopped) -> Self {
        Self::with_label(classify(call, FailureKind::Lost), stop_label(stopped))
    }
}

#[derive(Debug)]
pub(crate) enum LocalFailure {
    Busy,
    TooLarge,
    UploadBody,
    UploadLengthMismatch,
    DownloadBody,
    DownloadTimerExited,
    ContentLength,
    ContentRange,
    ChecksumMismatch,
    PresignExpiry,
    PresignedHeaders,
}

impl From<LocalFailure> for Failure {
    fn from(cause: LocalFailure) -> Self {
        use ObjectStorageError as E;
        let (error, label) = match cause {
            LocalFailure::Busy => (E::Busy, "busy"),
            LocalFailure::TooLarge => (E::TooLarge, "too_large"),
            LocalFailure::UploadBody => (E::Rejected, "body"),
            LocalFailure::UploadLengthMismatch => (E::Rejected, "body_length"),
            LocalFailure::DownloadBody => (E::Unavailable, "body"),
            LocalFailure::DownloadTimerExited => (E::Unavailable, "download_timer_exited"),
            LocalFailure::ContentLength => (E::Integrity, "content_length"),
            LocalFailure::ContentRange => (E::Integrity, "content_range"),
            LocalFailure::ChecksumMismatch => (E::Integrity, "checksum"),
            LocalFailure::PresignExpiry => (E::Rejected, "expires_in"),
            LocalFailure::PresignedHeaders => (E::Rejected, "presigned_headers"),
        };
        Self::with_label(error, label)
    }
}

fn stop_label(stopped: Stopped) -> &'static str {
    match stopped {
        Stopped::Deadline => "timeout",
        Stopped::Cancelled => "cancelled",
    }
}

type SdkFailure<E> = SdkError<E, HttpResponse>;

pub(crate) fn interpret_delete_result(
    result: &Result<DeleteObjectOutput, SdkFailure<DeleteObjectError>>,
) -> Result<(), Failure> {
    match result {
        Ok(_) => Ok(()),
        // S3 and R2 answer 204 for an absent key. Some stores report NoSuchKey.
        Err(error) if error.code() == Some("NoSuchKey") => Ok(()),
        Err(error) => Err(from_sdk(Call::UnconditionalMutation, error)),
    }
}

pub(crate) fn from_sdk<E>(call: Call, error: &SdkFailure<E>) -> Failure
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    let (kind, error_type): (_, Cow<'static, str>) = match error {
        SdkError::ConstructionFailure(_) => (FailureKind::NotSent, "construction".into()),
        SdkError::ServiceError(context) => {
            let status = context.raw().status().as_u16();
            let code = context.err().code();
            let error_type = match code {
                Some(code) if is_identifier(code) => code.to_owned().into(),
                _ => status.to_string().into(),
            };
            (FailureKind::Status { status, code }, error_type)
        }
        // The SDK reports a failed credential load as a dispatch failure,
        // although it happens before the request is signed.
        SdkError::DispatchFailure(_)
            if shared_rules::error_chain::causes(error).any(
                <dyn std::error::Error>::is::<
                    aws_credential_types::provider::error::CredentialsError,
                >,
            ) =>
        {
            (FailureKind::NoCredentials, "credentials".into())
        }
        SdkError::TimeoutError(_) => (FailureKind::Lost, "timeout".into()),
        SdkError::DispatchFailure(failure) if failure.is_timeout() => {
            (FailureKind::Lost, "timeout".into())
        }
        SdkError::DispatchFailure(_) => (FailureKind::Lost, "dispatch".into()),
        // A response that could not be read, and any future variant.
        _ => (FailureKind::Lost, "response".into()),
    };
    Failure {
        error: classify(call, kind),
        error_type,
    }
}

/// S3 error codes are short `PascalCase` identifiers such as `SlowDown`.
fn is_identifier(code: &str) -> bool {
    !code.is_empty() && code.len() <= 64 && code.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// The one mapping from a provider reply to the failure a caller sees.
fn classify(call: Call, kind: FailureKind<'_>) -> ObjectStorageError {
    use ObjectStorageError as E;
    match kind {
        FailureKind::NotSent => E::Rejected,
        FailureKind::NoCredentials => E::Unavailable,
        FailureKind::Lost => call.ambiguous(),
        FailureKind::Status { status, code } => match (classification_status(status, code), code) {
            // A missing bucket is a configuration failure even on a read.
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
            (500..=599, _) => call.ambiguous(),
            _ => E::Rejected,
        },
    }
}

/// The status a reply's error stands for. S3 can answer a PUT or DELETE with
/// 200 and an `<Error>` body when it fails after accepting the request, so a
/// success status carries no cause: the code names it, and an unknown code is
/// a server failure because the mutation may have applied.
fn classification_status(status: u16, code: Option<&str>) -> u16 {
    if !(200..300).contains(&status) {
        return status;
    }
    match code {
        Some("SlowDown" | "ServiceUnavailable") => 503,
        Some("PreconditionFailed") => 412,
        Some("ConditionalRequestConflict" | "OperationAborted") => 409,
        _ => 500,
    }
}

/// Whether a download body error is a checksum mismatch rather than transport.
pub(crate) fn is_checksum_mismatch(error: &(dyn std::error::Error + 'static)) -> bool {
    shared_rules::error_chain::causes(error)
        .any(<dyn std::error::Error>::is::<aws_smithy_checksums::body::validate::Error>)
}

#[cfg(test)]
mod tests {
    use super::Call::UnconditionalMutation as Unconditional;
    use super::{Call, FailureKind, ObjectStorageError, classify};

    #[test]
    fn classification_separates_not_applied_from_unknown() {
        use ObjectStorageError as E;
        let status = |status, code| FailureKind::Status { status, code };
        let cases = [
            (Call::Read, FailureKind::Lost, E::Unavailable),
            (Unconditional, FailureKind::Lost, E::OutcomeUnknown),
            (Call::CreateOnly, FailureKind::Lost, E::OutcomeUnknown),
            (Unconditional, FailureKind::NotSent, E::Rejected),
            (Call::Read, status(404, Some("NoSuchKey")), E::NotFound),
            (Call::Read, status(404, Some("NotFound")), E::NotFound),
            (Call::Read, status(404, None), E::NotFound),
            (Call::Read, status(404, Some("NoSuchBucket")), E::Rejected),
            (
                Unconditional,
                status(404, Some("NoSuchBucket")),
                E::Rejected,
            ),
            (
                Call::CreateOnly,
                status(412, Some("PreconditionFailed")),
                E::AlreadyExists,
            ),
            (
                Unconditional,
                status(412, Some("PreconditionFailed")),
                E::Rejected,
            ),
            (
                Call::CreateOnly,
                status(409, Some("ConditionalRequestConflict")),
                E::Unavailable,
            ),
            (
                Unconditional,
                status(429, Some("TooManyRequests")),
                E::Unavailable,
            ),
            (Unconditional, status(503, Some("SlowDown")), E::Unavailable),
            (
                Unconditional,
                status(500, Some("InternalError")),
                E::OutcomeUnknown,
            ),
            (Call::CreateOnly, status(502, None), E::OutcomeUnknown),
            (
                Call::Read,
                status(500, Some("InternalError")),
                E::Unavailable,
            ),
            (Call::Read, status(403, Some("AccessDenied")), E::Rejected),
            (Unconditional, status(400, Some("BadDigest")), E::Rejected),
            (
                Unconditional,
                status(403, Some("SignatureDoesNotMatch")),
                E::Rejected,
            ),
            (
                Unconditional,
                status(400, Some("RequestTimeout")),
                E::Unavailable,
            ),
            (
                Unconditional,
                status(501, Some("NotImplemented")),
                E::Rejected,
            ),
            (Call::Read, status(501, Some("NotImplemented")), E::Rejected),
            // A 2xx reply with an error body is classified by its code.
            (
                Unconditional,
                status(200, Some("InternalError")),
                E::OutcomeUnknown,
            ),
            (Unconditional, status(200, Some("SlowDown")), E::Unavailable),
            (
                Call::CreateOnly,
                status(200, Some("PreconditionFailed")),
                E::AlreadyExists,
            ),
            (
                Call::CreateOnly,
                status(200, Some("ConditionalRequestConflict")),
                E::Unavailable,
            ),
            (Unconditional, status(200, None), E::OutcomeUnknown),
            (
                Unconditional,
                status(200, Some("NoSuchBucket")),
                E::Rejected,
            ),
        ];
        for (call, reply, expected) in cases {
            assert_eq!(classify(call, reply), expected, "{call:?} {reply:?}");
        }
    }

    #[test]
    fn errors_display_no_provider_detail() {
        for error in [
            ObjectStorageError::NotFound,
            ObjectStorageError::OutcomeUnknown,
            ObjectStorageError::Integrity,
        ] {
            let rendered = error.to_string();
            assert!(rendered.starts_with("object"), "{rendered}");
        }
    }
}
