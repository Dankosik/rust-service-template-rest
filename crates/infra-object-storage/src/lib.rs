//! Optional S3-compatible object storage client for one bucket at one fixed
//! endpoint.
//!
//! The feature owns keys, authorization, content policy, retention,
//! and whether an operation is create-only. Its provider adapter uses this
//! client and maps storage results into the feature's business interface;
//! the feature does not depend on this crate. The AWS SDK owns signing, the
//! wire protocol, retries, checksums, and presigning; this crate admits the
//! provider tuple, bounds size and concurrency, maps every result onto
//! [`ObjectStorageError`], and observes each call without recording keys.
//!
//! Construction performs no I/O and the client is not a readiness
//! dependency. [`ObjectStorage::probe`] is the opt-in bucket check.
//!
//! [`PutBody::stream`] takes any `http_body::Body`, and [`Download`] is one,
//! so a request body can be stored and an object returned as a response body
//! without a body conversion. Direct response streaming holds an admission
//! slot at the reader's pace; use collected bytes or a presigned URL for a
//! reader that may be slow.

mod body;
mod credentials;
mod download;
mod error;
mod key;
mod observe;
mod provider;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::config::{
    BehaviorVersion, Region, RequestChecksumCalculation, ResponseChecksumValidation,
    SharedCredentialsProvider, SharedHttpClient, StalledStreamProtectionConfig,
};
use aws_sdk_s3::operation::{RequestId, RequestIdExt};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::DateTime;
use aws_sdk_s3::types::{ChecksumAlgorithm, ChecksumMode};
use aws_smithy_http_client::tls;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, timeout_at};

pub use self::body::PutBody;
use self::body::UploadSource;
pub use self::credentials::CredentialSource;
pub use self::download::Download;
use self::error::Call;
pub use self::error::ObjectStorageError;
pub use self::key::{ContentType, InvalidContentType, InvalidObjectKey, ObjectKey};
use self::observe::{Histograms, Operation, OperationGuard};
pub use self::observe::{OPERATION_DURATION_BUCKETS, OPERATION_DURATION_METRIC};
use self::provider::UploadChecksum;
pub use self::provider::{ConfigError, Provider};

/// Standard retry for reads. A put or delete makes one attempt: the SDK
/// keeps only the last attempt's reply, so after a retry a refusal could
/// hide an earlier attempt that applied the mutation.
const MAX_ATTEMPTS: u32 = 3;
/// Cap on one jittered retry delay, so three attempts fit an interactive
/// `operation_timeout`. The SDK default of 20 s would outlast it.
const MAX_BACKOFF: Duration = Duration::from_secs(1);
/// TCP and TLS connect bound per attempt; the SDK default of the pinned
/// behavior version, stated so an SDK bump cannot move it. The attempt bound
/// caps it when that is shorter.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(3100);
/// A read attempt gets this share of `operation_timeout`, so one attempt
/// that hangs before its response headers leaves room for a retry. The SDK
/// sets no attempt bound itself.
const READ_ATTEMPT_SHARE: u32 = 2;
/// A transfer with no progress for this long fails; the pinned behavior
/// version's default, stated because an explicit config would otherwise
/// take the builder's 20 s.
const STALL_GRACE: Duration = Duration::from_secs(5);
/// Shortest presigned lifetime.
const PRESIGN_MIN: Duration = Duration::from_secs(1);
/// The `SigV4` limit, and R2's; Railway would allow 90 days.
const PRESIGN_MAX: Duration = Duration::from_hours(7 * 24);

/// Construction input. [`Debug`] prints the provider name and limits only.
pub struct ObjectStorageOptions {
    /// Provider and its endpoint, region, and owner fields.
    pub provider: Provider,
    /// The S3 bucket name (on Railway the hashed `BUCKET`, not the display name).
    pub bucket: String,
    /// How the client authenticates.
    pub credentials: CredentialSource,
    /// Largest object a put may send or a get may return.
    pub max_object_bytes: u64,
    /// Operations admitted at once; the excess is refused with
    /// [`ObjectStorageError::Busy`]. A download holds its slot until it ends.
    pub max_concurrency: usize,
    /// Bound for one call up to its response headers, retries included.
    pub operation_timeout: Duration,
}

impl std::fmt::Debug for ObjectStorageOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObjectStorageOptions")
            .field("provider", &self.provider.name())
            .field("max_object_bytes", &self.max_object_bytes)
            .field("max_concurrency", &self.max_concurrency)
            .field("operation_timeout", &self.operation_timeout)
            .finish_non_exhaustive()
    }
}

/// A client for one bucket. Cheap to clone; clones share admission.
#[derive(Clone)]
pub struct ObjectStorage {
    inner: Arc<Inner>,
}

struct Inner {
    client: aws_sdk_s3::Client,
    provider: &'static str,
    /// The span's `cloud.region`: set only for Amazon S3, where the signing
    /// region names a real one. Elsewhere it can be a placeholder (`auto`).
    cloud_region: Option<String>,
    bucket: String,
    expected_bucket_owner: Option<String>,
    checksum: UploadChecksum,
    max_object_bytes: u64,
    operation_timeout: Duration,
    admission: Arc<Semaphore>,
    histograms: Arc<Histograms>,
}

/// The provider and limits only; the bucket and credentials stay out of logs.
impl std::fmt::Debug for ObjectStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObjectStorage")
            .field("provider", &self.inner.provider)
            .field("max_object_bytes", &self.inner.max_object_bytes)
            .finish_non_exhaustive()
    }
}

/// Per-put options.
#[derive(Clone, Debug, Default)]
pub struct PutOptions {
    content_type: Option<ContentType>,
    create_only: bool,
}

impl PutOptions {
    /// Store this `Content-Type` with the object.
    #[must_use]
    pub fn content_type(mut self, content_type: ContentType) -> Self {
        self.content_type = Some(content_type);
        self
    }

    /// Fail with [`ObjectStorageError::AlreadyExists`] instead of replacing an
    /// existing object (`If-None-Match: *`). The put makes exactly one
    /// attempt, so `AlreadyExists` never answers a retry of this call's own
    /// lost success.
    #[must_use]
    pub fn create_only(mut self) -> Self {
        self.create_only = true;
        self
    }
}

/// What the provider reports about a stored object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectMetadata {
    /// Size in bytes.
    pub size: u64,
    /// Stored `Content-Type`, if any.
    pub content_type: Option<String>,
    /// Last modification time, if the provider sent one.
    pub last_modified: Option<SystemTime>,
    /// Entity tag, if the provider sent one.
    pub e_tag: Option<String>,
}

impl ObjectMetadata {
    /// GET and HEAD share one interpretation of the provider's metadata.
    fn from_response_fields(
        content_length: Option<i64>,
        content_type: Option<&str>,
        last_modified: Option<&DateTime>,
        e_tag: Option<&str>,
    ) -> Result<Self, ObjectStorageError> {
        let size = content_length
            .and_then(|size| u64::try_from(size).ok())
            .ok_or(ObjectStorageError::Integrity)?;
        Ok(Self {
            size,
            content_type: content_type.map(str::to_owned),
            last_modified: last_modified.and_then(|at| SystemTime::try_from(*at).ok()),
            e_tag: e_tag.map(str::to_owned),
        })
    }
}

/// A presigned GET URL. It is a bearer credential until it expires: hand it
/// only to its intended recipient and never log it. [`Debug`] redacts it.
#[derive(Clone)]
pub struct PresignedUrl(String);

impl PresignedUrl {
    /// The URL.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PresignedUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PresignedUrl([REDACTED])")
    }
}

impl ObjectStorage {
    /// Admit the provider tuple and build the client. This performs no I/O.
    ///
    /// The SDK config is built directly, so no ambient `AWS_*` variable,
    /// profile file, or proxy variable can change the endpoint, region,
    /// retries, or checksum behavior. Credentials come from
    /// [`CredentialSource`] alone: an access key pair, or for
    /// [`CredentialSource::WorkloadIdentity`] the variables and endpoints the
    /// platform provides for that identity.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] naming the refused key.
    pub fn new(options: ObjectStorageOptions) -> Result<Self, ConfigError> {
        let admitted = provider::admit(&options.provider, &options.bucket)?;
        // None would refuse every call as `Busy`; more would panic the semaphore.
        if !(1..=Semaphore::MAX_PERMITS).contains(&options.max_concurrency) {
            return Err(ConfigError::MaxConcurrency);
        }
        let http_client = https_client();
        let credentials =
            options
                .credentials
                .provider(&options.provider, &admitted.region, &http_client)?;
        Ok(Self::build(options, admitted, http_client, credentials))
    }

    fn build(
        options: ObjectStorageOptions,
        admitted: provider::Admitted,
        http_client: SharedHttpClient,
        credentials: SharedCredentialsProvider,
    ) -> Self {
        let ObjectStorageOptions {
            provider,
            bucket,
            credentials: _,
            max_object_bytes,
            max_concurrency,
            operation_timeout,
        } = options;
        let cloud_region =
            matches!(provider, Provider::AmazonS3 { .. }).then(|| admitted.region.clone());
        let mut config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::v2026_01_12())
            .region(Region::new(admitted.region))
            .credentials_provider(credentials)
            .http_client(http_client)
            .force_path_style(admitted.path_style)
            // Send and validate only what a provider has proven; uploads
            // name CRC64NVME where it is accepted (see `UploadChecksum`).
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
            .retry_config(
                RetryConfig::standard()
                    .with_max_attempts(MAX_ATTEMPTS)
                    .with_max_backoff(MAX_BACKOFF),
            )
            .timeout_config(
                TimeoutConfig::builder()
                    .connect_timeout(CONNECT_TIMEOUT)
                    .operation_timeout(operation_timeout)
                    .operation_attempt_timeout(operation_timeout / READ_ATTEMPT_SHARE)
                    .build(),
            )
            .stalled_stream_protection(
                StalledStreamProtectionConfig::enabled()
                    .grace_period(STALL_GRACE)
                    .build(),
            );
        if let Some(endpoint) = admitted.endpoint {
            config = config.endpoint_url(endpoint);
        }
        observe::describe();
        Self {
            inner: Arc::new(Inner {
                client: aws_sdk_s3::Client::from_conf(config.build()),
                provider: provider.name(),
                cloud_region,
                bucket,
                expected_bucket_owner: admitted.expected_bucket_owner,
                checksum: admitted.checksum,
                max_object_bytes,
                operation_timeout,
                admission: Arc::new(Semaphore::new(max_concurrency)),
                histograms: Arc::default(),
            }),
        }
    }

    /// The configured provider name, safe to log.
    #[must_use]
    pub fn provider(&self) -> &'static str {
        self.inner.provider
    }

    /// Store an object, replacing any existing one unless
    /// [`PutOptions::create_only`] is set.
    ///
    /// # Errors
    ///
    /// [`ObjectStorageError::TooLarge`] and [`ObjectStorageError::Busy`] send
    /// nothing. `AlreadyExists` answers a create-only put. A failure after
    /// sending is `Unavailable`, `Rejected`, or `OutcomeUnknown`.
    pub async fn put(
        &self,
        key: &ObjectKey,
        body: PutBody,
        options: PutOptions,
    ) -> Result<(), ObjectStorageError> {
        // `sleep` saturates a budget too long to add to an instant, as the
        // SDK's own timeout does; `Instant + Duration` would panic.
        let deadline = tokio::time::sleep(self.inner.operation_timeout).deadline();
        let mut guard = self.start(Operation::Put);
        if body.len > self.inner.max_object_bytes {
            return Err(guard.fail(ObjectStorageError::TooLarge, "too_large"));
        }
        let _permit = self.admit(&mut guard)?;
        let mut body = body;
        body.prepare_empty_stream(deadline)
            .await
            .map_err(|(error, error_type)| guard.fail(error, error_type))?;
        let checksum = match (self.inner.checksum, &body.source) {
            (UploadChecksum::Always, _) | (UploadChecksum::BytesOnly, UploadSource::InMemory) => {
                Some(ChecksumAlgorithm::Crc64Nvme)
            }
            (UploadChecksum::BytesOnly, UploadSource::Streamed { .. })
            | (UploadChecksum::Never, _) => None,
        };
        // Under the client's `WhenRequired` the SDK sends the named
        // algorithm but computes no checksum value. An upload that carries
        // CRC64NVME therefore switches this one call to `WhenSupported`.
        let mut once = self.one_attempt();
        if checksum.is_some() {
            once = once.request_checksum_calculation(RequestChecksumCalculation::WhenSupported);
        }
        // A create-only retry could also meet this call's own lost success
        // and report it as `AlreadyExists`.
        let call = if options.create_only {
            Call::CreateOnly
        } else {
            Call::Mutation
        };
        let mismatch = match &body.source {
            UploadSource::InMemory => None,
            UploadSource::Streamed { mismatch } => Some(Arc::clone(mismatch)),
        };
        let request = self
            .inner
            .client
            .put_object()
            .bucket(&self.inner.bucket)
            .key(key.as_str())
            .content_length(i64::try_from(body.len).unwrap_or(i64::MAX))
            .body(body.stream)
            .set_content_type(options.content_type.map(|value| value.0))
            .set_checksum_algorithm(checksum)
            .set_if_none_match(options.create_only.then(|| "*".to_owned()))
            .set_expected_bucket_owner(self.inner.expected_bucket_owner.clone())
            .customize()
            .config_override(once)
            .send();
        // Preparation and dispatch spend one budget. Refuse an expired
        // budget before polling the request, while its outcome is still known.
        if Instant::now() >= deadline {
            return Err(guard.fail(ObjectStorageError::Unavailable, "timeout"));
        }
        let result = timeout_at(deadline, request).await;
        if let Ok(answer) = &result {
            guard.answered_by(answer.request_id(), answer.extended_request_id());
        }
        match result {
            Ok(Ok(_)) => {
                guard.succeed();
                Ok(())
            }
            Ok(Err(_)) | Err(_)
                if mismatch
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::Acquire)) =>
            {
                Err(guard.fail(ObjectStorageError::Rejected, "body_length"))
            }
            Ok(Err(failure)) => Err(Self::fail(&mut guard, call, &failure)),
            Err(_elapsed) => Err(guard.fail(ObjectStorageError::OutcomeUnknown, "timeout")),
        }
    }

    /// Start a download. The returned [`Download`] holds an admission slot
    /// until its body ends or it is dropped; the operation is observed then.
    ///
    /// # Errors
    ///
    /// `NotFound` when the key is absent; `TooLarge` when the stored object
    /// exceeds `max_object_bytes`; `Integrity` for a range or unsized
    /// response; otherwise `Busy`, `Unavailable`, or `Rejected`.
    pub async fn get(&self, key: &ObjectKey) -> Result<Download, ObjectStorageError> {
        let mut guard = self.start(Operation::Get);
        let permit = self.admit(&mut guard)?;
        let result = self
            .inner
            .client
            .get_object()
            .bucket(&self.inner.bucket)
            .key(key.as_str())
            .checksum_mode(ChecksumMode::Enabled)
            .set_expected_bucket_owner(self.inner.expected_bucket_owner.clone())
            .send()
            .await;
        guard.answered_by(result.request_id(), result.extended_request_id());
        let output = match result {
            Ok(output) => output,
            Err(failure) => return Err(Self::fail(&mut guard, Call::Read, &failure)),
        };
        if output.content_range().is_some() {
            return Err(guard.fail(ObjectStorageError::Integrity, "content_range"));
        }
        let metadata = ObjectMetadata::from_response_fields(
            output.content_length(),
            output.content_type(),
            output.last_modified(),
            output.e_tag(),
        )
        .map_err(|error| guard.fail(error, "content_length"))?;
        if metadata.size > self.inner.max_object_bytes {
            return Err(guard.fail(ObjectStorageError::TooLarge, "too_large"));
        }
        let mut download = Download::open(metadata, output.body, guard, permit);
        // hyper never polls a response body declared empty, so nothing would
        // observe an empty object's end: read it here, which records the
        // outcome and releases the slot.
        if download.metadata().size == 0 {
            download.next_chunk().await?;
        }
        Ok(download)
    }

    /// Read an object's metadata. The size is reported even above
    /// `max_object_bytes`; only reading the body is bounded.
    ///
    /// # Errors
    ///
    /// `NotFound` when the key is absent; `Integrity` when the response has
    /// no size; otherwise `Busy`, `Unavailable`, or `Rejected`.
    pub async fn head(&self, key: &ObjectKey) -> Result<ObjectMetadata, ObjectStorageError> {
        let mut guard = self.start(Operation::Head);
        let _permit = self.admit(&mut guard)?;
        let result = self
            .inner
            .client
            .head_object()
            .bucket(&self.inner.bucket)
            .key(key.as_str())
            .set_expected_bucket_owner(self.inner.expected_bucket_owner.clone())
            .send()
            .await;
        guard.answered_by(result.request_id(), result.extended_request_id());
        let output = match result {
            Ok(output) => output,
            Err(failure) => return Err(Self::fail(&mut guard, Call::Read, &failure)),
        };
        let metadata = ObjectMetadata::from_response_fields(
            output.content_length(),
            output.content_type(),
            output.last_modified(),
            output.e_tag(),
        )
        .map_err(|error| guard.fail(error, "content_length"))?;
        guard.succeed();
        Ok(metadata)
    }

    /// Delete an object. A missing key is success, as in S3.
    ///
    /// # Errors
    ///
    /// `Busy`, `Unavailable`, `Rejected`, or `OutcomeUnknown`.
    pub async fn delete(&self, key: &ObjectKey) -> Result<(), ObjectStorageError> {
        let mut guard = self.start(Operation::Delete);
        let _permit = self.admit(&mut guard)?;
        let result = self
            .inner
            .client
            .delete_object()
            .bucket(&self.inner.bucket)
            .key(key.as_str())
            .set_expected_bucket_owner(self.inner.expected_bucket_owner.clone())
            .customize()
            .config_override(self.one_attempt())
            .send()
            .await;
        guard.answered_by(result.request_id(), result.extended_request_id());
        match result {
            Ok(_) => {
                guard.succeed();
                Ok(())
            }
            // S3 and R2 answer 204; a provider that answers `NoSuchKey` means
            // the same thing.
            Err(failure)
                if aws_sdk_s3::error::ProvideErrorMetadata::code(&failure) == Some("NoSuchKey") =>
            {
                guard.succeed();
                Ok(())
            }
            Err(failure) => Err(Self::fail(&mut guard, Call::Mutation, &failure)),
        }
    }

    /// Presign a GET for `expires_in`, between 1 second and 7 days. This is a
    /// local signature: nothing is sent to the store, and no admission slot
    /// is used. Under [`CredentialSource::WorkloadIdentity`] it may first
    /// load the credentials. The
    /// URL works without extra headers, so it carries no expected bucket
    /// owner even on Amazon S3.
    ///
    /// # Errors
    ///
    /// `Rejected` when `expires_in` is out of range or signing fails.
    pub async fn presign_get(
        &self,
        key: &ObjectKey,
        expires_in: Duration,
    ) -> Result<PresignedUrl, ObjectStorageError> {
        let mut guard = self.start(Operation::PresignGet);
        if !(PRESIGN_MIN..=PRESIGN_MAX).contains(&expires_in) {
            return Err(guard.fail(ObjectStorageError::Rejected, "expires_in"));
        }
        let Ok(config) = PresigningConfig::expires_in(expires_in) else {
            return Err(guard.fail(ObjectStorageError::Rejected, "expires_in"));
        };
        match self
            .inner
            .client
            .get_object()
            .bucket(&self.inner.bucket)
            .key(key.as_str())
            .presigned(config)
            .await
        {
            // The URL must work alone. The SDK signs headers as headers, not
            // query parameters, so a header here (such as the expected bucket
            // owner, which is therefore not sent) would have to accompany it.
            Ok(request) if request.headers().next().is_some() => {
                Err(guard.fail(ObjectStorageError::Rejected, "presigned_headers"))
            }
            Ok(request) => {
                guard.succeed();
                Ok(PresignedUrl(request.uri().to_owned()))
            }
            Err(failure) => Err(Self::fail(&mut guard, Call::Read, &failure)),
        }
    }

    /// Readiness probe (`HeadBucket`). Not registered by default: a service
    /// registers it only when it cannot serve without storage.
    #[must_use]
    pub fn probe(&self) -> BucketProbe {
        BucketProbe {
            storage: self.clone(),
        }
    }

    fn start(&self, operation: Operation) -> OperationGuard {
        OperationGuard::start(
            Arc::clone(&self.inner.histograms),
            operation,
            self.inner.cloud_region.as_deref(),
        )
    }

    fn admit(
        &self,
        guard: &mut OperationGuard,
    ) -> Result<OwnedSemaphorePermit, ObjectStorageError> {
        Arc::clone(&self.inner.admission)
            .try_acquire_owned()
            .map_err(|_| guard.fail(ObjectStorageError::Busy, "busy"))
    }

    /// One attempt for a mutation, as a per-call override of the client's
    /// retry. The single attempt takes the whole `operation_timeout`.
    fn one_attempt(&self) -> aws_sdk_s3::config::Builder {
        aws_sdk_s3::Config::builder()
            .retry_config(RetryConfig::standard().with_max_attempts(1))
            .timeout_config(
                TimeoutConfig::builder()
                    .operation_attempt_timeout(self.inner.operation_timeout)
                    .build(),
            )
    }

    fn fail<E: aws_sdk_s3::error::ProvideErrorMetadata + std::error::Error + 'static>(
        guard: &mut OperationGuard,
        call: Call,
        failure: &aws_sdk_s3::error::SdkError<E, HttpResponse>,
    ) -> ObjectStorageError {
        let failure = error::from_sdk(call, failure, |response| response.status().as_u16());
        guard.fail(failure.error, &failure.error_type)
    }
}

/// rustls with aws-lc-rs. An explicit client reads no proxy variable (the
/// pinned behavior version's default client would), and hyper follows no
/// redirect, so a signed request reaches only its origin.
fn https_client() -> SharedHttpClient {
    aws_smithy_http_client::Builder::new()
        .tls_provider(tls::Provider::Rustls(
            tls::rustls_provider::CryptoMode::AwsLc,
        ))
        .build_https()
}

/// `HeadBucket` probe named `object_storage`. Failure text carries only the
/// bounded error type. It bypasses admission, so load never fails readiness.
#[derive(Clone, Debug)]
pub struct BucketProbe {
    storage: ObjectStorage,
}

#[async_trait::async_trait]
impl health::Probe for BucketProbe {
    fn name(&self) -> &'static str {
        "object_storage"
    }

    async fn check(&self) -> Result<(), health::ProbeError> {
        let inner = &self.storage.inner;
        let mut guard = self.storage.start(Operation::Probe);
        let result = inner
            .client
            .head_bucket()
            .bucket(&inner.bucket)
            .set_expected_bucket_owner(inner.expected_bucket_owner.clone())
            .send()
            .await;
        guard.answered_by(result.request_id(), result.extended_request_id());
        match result {
            Ok(_) => {
                guard.succeed();
                Ok(())
            }
            Err(failure) => {
                let failure =
                    error::from_sdk(Call::Read, &failure, |response| response.status().as_u16());
                guard.fail(failure.error, &failure.error_type);
                Err(health::ProbeError::new(format!(
                    "object storage bucket check failed: {}",
                    failure.error_type
                )))
            }
        }
    }
}
