//! Optional S3-compatible object storage client for one bucket at one fixed
//! endpoint.
//!
//! The feature owns keys, authorization, content policy, retention,
//! and whether an operation is create-only. Its provider adapter uses this
//! client and maps storage results into the feature's business interface;
//! the feature does not depend on this crate. The AWS SDK owns signing, the
//! wire protocol, retries, checksums, and presigning; this crate admits the
//! provider tuple, bounds size and concurrency, maps every SDK operation
//! result onto [`ObjectStorageError`], and observes each call without
//! recording keys.
//!
//! Construction performs no I/O and the client is not a readiness
//! dependency. [`ObjectStorage::probe`] is the opt-in bucket check.
//!
//! [`PutBody::stream`] takes any `http_body::Body<Data = Bytes>` whose error
//! converts into a boxed error, and [`Download`] is one, so a request body can
//! be stored and an object returned as a response body without a body
//! conversion. Direct response streaming holds an admission
//! slot at the reader's pace within the fixed operation deadline; use
//! collected bytes or a presigned URL for a reader that may be slow.

mod body;
mod call;
mod content_type;
mod credentials;
mod download;
mod error;
mod key;
mod observe;
mod provider;
mod response_limit;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aws_sdk_s3::config::{
    BehaviorVersion, Region, RequestChecksumCalculation, ResponseChecksumValidation,
    SharedCredentialsProvider, SharedHttpClient, StalledStreamProtectionConfig,
};
use aws_sdk_s3::operation::get_object::GetObjectOutput;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::DateTime;
use aws_sdk_s3::types::{ChecksumAlgorithm, ChecksumMode};
use aws_smithy_http_client::tls;
use operation_context::OperationContext;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub use self::body::PutBody;
use self::call::{call_policy, within};
pub use self::content_type::{ContentType, InvalidContentType};
pub use self::credentials::CredentialSource;
pub use self::download::Download;
use self::error::{Call, Failure, LocalFailure};
pub use self::error::{ConfigError, ObjectStorageError};
pub use self::key::{InvalidObjectKey, ObjectKey};
pub use self::observe::{HISTOGRAMS, OPERATION_DURATION_BUCKETS, OPERATION_DURATION_METRIC};
use self::observe::{Histograms, Operation, OperationGuard};
pub use self::provider::Provider;
use self::response_limit::ResponseLimit;

/// A transfer with no progress for this long fails; the pinned behavior
/// version's default, stated because an explicit config would otherwise
/// take the builder's 20 s.
const STALL_GRACE: Duration = Duration::from_secs(5);
const PRESIGN_MIN: Duration = Duration::from_secs(1);
/// The `SigV4` limit, and R2's; Railway would allow 90 days.
const PRESIGN_MAX: Duration = Duration::from_hours(7 * 24);

/// Construction input. [`Debug`] prints the provider name and limits only.
pub struct ObjectStorageOptions {
    pub provider: Provider,
    /// The S3 bucket name (on Railway the hashed `BUCKET`, not the display name).
    pub bucket: String,
    pub credentials: CredentialSource,
    pub max_object_bytes: u64,
    /// Operations admitted at once; the excess is refused with
    /// [`ObjectStorageError::Busy`]. A download holds its slot until it ends,
    /// is dropped, or its operation stops.
    pub max_concurrency: usize,
    /// Bound for the complete operation, including preparation, retries and body.
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
    provider_name: &'static str,
    cloud_region: Option<String>,
    bucket: String,
    expected_bucket_owner: Option<String>,
    checksum: provider::UploadChecksum,
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
            .field("provider", &self.inner.provider_name)
            .field("max_object_bytes", &self.inner.max_object_bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default)]
pub struct PutOptions {
    content_type: Option<ContentType>,
    create_only: bool,
}

impl PutOptions {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectMetadata {
    /// Byte length from the response `Content-Length`.
    pub size: u64,
    /// The stored `Content-Type` value as the provider returned it; not
    /// validated as a [`ContentType`].
    pub content_type: Option<String>,
    /// The provider's `Last-Modified` time; `None` when it sent none or the
    /// value is not representable as a `SystemTime`.
    pub last_modified: Option<SystemTime>,
    /// The raw `ETag` header value: quote characters and any `-N` part suffix
    /// are kept; it is not a content digest.
    pub e_tag: Option<String>,
}

impl ObjectMetadata {
    fn from_response_fields(
        content_length: Option<i64>,
        content_type: Option<&str>,
        last_modified: Option<&DateTime>,
        e_tag: Option<&str>,
    ) -> Result<Self, Failure> {
        let size = content_length
            .and_then(|size| u64::try_from(size).ok())
            .ok_or_else(|| Failure::from(LocalFailure::ContentLength))?;
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
#[derive(Clone, derive_more::Debug)]
#[debug("PresignedUrl([REDACTED])")]
pub struct PresignedUrl(String);

impl PresignedUrl {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
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
    pub fn try_new(options: ObjectStorageOptions) -> Result<Self, ConfigError> {
        let admitted = provider::admit(&options.provider, &options.bucket)?;
        // None would refuse every call as `Busy`; more would panic the semaphore.
        if !(1..=Semaphore::MAX_PERMITS).contains(&options.max_concurrency) {
            return Err(ConfigError::MaxConcurrency);
        }
        let http_client = https_client();
        let credentials = options
            .credentials
            .credentials_provider(&admitted, &http_client)?;
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
        // Every operation overrides these per call; only the bucket probe
        // runs on the client defaults.
        let (timeout, retry) = call_policy(Call::Read, operation_timeout, operation_timeout);
        let mut config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::v2026_01_12())
            .region(Region::new(admitted.region))
            .credentials_provider(credentials)
            .http_client(http_client)
            .interceptor(ResponseLimit::ErrorBody)
            .force_path_style(admitted.path_style)
            // Send and validate only what a provider has proven; uploads
            // name CRC64NVME where it is accepted (see `UploadChecksum`).
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
            .retry_config(retry)
            .timeout_config(timeout)
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
                provider_name: provider.name(),
                cloud_region: admitted.cloud_region,
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
    pub fn provider_name(&self) -> &'static str {
        self.inner.provider_name
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
        self.put_with_context(key, body, options, &OperationContext::unbounded())
            .await
    }

    /// Store within the caller's original remaining budget and this client's
    /// existing ceiling. A stopped caller sends nothing before dispatch.
    ///
    /// # Errors
    ///
    /// As [`Self::put`]. A known provider answer from a live SDK poll remains
    /// internally final when that poll crosses the cooperative cutoff; the
    /// outer caller remains responsible for its own terminal deadline.
    pub async fn put_with_context(
        &self,
        key: &ObjectKey,
        body: PutBody,
        options: PutOptions,
        parent: &OperationContext,
    ) -> Result<(), ObjectStorageError> {
        let (context, mut guard) = self.begin(parent, Operation::Put)?;
        if body.len > self.inner.max_object_bytes {
            return Err(guard.fail(&LocalFailure::TooLarge.into()));
        }
        let _permit = self.admit(&mut guard)?;
        let body = within(&context, body.normalize_empty_stream())
            .await
            .map_err(|interrupted| guard.fail(&Failure::stopped(interrupted.stopped)))?
            .map_err(|failure| guard.fail(&failure))?;
        let checksum = self
            .inner
            .checksum
            .applies_to(&body.source)
            .then_some(ChecksumAlgorithm::Crc64Nvme);
        let call = if options.create_only {
            Call::CreateOnly
        } else {
            Call::Mutation
        };
        // Under the client's `WhenRequired` the SDK sends the named
        // algorithm but computes no checksum value. An upload that carries
        // CRC64NVME therefore switches this one call to `WhenSupported`.
        let mut call_config = self.operation_config(&context, call);
        if checksum.is_some() {
            call_config =
                call_config.request_checksum_calculation(RequestChecksumCalculation::WhenSupported);
        }
        let request = self
            .inner
            .client
            .put_object()
            .bucket(&self.inner.bucket)
            .key(key.as_str())
            .content_length(i64::try_from(body.len).unwrap_or(i64::MAX))
            .body(body.stream)
            .set_content_type(options.content_type.map(ContentType::into_string))
            .set_checksum_algorithm(checksum)
            .set_if_none_match(options.create_only.then(|| "*".to_owned()))
            .set_expected_bucket_owner(self.inner.expected_bucket_owner.clone())
            .customize()
            .config_override(call_config)
            .interceptor(ResponseLimit::SuccessBody)
            .send();
        let result = within(&context, request).await;
        if let Ok(answer) = &result {
            guard.answered_by(answer);
        }
        match result {
            Ok(Ok(_)) => {
                guard.succeed();
                Ok(())
            }
            // The body broke its declared length: report refused input
            // (`Rejected`), not the transport error or unknown outcome it
            // caused; see [`PutBody::stream`].
            Ok(Err(_)) | Err(_) if body.source.mismatched() => {
                Err(guard.fail(&LocalFailure::UploadLengthMismatch.into()))
            }
            Ok(Err(sdk_error)) => Err(guard.fail(&error::from_sdk(call, &sdk_error))),
            Err(interrupted) => Err(interrupted.fail(&mut guard, call)),
        }
    }

    /// Start a download. The returned [`Download`] holds its admission slot
    /// through confirmed EOF/integrity, failure, drop, cancellation, or its
    /// original operation cutoff; an unpolled retained download releases its
    /// adapter-owned body, permit, and observation at that cutoff.
    ///
    /// # Errors
    ///
    /// `NotFound` when the key is absent; `TooLarge` when the stored object
    /// exceeds `max_object_bytes`; `Integrity` for a range or unsized
    /// response; otherwise `Busy`, `Unavailable`, or `Rejected`.
    pub async fn get(&self, key: &ObjectKey) -> Result<Download, ObjectStorageError> {
        self.get_with_context(key, &OperationContext::unbounded())
            .await
    }

    /// # Errors
    ///
    /// As [`Self::get`]. A stopped context maps to `Unavailable` and cannot
    /// yield a newly obtained body chunk or successful EOF.
    pub async fn get_with_context(
        &self,
        key: &ObjectKey,
        parent: &OperationContext,
    ) -> Result<Download, ObjectStorageError> {
        let call = Call::Read;
        let (context, mut guard) = self.begin(parent, Operation::Get)?;
        let permit = self.admit(&mut guard)?;
        // Keep the SDK's large send future off every GET caller's stack.
        let request = Box::pin(
            self.inner
                .client
                .get_object()
                .bucket(&self.inner.bucket)
                .key(key.as_str())
                .checksum_mode(ChecksumMode::Enabled)
                .set_expected_bucket_owner(self.inner.expected_bucket_owner.clone())
                .customize()
                .config_override(self.operation_config(&context, call))
                .send(),
        );
        let result = within(&context, request)
            .await
            .map_err(|interrupted| interrupted.fail(&mut guard, call))?;
        guard.answered_by(&result);
        // Unlike mutation finality, a still-open GET rejects a late header
        // response before it can transfer body custody.
        Self::check(&context, &mut guard)?;
        // Compute header decisions before committing their mapping, so a
        // stopped context takes precedence over preparation that finishes late.
        let output = match result {
            Ok(output) => output,
            Err(sdk_error) => {
                let failure = error::from_sdk(call, &sdk_error);
                Self::check(&context, &mut guard)?;
                return Err(guard.fail(&failure));
            }
        };
        let metadata = download_metadata(&output, self.inner.max_object_bytes);
        Self::check(&context, &mut guard)?;
        let metadata = metadata.map_err(|failure| guard.fail(&failure))?;
        let mut download = Download::open(metadata, output.body, guard, permit, context);
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
    /// `NotFound` when the key is absent; on a provider whose HEAD reply
    /// carries no error body (Amazon S3 among them), a missing bucket also
    /// reads as `NotFound`. `Integrity` when the response has no size;
    /// otherwise `Busy`, `Unavailable`, or `Rejected`.
    pub async fn head(&self, key: &ObjectKey) -> Result<ObjectMetadata, ObjectStorageError> {
        self.head_with_context(key, &OperationContext::unbounded())
            .await
    }

    /// Read metadata within the caller's original remaining budget and local
    /// ceiling. A stopped context maps to `Unavailable` before dispatch.
    ///
    /// # Errors
    ///
    /// As [`Self::head`].
    pub async fn head_with_context(
        &self,
        key: &ObjectKey,
        parent: &OperationContext,
    ) -> Result<ObjectMetadata, ObjectStorageError> {
        let call = Call::Read;
        let (context, mut guard) = self.begin(parent, Operation::Head)?;
        let _permit = self.admit(&mut guard)?;
        let request = self
            .inner
            .client
            .head_object()
            .bucket(&self.inner.bucket)
            .key(key.as_str())
            .set_expected_bucket_owner(self.inner.expected_bucket_owner.clone())
            .customize()
            .config_override(self.operation_config(&context, call))
            .interceptor(ResponseLimit::SuccessBody)
            .send();
        let result = within(&context, request)
            .await
            .map_err(|interrupted| interrupted.fail(&mut guard, call))?;
        guard.answered_by(&result);
        Self::check(&context, &mut guard)?;
        let output = match result {
            Ok(output) => output,
            Err(sdk_error) => return Err(guard.fail(&error::from_sdk(call, &sdk_error))),
        };
        let metadata = ObjectMetadata::from_response_fields(
            output.content_length(),
            output.content_type(),
            output.last_modified(),
            output.e_tag(),
        )
        .map_err(|failure| guard.fail(&failure))?;
        Self::check(&context, &mut guard)?;
        guard.succeed();
        Ok(metadata)
    }

    /// Delete an object. A missing key is success, as in S3.
    ///
    /// # Errors
    ///
    /// `Busy`, `Unavailable`, `Rejected`, or `OutcomeUnknown`.
    pub async fn delete(&self, key: &ObjectKey) -> Result<(), ObjectStorageError> {
        self.delete_with_context(key, &OperationContext::unbounded())
            .await
    }

    /// # Errors
    ///
    /// As [`Self::delete`]. A stop before the first request poll is `Unavailable`.
    /// A stop that interrupts an already-polled request is `OutcomeUnknown`;
    /// recovery does not grant replay authority. A definitive provider answer
    /// from a poll that began live still applies if that poll crosses the cutoff.
    pub async fn delete_with_context(
        &self,
        key: &ObjectKey,
        parent: &OperationContext,
    ) -> Result<(), ObjectStorageError> {
        let call = Call::Mutation;
        let (context, mut guard) = self.begin(parent, Operation::Delete)?;
        let _permit = self.admit(&mut guard)?;
        let request = self
            .inner
            .client
            .delete_object()
            .bucket(&self.inner.bucket)
            .key(key.as_str())
            .set_expected_bucket_owner(self.inner.expected_bucket_owner.clone())
            .customize()
            .config_override(self.operation_config(&context, call))
            .interceptor(ResponseLimit::SuccessBody)
            .send();
        let result = within(&context, request)
            .await
            .map_err(|interrupted| interrupted.fail(&mut guard, call))?;
        guard.answered_by(&result);
        match result {
            Ok(_) => {
                guard.succeed();
                Ok(())
            }
            // S3 and R2 answer 204; a provider that answers `NoSuchKey` means
            // the same thing.
            Err(sdk_error)
                if aws_sdk_s3::error::ProvideErrorMetadata::code(&sdk_error)
                    == Some("NoSuchKey") =>
            {
                guard.succeed();
                Ok(())
            }
            Err(sdk_error) => Err(guard.fail(&error::from_sdk(call, &sdk_error))),
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
    /// `Rejected` when `expires_in` is out of range or the request cannot be
    /// constructed. `Unavailable` when
    /// [`CredentialSource::WorkloadIdentity`] yields no credentials or its
    /// endpoint fails; nothing reaches the store. The URL also stops working
    /// when those temporary credentials expire, which may be before
    /// `expires_in`.
    pub async fn presign_get(
        &self,
        key: &ObjectKey,
        expires_in: Duration,
    ) -> Result<PresignedUrl, ObjectStorageError> {
        self.presign_get_with_context(key, expires_in, &OperationContext::unbounded())
            .await
    }

    /// Sign within the caller's budget. URL expiry remains an independent
    /// bearer-credential lifetime.
    ///
    /// # Errors
    ///
    /// As [`Self::presign_get`]. A stopped context maps to `Unavailable`.
    pub async fn presign_get_with_context(
        &self,
        key: &ObjectKey,
        expires_in: Duration,
        parent: &OperationContext,
    ) -> Result<PresignedUrl, ObjectStorageError> {
        let call = Call::Read;
        let (context, mut guard) = self.begin(parent, Operation::PresignGet)?;
        if !(PRESIGN_MIN..=PRESIGN_MAX).contains(&expires_in) {
            return Err(guard.fail(&LocalFailure::PresignExpiry.into()));
        }
        // `PRESIGN_MAX` is the SDK's one-week signing limit; the conversion is
        // a backstop for that limit, not a second input decision.
        let config = PresigningConfig::expires_in(expires_in)
            .map_err(|_| guard.fail(&LocalFailure::PresignExpiry.into()))?;
        let request = self
            .inner
            .client
            .get_object()
            .bucket(&self.inner.bucket)
            .key(key.as_str())
            .customize()
            .config_override(self.operation_config(&context, call))
            .presigned(config);
        let result = within(&context, request)
            .await
            .map_err(|interrupted| interrupted.fail(&mut guard, call))?;
        Self::check(&context, &mut guard)?;
        match result {
            // The URL must work alone. The SDK signs headers as headers, not
            // query parameters, so a header here (such as the expected bucket
            // owner, which is therefore not sent) would have to accompany it.
            Ok(request) if request.headers().next().is_some() => {
                Err(guard.fail(&LocalFailure::PresignedHeaders.into()))
            }
            Ok(request) => {
                let url = PresignedUrl(request.uri().to_owned());
                Self::check(&context, &mut guard)?;
                guard.succeed();
                Ok(url)
            }
            Err(sdk_error) => Err(guard.fail(&error::from_sdk(call, &sdk_error))),
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

    fn start_observation(&self, operation: Operation) -> OperationGuard {
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
            .map_err(|_| guard.fail(&LocalFailure::Busy.into()))
    }

    /// Fix the complete operation cutoff before admission, preparation, or
    /// SDK construction. Every later stage spends this same child context.
    fn begin(
        &self,
        parent: &OperationContext,
        operation: Operation,
    ) -> Result<(OperationContext, OperationGuard), ObjectStorageError> {
        let context = parent.child(self.inner.operation_timeout);
        let mut guard = self.start_observation(operation);
        Self::check(&context, &mut guard)?;
        Ok((context, guard))
    }

    fn check(
        context: &OperationContext,
        guard: &mut OperationGuard,
    ) -> Result<(), ObjectStorageError> {
        context
            .check()
            .map_err(|stopped| guard.fail(&Failure::stopped(stopped)))
    }

    fn operation_config(
        &self,
        context: &OperationContext,
        call: Call,
    ) -> aws_sdk_s3::config::Builder {
        let remaining = context.remaining().unwrap_or(self.inner.operation_timeout);
        let (timeout, retry) = call_policy(call, remaining, self.inner.operation_timeout);
        aws_sdk_s3::Config::builder()
            .timeout_config(timeout)
            .retry_config(retry)
    }
}

/// The stored size a get may stream, refused in this order: a range
/// response, a missing or negative size, then a size over `max_object_bytes`.
fn download_metadata(
    output: &GetObjectOutput,
    max_object_bytes: u64,
) -> Result<ObjectMetadata, Failure> {
    if output.content_range().is_some() {
        return Err(LocalFailure::ContentRange.into());
    }
    let metadata = ObjectMetadata::from_response_fields(
        output.content_length(),
        output.content_type(),
        output.last_modified(),
        output.e_tag(),
    )?;
    if metadata.size > max_object_bytes {
        return Err(LocalFailure::TooLarge.into());
    }
    Ok(metadata)
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
/// bounded error type. The probe bypasses the object-operation semaphore.
/// It runs on the client's read policy with the full `operation_timeout`.
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
        let mut guard = self.storage.start_observation(Operation::Probe);
        let result = inner
            .client
            .head_bucket()
            .bucket(&inner.bucket)
            .set_expected_bucket_owner(inner.expected_bucket_owner.clone())
            .customize()
            .interceptor(ResponseLimit::SuccessBody)
            .send()
            .await;
        guard.answered_by(&result);
        match result {
            Ok(_) => {
                guard.succeed();
                Ok(())
            }
            Err(sdk_error) => {
                let failure = error::from_sdk(Call::Read, &sdk_error);
                guard.fail(&failure);
                Err(health::ProbeError::new(format!(
                    "object storage bucket check failed: {}",
                    failure.error_type
                )))
            }
        }
    }
}
