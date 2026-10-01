//! Optional S3-compatible object storage client for one bucket at one fixed
//! endpoint.
//!
//! The calling feature owns keys, authorization, content policy, retention,
//! and whether an operation is create-only. The AWS SDK owns signing, the
//! wire protocol, retries, checksums, and presigning; this crate admits the
//! provider tuple, bounds size and concurrency, maps every result onto
//! [`ObjectStorageError`], and observes each call without recording keys.
//!
//! Construction performs no I/O and the client is not a readiness
//! dependency. [`ObjectStorage::probe`] is the opt-in bucket check.
//!
//! [`PutBody::stream`] takes any `http_body::Body`, and [`Download`] is one,
//! so a request body can be stored and an object returned as a response body
//! without an adapter.

mod body;
mod error;
mod key;
mod observe;
mod provider;

#[cfg(test)]
mod tests;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, ready};
use std::time::{Duration, SystemTime};

use aws_config::ecs::EcsCredentialsProvider;
use aws_config::imds::credentials::ImdsCredentialsProvider;
use aws_config::meta::credentials::CredentialsProviderChain;
use aws_config::provider_config::ProviderConfig;
use aws_config::web_identity_token::WebIdentityTokenCredentialsProvider;
use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::config::{
    BehaviorVersion, Credentials, Region, RequestChecksumCalculation, ResponseChecksumValidation,
    SharedCredentialsProvider, SharedHttpClient, StalledStreamProtectionConfig,
};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::{ByteStream, DateTime};
use aws_sdk_s3::types::{ChecksumAlgorithm, ChecksumMode};
use aws_smithy_http_client::tls;
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, timeout_at};

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

/// How the client authenticates. [`Debug`] prints the variant only.
pub enum CredentialSource {
    /// A long-lived access key pair.
    AccessKey {
        /// Access key id.
        access_key_id: String,
        /// Secret access key.
        secret_access_key: SecretString,
    },
    /// The AWS identity the platform gives the workload, refreshed by the
    /// SDK before it expires: a web identity token (EKS IAM roles for service
    /// accounts), the container credentials endpoint (ECS task roles, EKS
    /// Pod Identity), then the EC2 instance profile. [`Provider::AmazonS3`]
    /// only.
    WorkloadIdentity,
}

impl std::fmt::Debug for CredentialSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::AccessKey { .. } => "AccessKey",
            Self::WorkloadIdentity => "WorkloadIdentity",
        })
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

/// An upload body with its exact length.
#[derive(Debug)]
pub struct PutBody {
    len: u64,
    stream: ByteStream,
    source: UploadSource,
}

#[derive(Debug)]
enum UploadSource {
    InMemory,
    Streamed {
        /// Set when the body did not match its declared length.
        mismatch: Arc<AtomicBool>,
    },
}

impl PutBody {
    /// A streamed body that must yield exactly `len` bytes. A body that
    /// yields more or fewer fails the put with
    /// [`ObjectStorageError::Rejected`] instead of storing a truncated object.
    pub fn stream<B, E>(len: u64, body: B) -> Self
    where
        B: http_body::Body<Data = Bytes, Error = E> + Send + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>> + 'static,
    {
        let (body, mismatch) = body::ExactLength::new(len, body);
        Self {
            len,
            stream: ByteStream::from_body_1_x(body),
            source: UploadSource::Streamed { mismatch },
        }
    }

    /// Declared length in bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the body is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// hyper never polls a body declared empty, so validate an empty stream
    /// before sending and normalize its EOF to an in-memory body.
    async fn prepare_empty_stream(
        &mut self,
        deadline: Instant,
    ) -> Result<(), (ObjectStorageError, &'static str)> {
        let UploadSource::Streamed { mismatch } = &self.source else {
            return Ok(());
        };
        if self.len != 0 {
            return Ok(());
        }
        let first = timeout_at(deadline, self.stream.next()).await;
        let length_mismatch = mismatch.load(Ordering::Acquire);
        match first {
            Err(_elapsed) => Err((ObjectStorageError::Unavailable, "timeout")),
            Ok(Some(_)) if length_mismatch => Err((ObjectStorageError::Rejected, "body_length")),
            Ok(Some(_)) => Err((ObjectStorageError::Rejected, "body")),
            Ok(None) => {
                *self = Self::from(Bytes::new());
                Ok(())
            }
        }
    }
}

/// In-memory bytes. The SDK can sign a checksum of them as a header.
impl From<Bytes> for PutBody {
    fn from(bytes: Bytes) -> Self {
        Self {
            len: bytes.len() as u64,
            stream: ByteStream::from(bytes),
            source: UploadSource::InMemory,
        }
    }
}

impl From<Vec<u8>> for PutBody {
    fn from(bytes: Vec<u8>) -> Self {
        Bytes::from(bytes).into()
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
        let http_client = https_client();
        let credentials = match &options.credentials {
            CredentialSource::AccessKey {
                access_key_id,
                secret_access_key,
            } => {
                if access_key_id.trim().is_empty() {
                    return Err(ConfigError::AccessKeyId);
                }
                if secret_access_key.expose_secret().trim().is_empty() {
                    return Err(ConfigError::SecretAccessKey);
                }
                SharedCredentialsProvider::new(Credentials::new(
                    access_key_id,
                    secret_access_key.expose_secret(),
                    None,
                    None,
                    "object_storage",
                ))
            }
            CredentialSource::WorkloadIdentity => {
                if !matches!(options.provider, Provider::AmazonS3 { .. }) {
                    return Err(ConfigError::Credentials);
                }
                SharedCredentialsProvider::new(workload_identity(
                    &admitted.region,
                    http_client.clone(),
                )?)
            }
        };
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
        let output = match self
            .inner
            .client
            .get_object()
            .bucket(&self.inner.bucket)
            .key(key.as_str())
            .checksum_mode(ChecksumMode::Enabled)
            .set_expected_bucket_owner(self.inner.expected_bucket_owner.clone())
            .send()
            .await
        {
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
        Ok(Download {
            remaining: metadata.size,
            metadata,
            body: output.body,
            last: None,
            state: DownloadState::Open(End {
                guard,
                _permit: permit,
            }),
        })
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
        let output = match self
            .inner
            .client
            .head_object()
            .bucket(&self.inner.bucket)
            .key(key.as_str())
            .set_expected_bucket_owner(self.inner.expected_bucket_owner.clone())
            .send()
            .await
        {
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
        OperationGuard::start(Arc::clone(&self.inner.histograms), operation)
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

/// The instance metadata service at its IPv4 link-local address. Left to
/// resolve its endpoint, the SDK's client would read it from an AWS profile
/// file.
const INSTANCE_METADATA_ENDPOINT: &str = "http://169.254.169.254";

/// The workload's AWS identity, in the SDK default chain's order. Unlike that
/// chain it has no environment access keys and no profile files: the
/// providers read only what the platform injects for the identity
/// (`AWS_WEB_IDENTITY_TOKEN_FILE` and `AWS_ROLE_ARN`, the
/// `AWS_CONTAINER_*` variables) and the fixed instance metadata endpoint.
fn workload_identity(
    region: &str,
    http_client: SharedHttpClient,
) -> Result<CredentialsProviderChain, ConfigError> {
    let config = ProviderConfig::without_region()
        .with_region(Some(Region::new(region.to_owned())))
        .with_http_client(http_client);
    let instance_metadata = aws_config::imds::Client::builder()
        .configure(&config)
        .endpoint(INSTANCE_METADATA_ENDPOINT)
        .map_err(|_| ConfigError::Credentials)?
        .build();
    Ok(CredentialsProviderChain::first_try(
        "WebIdentityToken",
        WebIdentityTokenCredentialsProvider::builder()
            .configure(&config)
            .build(),
    )
    .or_else(
        "EcsContainer",
        EcsCredentialsProvider::builder().configure(&config).build(),
    )
    .or_else(
        "Ec2InstanceMetadata",
        ImdsCredentialsProvider::builder()
            .configure(&config)
            .imds_client(instance_metadata)
            .build(),
    ))
}

/// An open download. Dropping it releases the admission slot and the
/// connection; the operation is recorded as `cancelled` unless the body
/// already ended.
///
/// It is an [`http_body::Body`] of exactly [`ObjectMetadata::size`] bytes,
/// so it can be returned as a response body. The chunk that completes the
/// object is released only after the provider's body has ended and its
/// checksum, when one came back, has been validated: a reader that stops at
/// the declared length never holds a complete object that failed the check.
#[derive(Debug)]
pub struct Download {
    metadata: ObjectMetadata,
    remaining: u64,
    body: ByteStream,
    /// The chunk that completed the object, until the body confirms its end.
    last: Option<Bytes>,
    state: DownloadState,
}

#[derive(Debug)]
enum DownloadState {
    Open(End),
    Succeeded,
    /// The failure returned again on every later call.
    Failed(ObjectStorageError),
}

/// What a download releases when it ends.
#[derive(Debug)]
struct End {
    guard: OperationGuard,
    _permit: OwnedSemaphorePermit,
}

impl Download {
    /// Metadata from the response headers.
    #[must_use]
    pub fn metadata(&self) -> &ObjectMetadata {
        &self.metadata
    }

    /// The next chunk, or `None` at the end. The download succeeds only at
    /// the end: the SDK validates a returned full-object checksum there.
    ///
    /// # Errors
    ///
    /// `Integrity` for a checksum mismatch or a body that differs from its
    /// headers; `Unavailable` for a transport failure or a stalled body.
    /// After an error every call returns the same error; after the end,
    /// `Ok(None)`.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, ObjectStorageError> {
        std::future::poll_fn(|context| self.poll_chunk(context)).await
    }

    fn poll_chunk(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Option<Bytes>, ObjectStorageError>> {
        loop {
            let end = match &mut self.state {
                DownloadState::Open(end) => end,
                DownloadState::Succeeded => return Poll::Ready(Ok(self.last.take())),
                DownloadState::Failed(error) => return Poll::Ready(Err(*error)),
            };
            let failure = match ready!(Pin::new(&mut self.body).poll_next(context)) {
                Some(Ok(chunk)) if chunk.is_empty() => continue,
                Some(Ok(chunk)) => match self.remaining.checked_sub(chunk.len() as u64) {
                    Some(0) => {
                        self.remaining = 0;
                        self.last = Some(chunk);
                        continue;
                    }
                    Some(remaining) => {
                        self.remaining = remaining;
                        return Poll::Ready(Ok(Some(chunk)));
                    }
                    None => (ObjectStorageError::Integrity, "content_length"),
                },
                Some(Err(error)) if error::is_checksum_mismatch(&error) => {
                    (ObjectStorageError::Integrity, "checksum")
                }
                Some(Err(_)) => (ObjectStorageError::Unavailable, "body"),
                None if self.remaining == 0 => {
                    end.guard.succeed();
                    self.state = DownloadState::Succeeded;
                    continue;
                }
                None => (ObjectStorageError::Integrity, "content_length"),
            };
            let error = end.guard.fail(failure.0, failure.1);
            self.last = None;
            self.state = DownloadState::Failed(error);
            return Poll::Ready(Err(error));
        }
    }

    /// Consume the download and collect its remaining body. Chunks already
    /// returned by [`Download::next_chunk`] are not included.
    /// `max_object_bytes` bounds the collection.
    ///
    /// # Errors
    ///
    /// As [`Download::next_chunk`].
    pub async fn bytes(mut self) -> Result<Bytes, ObjectStorageError> {
        let mut buffer = Vec::with_capacity(usize::try_from(self.metadata.size).unwrap_or(0));
        while let Some(chunk) = self.next_chunk().await? {
            buffer.extend_from_slice(&chunk);
        }
        Ok(Bytes::from(buffer))
    }
}

impl http_body::Body for Download {
    type Data = Bytes;
    type Error = ObjectStorageError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, ObjectStorageError>>> {
        self.poll_chunk(context)
            .map(|chunk| chunk.map(|chunk| chunk.map(Frame::data)).transpose())
    }

    fn is_end_stream(&self) -> bool {
        matches!(self.state, DownloadState::Succeeded) && self.last.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        let last = self.last.as_ref().map_or(0, |chunk| chunk.len() as u64);
        SizeHint::with_exact(self.remaining + last)
    }
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
        match inner
            .client
            .head_bucket()
            .bucket(&inner.bucket)
            .set_expected_bucket_owner(inner.expected_bucket_owner.clone())
            .send()
            .await
        {
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
