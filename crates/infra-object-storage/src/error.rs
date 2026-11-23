use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::operation::delete_object::{DeleteObjectError, DeleteObjectOutput};
use aws_sdk_s3::operation::put_object::{PutObjectError, PutObjectOutput};
use aws_sdk_s3::primitives::ByteStreamError;
use aws_smithy_runtime::client::http::body::content_length_enforcement::ContentLengthError;
use compact_str::CompactString;
use operation_context::{BoundedStop, Stopped};
use shared_rules::error_chain;

use crate::call::Interrupted;
use crate::observe::FailureOutcome;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ObjectStorageError {
    #[error("object not found")]
    NotFound,
    #[error("object exceeds the configured size limit")]
    TooLarge,
    #[error("object storage admission limit reached")]
    Busy,
    #[error("object storage unavailable")]
    Unavailable,
    #[error("object storage rejected the request")]
    Rejected,
    #[error("object storage integrity check failed")]
    Integrity,
    #[error("object storage operation stopped: {0:?}")]
    Stopped(Stopped),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UnknownOutcome {
    #[error("object storage unavailable")]
    Unavailable,
    #[error("object storage operation stopped: {0:?}")]
    Stopped(Stopped),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MutationError {
    #[error("object storage mutation was not applied: {0}")]
    NotApplied(#[source] MutationFailure),
    #[error("object storage mutation outcome is unknown: {0}")]
    OutcomeUnknown(#[source] UnknownOutcome),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MutationFailure {
    #[error("object already exists")]
    AlreadyExists,
    #[error("object storage conditional write conflicted with another operation")]
    Conflict,
    #[error("object exceeds the configured size limit")]
    TooLarge,
    #[error("object storage admission limit reached")]
    Busy,
    #[error("object storage unavailable")]
    Unavailable,
    #[error("object storage rejected the request")]
    Rejected,
    #[error("object storage operation stopped: {0:?}")]
    Stopped(Stopped),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(transparent)]
pub struct ConfigError(pub(crate) ConfigErrorKind);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ConfigErrorKind {
    #[error("object_storage.bucket is not a valid dotless bucket name")]
    Bucket,
    #[error("object_storage.region is not valid for the provider")]
    Region,
    #[error("object_storage.endpoint is not valid for the provider")]
    Endpoint,
    #[error("object_storage.expected_bucket_owner must be a 12-digit account id")]
    ExpectedBucketOwner,
    #[error(
        "AWS_ROLE_ARN and AWS_WEB_IDENTITY_TOKEN_FILE must both have nonblank Unicode values when configuring web identity"
    )]
    WebIdentity,
    #[error("object_storage.access_key_id is required")]
    AccessKeyId,
    #[error("object_storage.secret_access_key is required")]
    SecretAccessKey,
    #[error("object_storage.max_concurrency is out of range")]
    MaxConcurrency,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MutationMode {
    Unconditional,
    CreateOnly,
}

impl MutationMode {
    pub(crate) fn if_none_match(self) -> Option<String> {
        matches!(self, Self::CreateOnly).then(|| "*".to_owned())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind<'a> {
    NotSent,
    NoCredentials,
    Lost,
    Timeout,
    Status { status: u16, code: Option<&'a str> },
}

pub(crate) trait OutcomeError: Copy {
    fn failure_outcome(self) -> FailureOutcome;
    fn busy() -> Self;
    fn too_large() -> Self;
    fn not_applied_stop(stopped: BoundedStop) -> Self;
}

impl OutcomeError for ObjectStorageError {
    fn failure_outcome(self) -> FailureOutcome {
        match self {
            Self::NotFound => FailureOutcome::NotFound,
            Self::TooLarge => FailureOutcome::TooLarge,
            Self::Busy => FailureOutcome::Busy,
            Self::Unavailable | Self::Stopped(_) => FailureOutcome::Unavailable,
            Self::Rejected => FailureOutcome::Rejected,
            Self::Integrity => FailureOutcome::Integrity,
        }
    }

    fn busy() -> Self {
        Self::Busy
    }

    fn too_large() -> Self {
        Self::TooLarge
    }

    fn not_applied_stop(stopped: BoundedStop) -> Self {
        match stopped {
            BoundedStop::Parent(stopped) => Self::Stopped(stopped),
            BoundedStop::Timeout => Self::Unavailable,
        }
    }
}

impl OutcomeError for MutationError {
    fn failure_outcome(self) -> FailureOutcome {
        match self {
            Self::OutcomeUnknown(_) => FailureOutcome::Unknown,
            Self::NotApplied(MutationFailure::AlreadyExists) => FailureOutcome::AlreadyExists,
            Self::NotApplied(MutationFailure::TooLarge) => FailureOutcome::TooLarge,
            Self::NotApplied(MutationFailure::Busy) => FailureOutcome::Busy,
            Self::NotApplied(
                MutationFailure::Conflict
                | MutationFailure::Unavailable
                | MutationFailure::Stopped(_),
            ) => FailureOutcome::Unavailable,
            Self::NotApplied(MutationFailure::Rejected) => FailureOutcome::Rejected,
        }
    }

    fn busy() -> Self {
        Self::NotApplied(MutationFailure::Busy)
    }

    fn too_large() -> Self {
        Self::NotApplied(MutationFailure::TooLarge)
    }

    fn not_applied_stop(stopped: BoundedStop) -> Self {
        let reason = match stopped {
            BoundedStop::Parent(stopped) => MutationFailure::Stopped(stopped),
            BoundedStop::Timeout => MutationFailure::Unavailable,
        };
        Self::NotApplied(reason)
    }
}

#[derive(Debug)]
pub(crate) struct Failure<E = ObjectStorageError> {
    pub(crate) error: E,
    pub(crate) error_type: CompactString,
    pub(crate) timeout_source: Option<&'static str>,
}

impl<E> Failure<E> {
    const fn with_error_type(error: E, error_type: &'static str) -> Self {
        Self {
            error,
            error_type: CompactString::const_new(error_type),
            timeout_source: None,
        }
    }

    const fn with_stop(error: E, stopped: BoundedStop) -> Self {
        let (error_type, timeout_source) = match stopped {
            BoundedStop::Parent(Stopped::Deadline) => ("timeout", Some("parent_deadline")),
            BoundedStop::Timeout => ("timeout", Some("storage_timeout")),
            BoundedStop::Parent(Stopped::Cancelled) => ("cancelled", None),
        };
        Self {
            error,
            error_type: CompactString::const_new(error_type),
            timeout_source,
        }
    }
}

impl<E: OutcomeError> Failure<E> {
    pub(crate) fn busy() -> Self {
        Self::with_error_type(E::busy(), "busy")
    }

    pub(crate) fn too_large() -> Self {
        Self::with_error_type(E::too_large(), "too_large")
    }

    pub(crate) fn not_applied_stop(stopped: BoundedStop) -> Self {
        Self::with_stop(E::not_applied_stop(stopped), stopped)
    }
}

impl From<Interrupted> for Failure {
    fn from(interrupted: Interrupted) -> Self {
        Self::not_applied_stop(interrupted.stopped)
    }
}

impl Failure<MutationError> {
    const fn not_applied(reason: MutationFailure, error_type: &'static str) -> Self {
        Self::with_error_type(MutationError::NotApplied(reason), error_type)
    }

    pub(crate) const fn mutation_unknown_stopped(stopped: BoundedStop) -> Self {
        let reason = match stopped {
            BoundedStop::Parent(stopped) => UnknownOutcome::Stopped(stopped),
            BoundedStop::Timeout => UnknownOutcome::Unavailable,
        };
        Self::with_stop(MutationError::OutcomeUnknown(reason), stopped)
    }
}

impl From<Interrupted> for Failure<MutationError> {
    fn from(interrupted: Interrupted) -> Self {
        if interrupted.polled {
            Self::mutation_unknown_stopped(interrupted.stopped)
        } else {
            Self::not_applied_stop(interrupted.stopped)
        }
    }
}

#[derive(Debug)]
pub(crate) enum LocalFailure {
    DownloadAllocation,
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
        let (error, error_type) = match cause {
            LocalFailure::DownloadAllocation => (E::Unavailable, "download_allocation"),
            LocalFailure::DownloadBody => (E::Unavailable, "body"),
            LocalFailure::DownloadTimerExited => (E::Unavailable, "download_timer_exited"),
            LocalFailure::ContentLength => (E::Integrity, "content_length"),
            LocalFailure::ContentRange => (E::Integrity, "content_range"),
            LocalFailure::ChecksumMismatch => (E::Integrity, "checksum"),
            LocalFailure::PresignExpiry => (E::Rejected, "expires_in"),
            LocalFailure::PresignedHeaders => (E::Rejected, "presigned_headers"),
        };
        Self::with_error_type(error, error_type)
    }
}

#[derive(Debug)]
pub(crate) enum UploadFailure {
    Body,
    Length,
}

impl From<UploadFailure> for Failure<MutationError> {
    fn from(cause: UploadFailure) -> Self {
        let error_type = match cause {
            UploadFailure::Body => "body",
            UploadFailure::Length => "body_length",
        };
        Self::not_applied(MutationFailure::Rejected, error_type)
    }
}

type SdkFailure<E> = SdkError<E, HttpResponse>;
type PutReply = Result<PutObjectOutput, SdkFailure<PutObjectError>>;
pub(crate) type PutCallResult = Result<(PutReply, Option<BoundedStop>), Interrupted>;

pub(crate) fn interpret_put_result(
    result: &PutCallResult,
    mode: MutationMode,
    mismatch: Option<&AtomicBool>,
) -> Result<(), Failure<MutationError>> {
    let length_mismatch = mismatch.is_some_and(|flag| flag.load(Ordering::Acquire));
    match result {
        Ok((Ok(_), _)) => Ok(()),
        Ok((Err(sdk_error), stopped_on_reply)) => Err(from_sdk_mutation(
            sdk_error,
            mode,
            length_mismatch,
            *stopped_on_reply,
        )),
        Err(_) if length_mismatch => Err(UploadFailure::Length.into()),
        Err(interrupted) => Err((*interrupted).into()),
    }
}

pub(crate) fn interpret_delete_result(
    result: &Result<DeleteObjectOutput, SdkFailure<DeleteObjectError>>,
    stopped_on_reply: Option<BoundedStop>,
) -> Result<(), Failure<MutationError>> {
    match result {
        Ok(_) => Ok(()),
        Err(error) if error.code() == Some("NoSuchKey") => Ok(()),
        Err(error) => Err(from_sdk_mutation(
            error,
            MutationMode::Unconditional,
            false,
            stopped_on_reply,
        )),
    }
}

pub(crate) fn from_sdk<E>(error: &SdkFailure<E>) -> Failure
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    let (kind, error_type) = sdk_failure(error);
    Failure {
        error: classify_read(kind),
        error_type,
        timeout_source: None,
    }
}

fn from_sdk_mutation<E>(
    error: &SdkFailure<E>,
    mode: MutationMode,
    length_mismatch: bool,
    stopped_on_reply: Option<BoundedStop>,
) -> Failure<MutationError>
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    let (kind, error_type) = sdk_failure(error);
    if length_mismatch && matches!(kind, FailureKind::Lost | FailureKind::Timeout) {
        return UploadFailure::Length.into();
    }
    if kind == FailureKind::Timeout
        && let Some(stopped @ BoundedStop::Parent(Stopped::Deadline)) = stopped_on_reply
    {
        return Failure::mutation_unknown_stopped(stopped);
    }
    Failure {
        error: classify_mutation(kind, mode),
        error_type,
        timeout_source: None,
    }
}

fn sdk_failure<E>(error: &SdkFailure<E>) -> (FailureKind<'_>, CompactString)
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    match error {
        SdkError::ConstructionFailure(_) => (
            FailureKind::NotSent,
            CompactString::const_new("construction"),
        ),
        SdkError::ServiceError(context) => {
            let status = context.raw().status().as_u16();
            let code = context.err().code();
            let error_type = match code {
                Some(code) if is_safe_error_code(code) => CompactString::from(code),
                _ => CompactString::from(status.format_into(&mut fmt::NumBuffer::new())),
            };
            (FailureKind::Status { status, code }, error_type)
        }
        SdkError::TimeoutError(_) => (FailureKind::Timeout, CompactString::const_new("timeout")),
        SdkError::DispatchFailure(failure) => {
            if error_chain::contains::<aws_credential_types::provider::error::CredentialsError>(
                error,
            ) {
                (
                    FailureKind::NoCredentials,
                    CompactString::const_new("credentials"),
                )
            } else if failure.is_timeout() {
                (FailureKind::Timeout, CompactString::const_new("timeout"))
            } else {
                (FailureKind::Lost, CompactString::const_new("dispatch"))
            }
        }
        _ => (FailureKind::Lost, CompactString::const_new("response")),
    }
}

const MAX_ERROR_CODE_BYTES: usize = 64;

fn is_safe_error_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= MAX_ERROR_CODE_BYTES
        && code.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn classify_read(kind: FailureKind<'_>) -> ObjectStorageError {
    use ObjectStorageError as E;
    if kind.object_status() == Some(404) {
        return E::NotFound;
    }
    match kind.classify_common() {
        CommonFailure::Unavailable | CommonFailure::OutcomeUnknown => E::Unavailable,
        CommonFailure::Rejected => E::Rejected,
    }
}

fn classify_mutation(kind: FailureKind<'_>, mode: MutationMode) -> MutationError {
    use MutationFailure as F;
    if mode == MutationMode::CreateOnly {
        match kind.object_status() {
            Some(412) => return MutationError::NotApplied(F::AlreadyExists),
            Some(409) => return MutationError::NotApplied(F::Conflict),
            _ => {}
        }
    }
    match kind.classify_common() {
        CommonFailure::Unavailable => MutationError::NotApplied(F::Unavailable),
        CommonFailure::Rejected => MutationError::NotApplied(F::Rejected),
        CommonFailure::OutcomeUnknown => MutationError::OutcomeUnknown(UnknownOutcome::Unavailable),
    }
}

enum CommonFailure {
    Unavailable,
    Rejected,
    OutcomeUnknown,
}

impl FailureKind<'_> {
    fn object_status(self) -> Option<u16> {
        match self {
            Self::Status { status, code } if code != Some("NoSuchBucket") => {
                Some(effective_status(status, code))
            }
            _ => None,
        }
    }

    fn classify_common(self) -> CommonFailure {
        match self {
            Self::NotSent => CommonFailure::Rejected,
            Self::NoCredentials => CommonFailure::Unavailable,
            Self::Lost | Self::Timeout => CommonFailure::OutcomeUnknown,
            Self::Status { status, code } => match (effective_status(status, code), code) {
                (_, Some("NoSuchBucket")) => CommonFailure::Rejected,
                (409 | 429 | 503, _) | (_, Some("RequestTimeout")) => CommonFailure::Unavailable,
                (501, _) => CommonFailure::Rejected,
                (500..=599, _) => CommonFailure::OutcomeUnknown,
                _ => CommonFailure::Rejected,
            },
        }
    }
}

fn effective_status(status: u16, code: Option<&str>) -> u16 {
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

pub(crate) fn from_download_body(error: &ByteStreamError) -> Failure {
    if error_chain::contains::<aws_smithy_checksums::body::validate::Error>(error) {
        LocalFailure::ChecksumMismatch.into()
    } else if error_chain::contains::<ContentLengthError>(error) {
        LocalFailure::ContentLength.into()
    } else {
        LocalFailure::DownloadBody.into()
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Duration;

    use aws_smithy_runtime_api::client::result::ConnectorError;
    use operation_context::{OperationContext, Stopped};

    use super::MutationError::{NotApplied, OutcomeUnknown};
    use super::MutationMode::{CreateOnly, Unconditional};
    use super::{
        DeleteObjectError, FailureKind, MutationFailure, ObjectStorageError, SdkFailure,
        UnknownOutcome, classify_mutation, classify_read, from_sdk_mutation,
    };

    #[test]
    fn classification_separates_not_applied_from_unknown() {
        use MutationFailure as F;
        let status = |status, code| FailureKind::Status { status, code };
        let unknown = OutcomeUnknown(UnknownOutcome::Unavailable);
        let rejected = NotApplied(F::Rejected);
        let unavailable = NotApplied(F::Unavailable);
        let cases = [
            (Unconditional, FailureKind::Lost, unknown),
            (CreateOnly, FailureKind::Lost, unknown),
            (Unconditional, FailureKind::NotSent, rejected),
            (Unconditional, FailureKind::NoCredentials, unavailable),
            (Unconditional, status(404, Some("NoSuchBucket")), rejected),
            (
                CreateOnly,
                status(412, Some("PreconditionFailed")),
                NotApplied(F::AlreadyExists),
            ),
            (
                Unconditional,
                status(412, Some("PreconditionFailed")),
                rejected,
            ),
            (
                CreateOnly,
                status(409, Some("ConditionalRequestConflict")),
                NotApplied(F::Conflict),
            ),
            (
                Unconditional,
                status(429, Some("TooManyRequests")),
                unavailable,
            ),
            (Unconditional, status(503, Some("SlowDown")), unavailable),
            (Unconditional, status(500, Some("InternalError")), unknown),
            (CreateOnly, status(502, None), unknown),
            (Unconditional, status(400, Some("BadDigest")), rejected),
            (
                Unconditional,
                status(403, Some("SignatureDoesNotMatch")),
                rejected,
            ),
            (
                Unconditional,
                status(400, Some("RequestTimeout")),
                unavailable,
            ),
            (Unconditional, status(501, Some("NotImplemented")), rejected),
            (Unconditional, status(200, Some("InternalError")), unknown),
            (Unconditional, status(200, Some("SlowDown")), unavailable),
            (
                CreateOnly,
                status(200, Some("PreconditionFailed")),
                NotApplied(F::AlreadyExists),
            ),
            (
                CreateOnly,
                status(200, Some("ConditionalRequestConflict")),
                NotApplied(F::Conflict),
            ),
            (Unconditional, status(200, None), unknown),
            (Unconditional, status(200, Some("NoSuchBucket")), rejected),
        ];
        for (mode, reply, expected) in cases {
            assert_eq!(
                classify_mutation(reply, mode),
                expected,
                "{mode:?} {reply:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn sdk_mutation_timeout_preserves_the_effective_parent_deadline() {
        let budget = Duration::from_secs(1);
        let parent = OperationContext::with_timeout(budget);
        let parent_limited = parent.bounded(2 * budget);
        let locally_limited = OperationContext::unbounded().bounded(budget);
        let timed_out = || io::Error::from(io::ErrorKind::TimedOut);
        let errors: [SdkFailure<DeleteObjectError>; 2] = [
            SdkFailure::timeout_error(timed_out()),
            SdkFailure::dispatch_failure(ConnectorError::timeout(Box::new(timed_out()))),
        ];
        for error in &errors {
            assert_eq!(
                from_sdk_mutation(error, Unconditional, false, parent_limited.stopped()).error,
                OutcomeUnknown(UnknownOutcome::Unavailable)
            );
        }

        tokio::time::advance(budget).await;

        for error in &errors {
            assert_eq!(
                from_sdk_mutation(error, Unconditional, false, parent_limited.stopped()).error,
                OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Deadline))
            );
            assert_eq!(
                from_sdk_mutation(error, Unconditional, false, locally_limited.stopped()).error,
                OutcomeUnknown(UnknownOutcome::Unavailable)
            );
            assert_eq!(
                from_sdk_mutation(error, Unconditional, true, parent_limited.stopped()).error,
                NotApplied(MutationFailure::Rejected)
            );
        }
    }

    #[test]
    fn read_classification_preserves_absence_and_rejection() {
        use ObjectStorageError as E;
        let status = |status, code| FailureKind::Status { status, code };
        let cases = [
            (FailureKind::Lost, E::Unavailable),
            (status(404, Some("NoSuchKey")), E::NotFound),
            (status(404, Some("NotFound")), E::NotFound),
            (status(404, None), E::NotFound),
            (status(404, Some("NoSuchBucket")), E::Rejected),
            (status(500, Some("InternalError")), E::Unavailable),
            (status(403, Some("AccessDenied")), E::Rejected),
            (status(501, Some("NotImplemented")), E::Rejected),
        ];
        for (reply, expected) in cases {
            assert_eq!(classify_read(reply), expected, "{reply:?}");
        }
    }

    #[test]
    fn errors_display_no_provider_detail() {
        for rendered in [
            ObjectStorageError::NotFound.to_string(),
            OutcomeUnknown(UnknownOutcome::Unavailable).to_string(),
            ObjectStorageError::Integrity.to_string(),
        ] {
            assert!(rendered.starts_with("object"), "{rendered}");
        }
    }
}
