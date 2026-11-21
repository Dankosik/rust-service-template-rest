mod byte_progress;
mod call;
mod content_type;
mod credentials;
mod download;
mod error;
mod key;
mod observe;
mod provider;
mod put_body;
mod response_limit;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use aws_sdk_s3::config::{
    BehaviorVersion, Region, RequestChecksumCalculation, ResponseChecksumValidation,
    SharedCredentialsProvider, SharedHttpClient, StalledStreamProtectionConfig,
};
use aws_sdk_s3::operation::get_object::GetObjectOutput;
use aws_sdk_s3::operation::{RequestId, RequestIdExt};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::DateTime;
use aws_sdk_s3::types::{ChecksumAlgorithm, ChecksumMode};
use aws_smithy_http_client::tls;
use operation_context::{BoundedContext, OperationContext};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use self::call::{Interrupted, RetryPolicy, call_policy, check_active, within, within_mutation};
pub use self::content_type::{ContentType, InvalidContentType};
pub use self::credentials::{AccessKey, AmazonCredentials};
pub use self::download::Download;
pub use self::error::{
    ConfigError, MutationError, MutationFailure, ObjectStorageError, UnknownOutcome,
};
use self::error::{Failure, LocalFailure, MutationMode, OutcomeError};
pub use self::key::{InvalidObjectKey, ObjectKey};
pub use self::observe::HISTOGRAMS;
use self::observe::{Histograms, Operation, OperationGuard};
pub use self::provider::Provider;
pub use self::put_body::{PutBody, PutOutcomes};
use self::response_limit::ResponseLimit;

const STALL_GRACE: Duration = Duration::from_secs(5);
const PRESIGN_MIN: Duration = Duration::from_secs(1);
const PRESIGN_MAX: Duration = Duration::from_hours(7 * 24);

#[derive(derive_more::Debug)]
pub struct ObjectStorageOptions {
    #[debug("{:?}", provider.name())]
    pub provider: Provider,
    #[debug(skip)]
    pub bucket: String,
    pub max_object_bytes: u64,
    pub max_concurrency: usize,
    pub operation_timeout: Duration,
}

#[derive(Clone, derive_more::Debug)]
#[debug(
    "ObjectStorage {{ provider: {:?}, max_object_bytes: {}, .. }}",
    inner.provider_name,
    inner.max_object_bytes
)]
pub struct ObjectStorage {
    inner: Arc<Inner>,
}

struct AmazonS3Target {
    cloud_region: String,
    expected_bucket_owner: String,
}

struct Inner {
    client: aws_sdk_s3::Client,
    provider_name: &'static str,
    amazon_s3: Option<AmazonS3Target>,
    bucket: String,
    checksum: provider::UploadChecksum,
    max_object_bytes: u64,
    operation_timeout: Duration,
    admission: Arc<Semaphore>,
    histograms: Arc<Histograms>,
}

impl Inner {
    fn expected_bucket_owner(&self) -> Option<String> {
        self.amazon_s3
            .as_ref()
            .map(|target| target.expected_bucket_owner.clone())
    }
}

#[derive(Clone, Debug)]
pub struct PutOptions {
    content_type: Option<ContentType>,
    mode: MutationMode,
}

impl PutOptions {
    #[must_use]
    pub const fn create_only() -> Self {
        Self {
            content_type: None,
            mode: MutationMode::CreateOnly,
        }
    }

    #[must_use]
    pub const fn overwrite() -> Self {
        Self {
            content_type: None,
            mode: MutationMode::Unconditional,
        }
    }

    #[must_use]
    pub fn content_type(mut self, content_type: ContentType) -> Self {
        self.content_type = Some(content_type);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectMetadata {
    pub size: u64,
    pub content_type: Option<String>,
    pub last_modified: Option<SystemTime>,
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

#[derive(Clone, derive_more::Debug)]
#[debug("PresignedUrl([REDACTED])")]
pub struct PresignedUrl(SecretString);

impl PresignedUrl {
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl ObjectStorage {
    pub fn try_new(options: ObjectStorageOptions) -> Result<Self, ConfigError> {
        let resolved = provider::resolve(&options.provider, &options.bucket)?;
        if !(1..=Semaphore::MAX_PERMITS).contains(&options.max_concurrency) {
            return Err(ConfigError::MaxConcurrency);
        }
        let http_client = https_client();
        let credentials = options
            .provider
            .credentials_provider(&resolved.region, &http_client)?;
        Ok(Self::build(options, resolved, http_client, credentials))
    }

    fn build(
        options: ObjectStorageOptions,
        resolved: provider::ResolvedProvider,
        http_client: SharedHttpClient,
        credentials: SharedCredentialsProvider,
    ) -> Self {
        let ObjectStorageOptions {
            provider,
            bucket,
            max_object_bytes,
            max_concurrency,
            operation_timeout,
        } = options;
        let (timeout, retry) = call_policy(RetryPolicy::Read, operation_timeout, operation_timeout);
        let mut config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::v2026_01_12())
            .credentials_provider(credentials)
            .http_client(http_client)
            .interceptor(ResponseLimit::ErrorBody)
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
            .retry_config(retry)
            .timeout_config(timeout)
            .stalled_stream_protection(
                StalledStreamProtectionConfig::enabled()
                    .grace_period(STALL_GRACE)
                    .build(),
            );
        let amazon_s3 = match resolved.target {
            provider::Target::AmazonS3 {
                expected_bucket_owner,
            } => {
                config = config.force_path_style(false);
                Some(AmazonS3Target {
                    cloud_region: resolved.region.clone(),
                    expected_bucket_owner,
                })
            }
            provider::Target::Endpoint { url, path_style } => {
                config = config.endpoint_url(url).force_path_style(path_style);
                None
            }
        };
        config = config.region(Region::new(resolved.region));
        observe::describe();
        Self {
            inner: Arc::new(Inner {
                client: aws_sdk_s3::Client::from_conf(config.build()),
                provider_name: provider.name(),
                amazon_s3,
                bucket,
                checksum: resolved.checksum,
                max_object_bytes,
                operation_timeout,
                admission: Arc::new(Semaphore::new(max_concurrency)),
                histograms: Arc::default(),
            }),
        }
    }

    #[must_use]
    pub fn provider_name(&self) -> &'static str {
        self.inner.provider_name
    }

    pub async fn put(
        &self,
        parent: &OperationContext,
        key: &ObjectKey,
        body: PutBody,
        options: PutOptions,
    ) -> Result<(), MutationError> {
        self.observe(parent, Operation::Put, async |context, guard| {
            check_active(context)?;
            if body.len() > self.inner.max_object_bytes {
                return Err(Failure::too_large());
            }
            let _permit = self.try_admit().ok_or_else(Failure::busy)?;
            let PutBody { payload, outcomes } = body;
            let payload = within(context, payload.normalize_empty_stream())
                .await
                .map_err(|interrupted| Failure::not_applied_stop(interrupted.stopped))??;
            let checksum = self
                .inner
                .checksum
                .applies_to(&payload)
                .then_some(ChecksumAlgorithm::Crc64Nvme);
            let (len, stream, mismatch) = payload.into_stream();
            let mut call_config =
                self.operation_config(context, RetryPolicy::SingleAttempt, SuccessBody::Bounded);
            if checksum.is_some() {
                call_config = call_config
                    .request_checksum_calculation(RequestChecksumCalculation::WhenSupported);
            }
            let request = self
                .inner
                .client
                .put_object()
                .bucket(&self.inner.bucket)
                .key(key.as_str())
                .content_length(i64::try_from(len).unwrap_or(i64::MAX))
                .body(stream)
                .set_content_type(options.content_type.map(ContentType::into_string))
                .set_checksum_algorithm(checksum)
                .set_if_none_match(options.mode.if_none_match())
                .set_expected_bucket_owner(self.inner.expected_bucket_owner())
                .customize()
                .config_override(call_config)
                .send();
            let mut inflight = None;
            let result = within_mutation(context, async {
                inflight = outcomes.as_ref().map(PutOutcomes::begin_dispatch);
                request.await
            })
            .await;
            if let Ok((answer, _)) = &result {
                guard.record_request_ids(answer);
            }
            let mismatched = mismatch.is_some_and(|flag| flag.load(Ordering::Acquire));
            let outcome = error::interpret_put_result(&result, options.mode, mismatched);
            if !matches!(
                outcome,
                Err(Failure {
                    error: MutationError::OutcomeUnknown(_),
                    ..
                })
            ) && let Some(inflight) = inflight
            {
                inflight.settle();
            }
            outcome
        })
        .await
    }

    pub async fn get(
        &self,
        parent: &OperationContext,
        key: &ObjectKey,
    ) -> Result<Download, ObjectStorageError> {
        let context = parent.bounded(self.inner.operation_timeout);
        let mut guard = self.start_observation(Operation::Get);
        let opened = async {
            check_active(&context)?;
            let permit = self.try_admit().ok_or_else(Failure::busy)?;
            let request = Box::pin(
                self.inner
                    .client
                    .get_object()
                    .bucket(&self.inner.bucket)
                    .key(key.as_str())
                    .checksum_mode(ChecksumMode::Enabled)
                    .set_expected_bucket_owner(self.inner.expected_bucket_owner())
                    .customize()
                    .config_override(self.operation_config(
                        &context,
                        RetryPolicy::Read,
                        SuccessBody::Streamed,
                    ))
                    .send(),
            );
            let reply =
                Self::checked_read_reply(within(&context, request).await, &context, &mut guard)?;
            let output = reply.map_err(|sdk_error| error::from_sdk(&sdk_error))?;
            let metadata = download_metadata(&output, self.inner.max_object_bytes)?;
            Ok((metadata, output.body, permit))
        }
        .await;
        let (metadata, body, permit) = match opened {
            Ok(opened) => opened,
            Err(failure) => return Err(guard.fail(&failure)),
        };
        let mut download = Download::open(metadata, body, guard, permit, context);
        if download.metadata().size == 0 {
            download.next_chunk().await?;
        }
        Ok(download)
    }

    pub async fn head(
        &self,
        parent: &OperationContext,
        key: &ObjectKey,
    ) -> Result<ObjectMetadata, ObjectStorageError> {
        self.observe(parent, Operation::Head, async |context, guard| {
            check_active(context)?;
            let _permit = self.try_admit().ok_or_else(Failure::busy)?;
            let request = self
                .inner
                .client
                .head_object()
                .bucket(&self.inner.bucket)
                .key(key.as_str())
                .set_expected_bucket_owner(self.inner.expected_bucket_owner())
                .customize()
                .config_override(self.operation_config(
                    context,
                    RetryPolicy::Read,
                    SuccessBody::Bounded,
                ))
                .send();
            let reply = Self::checked_read_reply(within(context, request).await, context, guard)?;
            let output = reply.map_err(|sdk_error| error::from_sdk(&sdk_error))?;
            ObjectMetadata::from_response_fields(
                output.content_length(),
                output.content_type(),
                output.last_modified(),
                output.e_tag(),
            )
        })
        .await
    }

    pub async fn delete(
        &self,
        parent: &OperationContext,
        key: &ObjectKey,
    ) -> Result<(), MutationError> {
        self.observe(parent, Operation::Delete, async |context, guard| {
            check_active(context)?;
            let _permit = self.try_admit().ok_or_else(Failure::busy)?;
            let request = self
                .inner
                .client
                .delete_object()
                .bucket(&self.inner.bucket)
                .key(key.as_str())
                .set_expected_bucket_owner(self.inner.expected_bucket_owner())
                .customize()
                .config_override(self.operation_config(
                    context,
                    RetryPolicy::SingleAttempt,
                    SuccessBody::Bounded,
                ))
                .send();
            let (reply, stopped_on_reply) = within_mutation(context, request).await?;
            guard.record_request_ids(&reply);
            error::interpret_delete_result(&reply, stopped_on_reply)
        })
        .await
    }

    pub async fn presign_get(
        &self,
        parent: &OperationContext,
        key: &ObjectKey,
        expires_in: Duration,
    ) -> Result<PresignedUrl, ObjectStorageError> {
        self.observe(parent, Operation::PresignGet, async |context, _| {
            check_active(context)?;
            if !(PRESIGN_MIN..=PRESIGN_MAX).contains(&expires_in) {
                return Err(LocalFailure::PresignExpiry.into());
            }
            let config = PresigningConfig::expires_in(expires_in)
                .map_err(|_| LocalFailure::PresignExpiry)?;
            let request = self
                .inner
                .client
                .get_object()
                .bucket(&self.inner.bucket)
                .key(key.as_str())
                .customize()
                .config_override(self.operation_config(
                    context,
                    RetryPolicy::Read,
                    SuccessBody::Bounded,
                ))
                .presigned(config);
            let reply = within(context, request).await?;
            check_active(context)?;
            match reply {
                Ok(request) if request.headers().next().is_some() => {
                    Err(LocalFailure::PresignedHeaders.into())
                }
                Ok(request) => Ok(PresignedUrl(SecretString::from(request.uri()))),
                Err(sdk_error) => Err(error::from_sdk(&sdk_error)),
            }
        })
        .await
    }

    async fn observe<T, E: OutcomeError>(
        &self,
        parent: &OperationContext,
        operation: Operation,
        body: impl AsyncFnOnce(&BoundedContext, &mut OperationGuard) -> Result<T, Failure<E>>,
    ) -> Result<T, E> {
        let context = parent.bounded(self.inner.operation_timeout);
        let mut guard = self.start_observation(operation);
        let result = body(&context, &mut guard).await;
        guard.finish(result)
    }

    fn start_observation(&self, operation: Operation) -> OperationGuard {
        OperationGuard::start(
            Arc::clone(&self.inner.histograms),
            operation,
            self.inner
                .amazon_s3
                .as_ref()
                .map(|target| target.cloud_region.as_str()),
        )
    }

    fn try_admit(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.inner.admission).try_acquire_owned().ok()
    }

    fn checked_read_reply<R: RequestId + RequestIdExt>(
        result: Result<R, Interrupted>,
        context: &BoundedContext,
        guard: &mut OperationGuard,
    ) -> Result<R, Failure> {
        let reply = result?;
        guard.record_request_ids(&reply);
        check_active(context)?;
        Ok(reply)
    }

    fn operation_config(
        &self,
        context: &BoundedContext,
        policy: RetryPolicy,
        success_body: SuccessBody,
    ) -> aws_sdk_s3::config::Builder {
        let remaining = context.remaining();
        let (timeout, retry) = call_policy(policy, remaining, self.inner.operation_timeout);
        let config = aws_sdk_s3::Config::builder()
            .timeout_config(timeout)
            .retry_config(retry);
        match success_body {
            SuccessBody::Bounded => config.interceptor(ResponseLimit::SuccessBody),
            SuccessBody::Streamed => config,
        }
    }
}

#[derive(Clone, Copy)]
enum SuccessBody {
    Bounded,
    Streamed,
}

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
        return Err(Failure::too_large());
    }
    Ok(metadata)
}

fn https_client() -> SharedHttpClient {
    aws_smithy_http_client::Builder::new()
        .tls_provider(tls::Provider::Rustls(
            tls::rustls_provider::CryptoMode::AwsLc,
        ))
        .build_https()
}
