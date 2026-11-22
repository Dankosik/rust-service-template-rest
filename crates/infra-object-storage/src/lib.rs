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

use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::config::{
    BehaviorVersion, Region, RequestChecksumCalculation, ResponseChecksumValidation,
    SharedCredentialsProvider, SharedHttpClient, SharedInterceptor, StalledStreamProtectionConfig,
};
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::operation::get_object::GetObjectOutput;
use aws_sdk_s3::operation::{RequestId, RequestIdExt};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::DateTime;
use aws_sdk_s3::types::{ChecksumAlgorithm, ChecksumMode};
use aws_smithy_http_client::tls;
use operation_context::{BoundedContext, OperationContext};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, SemaphorePermit};

use self::call::{
    RetryPolicy, call_timeout, check_active, read_retry_config, single_attempt_retry_config,
    within, within_mutation,
};
pub use self::content_type::{ContentType, InvalidContentType};
pub use self::credentials::{AccessKey, AmazonCredentials};
pub use self::download::Download;
pub use self::error::{
    ConfigError, MutationError, MutationFailure, ObjectStorageError, UnknownOutcome,
};
use self::error::{ConfigErrorKind, Failure, LocalFailure, MutationMode, PutCallResult};
pub use self::key::{InvalidObjectKey, ObjectKey};
pub use self::observe::HISTOGRAMS;
use self::observe::{Histograms, Operation, OperationGuard};
pub use self::provider::Provider;
use self::put_body::{DispatchedPut, Payload};
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

struct SentPut<'a> {
    result: PutCallResult,
    mismatch: Option<Arc<AtomicBool>>,
    inflight: Option<DispatchedPut<'a>>,
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
    success_limit: SharedInterceptor,
    histograms: Arc<Histograms>,
}

impl Inner {
    fn cloud_region(&self) -> Option<&str> {
        self.amazon_s3
            .as_ref()
            .map(|target| target.cloud_region.as_str())
    }

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
        size: u64,
        content_type: Option<String>,
        last_modified: Option<DateTime>,
        e_tag: Option<String>,
    ) -> Self {
        Self {
            size,
            content_type,
            last_modified: last_modified.and_then(|at| SystemTime::try_from(at).ok()),
            e_tag,
        }
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
        let resolved =
            provider::resolve(&options.provider, &options.bucket).map_err(ConfigError)?;
        if !(1..=Semaphore::MAX_PERMITS).contains(&options.max_concurrency) {
            return Err(ConfigError(ConfigErrorKind::MaxConcurrency));
        }
        let http_client = https_client();
        let credentials = options
            .provider
            .credentials_provider(&resolved.region, &http_client)
            .map_err(ConfigError)?;
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
        let mut base = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::v2026_01_12())
            .credentials_provider(credentials)
            .http_client(http_client)
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
            .retry_config(read_retry_config())
            .timeout_config(call_timeout(
                RetryPolicy::Read,
                operation_timeout,
                operation_timeout,
            ))
            .stalled_stream_protection(
                StalledStreamProtectionConfig::enabled()
                    .grace_period(STALL_GRACE)
                    .build(),
            );
        base.push_interceptor(SharedInterceptor::permanent(ResponseLimit::ErrorBody));
        let (config, amazon_s3) = match resolved.target {
            provider::Target::AmazonS3 {
                expected_bucket_owner,
            } => (
                base.force_path_style(false),
                Some(AmazonS3Target {
                    cloud_region: resolved.region.clone(),
                    expected_bucket_owner,
                }),
            ),
            provider::Target::Endpoint { url, path_style } => {
                (base.endpoint_url(url).force_path_style(path_style), None)
            }
        };
        let config = config.region(Region::new(resolved.region));
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
                success_limit: SharedInterceptor::permanent(ResponseLimit::SuccessBody),
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
        let context = parent.bounded(self.inner.operation_timeout);
        let guard = self.start_borrowed_observation(Operation::Put);
        let PutBody { payload, outcomes } = body;
        let mode = options.mode;
        let sent = match self
            .send_put(&context, key, payload, options, outcomes.as_ref())
            .await
        {
            Ok(sent) => sent,
            Err(failure) => return Err(guard.fail(&failure)),
        };
        let outcome = error::interpret_put_result(&sent.result, mode, sent.mismatch.as_deref());
        let outcome_unknown = matches!(
            outcome,
            Err(Failure {
                error: MutationError::OutcomeUnknown(_),
                ..
            })
        );
        if let Some(inflight) = sent.inflight
            && !outcome_unknown
        {
            inflight.settle();
        }
        guard.finish_with_reply(outcome, sent.result.as_ref().ok().map(|(reply, _)| reply))
    }

    async fn send_put<'a>(
        &self,
        context: &BoundedContext,
        key: &ObjectKey,
        payload: Payload,
        options: PutOptions,
        outcomes: Option<&'a PutOutcomes>,
    ) -> Result<SentPut<'a>, Failure<MutationError>> {
        check_active(context)?;
        if payload.len() > self.inner.max_object_bytes {
            return Err(Failure::too_large());
        }
        let _permit = self.try_admit().ok_or_else(Failure::busy)?;
        let payload = match payload {
            Payload::Streamed {
                len: 0,
                stream,
                mismatch,
            } => within(context, Payload::verify_empty_stream(stream, mismatch))
                .await
                .map_err(|interrupted| Failure::not_applied_stop(interrupted.stopped))?
                .map_err(Failure::from)?,
            payload => payload,
        };
        let checksum = self
            .inner
            .checksum
            .applies_to(&payload)
            .then_some(ChecksumAlgorithm::Crc64Nvme);
        let (len, stream, mismatch) = payload.into_stream();
        let config = self.operation_config_with_success_limit(context, RetryPolicy::SingleAttempt);
        let call_config = if checksum.is_some() {
            config.request_checksum_calculation(RequestChecksumCalculation::WhenSupported)
        } else {
            config
        };
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
            inflight = outcomes.map(PutOutcomes::begin_dispatch);
            request.await
        })
        .await;
        Ok(SentPut {
            result,
            mismatch,
            inflight,
        })
    }

    pub async fn get(
        &self,
        parent: &OperationContext,
        key: &ObjectKey,
    ) -> Result<Download, ObjectStorageError> {
        let context = parent.bounded(self.inner.operation_timeout);
        let guard = self.start_owned_observation(Operation::Get);
        let (reply, permit) = match async {
            check_active(&context)?;
            let permit = self.try_admit_owned().ok_or_else(Failure::busy)?;
            let request = Box::pin(
                self.inner
                    .client
                    .get_object()
                    .bucket(&self.inner.bucket)
                    .key(key.as_str())
                    .checksum_mode(ChecksumMode::Enabled)
                    .set_expected_bucket_owner(self.inner.expected_bucket_owner())
                    .customize()
                    .config_override(self.operation_config(&context, RetryPolicy::Read))
                    .send(),
            );
            let reply = within(&context, request).await?;
            Ok::<_, Failure>((reply, permit))
        }
        .await
        {
            Ok(received) => received,
            Err(failure) => return Err(guard.fail(&failure)),
        };
        let (mut guard, output) = accept_reply(guard, &context, reply)?;
        let size = match validate_download_headers(&output, self.inner.max_object_bytes) {
            Ok(size) => size,
            Err(failure) => return Err(guard.fail_with_reply(&failure, &output)),
        };
        guard.retain_request_ids(&output);
        let metadata = ObjectMetadata::from_response_fields(
            size,
            output.content_type,
            output.last_modified,
            output.e_tag,
        );
        let mut download = Download::open(metadata, output.body, guard, permit, context);
        if size == 0 {
            download.next_chunk().await?;
        }
        Ok(download)
    }

    pub async fn head(
        &self,
        parent: &OperationContext,
        key: &ObjectKey,
    ) -> Result<ObjectMetadata, ObjectStorageError> {
        let context = parent.bounded(self.inner.operation_timeout);
        let guard = self.start_borrowed_observation(Operation::Head);
        let reply = match async {
            check_active(&context)?;
            let _permit = self.try_admit().ok_or_else(Failure::busy)?;
            let request = self
                .inner
                .client
                .head_object()
                .bucket(&self.inner.bucket)
                .key(key.as_str())
                .set_expected_bucket_owner(self.inner.expected_bucket_owner())
                .customize()
                .config_override(
                    self.operation_config_with_success_limit(&context, RetryPolicy::Read),
                )
                .send();
            Ok::<_, Failure>(within(&context, request).await?)
        }
        .await
        {
            Ok(reply) => reply,
            Err(failure) => return Err(guard.fail(&failure)),
        };
        let (guard, output) = accept_reply(guard, &context, reply)?;
        let size = match response_size(output.content_length) {
            Ok(size) => size,
            Err(failure) => return Err(guard.fail_with_reply(&failure, &output)),
        };
        guard.succeed_with_reply(&output);
        Ok(ObjectMetadata::from_response_fields(
            size,
            output.content_type,
            output.last_modified,
            output.e_tag,
        ))
    }

    pub async fn delete(
        &self,
        parent: &OperationContext,
        key: &ObjectKey,
    ) -> Result<(), MutationError> {
        let context = parent.bounded(self.inner.operation_timeout);
        let guard = self.start_borrowed_observation(Operation::Delete);
        let (reply, stopped_on_reply) = match async {
            check_active(&context)?;
            let _permit = self.try_admit().ok_or_else(Failure::busy)?;
            let request = self
                .inner
                .client
                .delete_object()
                .bucket(&self.inner.bucket)
                .key(key.as_str())
                .set_expected_bucket_owner(self.inner.expected_bucket_owner())
                .customize()
                .config_override(
                    self.operation_config_with_success_limit(&context, RetryPolicy::SingleAttempt),
                )
                .send();
            Ok::<_, Failure<MutationError>>(within_mutation(&context, request).await?)
        }
        .await
        {
            Ok(reply) => reply,
            Err(failure) => return Err(guard.fail(&failure)),
        };
        let outcome = error::interpret_delete_result(&reply, stopped_on_reply);
        guard.finish_with_reply(outcome, Some(&reply))
    }

    pub async fn presign_get(
        &self,
        parent: &OperationContext,
        key: &ObjectKey,
        expires_in: Duration,
    ) -> Result<PresignedUrl, ObjectStorageError> {
        let context = parent.bounded(self.inner.operation_timeout);
        let guard = self.start_borrowed_observation(Operation::PresignGet);
        let result = async {
            check_active(&context)?;
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
                .config_override(self.operation_config(&context, RetryPolicy::Read))
                .presigned(config);
            let reply = within(&context, request).await?;
            check_active(&context)?;
            match reply {
                Ok(request) if request.headers().next().is_some() => {
                    Err(LocalFailure::PresignedHeaders.into())
                }
                Ok(request) => Ok(PresignedUrl(SecretString::from(request.uri()))),
                Err(sdk_error) => Err(error::from_sdk(&sdk_error)),
            }
        }
        .await;
        guard.finish(result)
    }

    fn start_borrowed_observation(&self, operation: Operation) -> OperationGuard<&Histograms> {
        OperationGuard::start(
            &*self.inner.histograms,
            operation,
            self.inner.cloud_region(),
        )
    }

    fn start_owned_observation(&self, operation: Operation) -> OperationGuard {
        OperationGuard::start(
            Arc::clone(&self.inner.histograms),
            operation,
            self.inner.cloud_region(),
        )
    }

    fn try_admit(&self) -> Option<SemaphorePermit<'_>> {
        self.inner.admission.try_acquire().ok()
    }

    fn try_admit_owned(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.inner.admission).try_acquire_owned().ok()
    }

    fn operation_config(
        &self,
        context: &BoundedContext,
        policy: RetryPolicy,
    ) -> aws_sdk_s3::config::Builder {
        let config = aws_sdk_s3::Config::builder().timeout_config(call_timeout(
            policy,
            context.remaining(),
            self.inner.operation_timeout,
        ));
        match policy {
            RetryPolicy::Read => config,
            RetryPolicy::SingleAttempt => config.retry_config(single_attempt_retry_config()),
        }
    }

    fn operation_config_with_success_limit(
        &self,
        context: &BoundedContext,
        policy: RetryPolicy,
    ) -> aws_sdk_s3::config::Builder {
        let mut config = self.operation_config(context, policy);
        config.push_interceptor(self.inner.success_limit.clone());
        config
    }
}

fn accept_reply<H, T, E>(
    guard: OperationGuard<H>,
    context: &BoundedContext,
    reply: Result<T, SdkError<E, HttpResponse>>,
) -> Result<(OperationGuard<H>, T), ObjectStorageError>
where
    H: Deref<Target = Histograms>,
    T: RequestId + RequestIdExt,
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    if let Err(failure) = check_active(context) {
        return Err(guard.fail_with_reply(&failure, &reply));
    }
    match reply {
        Ok(output) => Ok((guard, output)),
        Err(sdk_error) => Err(guard.fail_with_reply(&error::from_sdk(&sdk_error), &sdk_error)),
    }
}

fn response_size(content_length: Option<i64>) -> Result<u64, Failure> {
    content_length
        .and_then(|size| u64::try_from(size).ok())
        .ok_or_else(|| LocalFailure::ContentLength.into())
}

fn validate_download_headers(
    output: &GetObjectOutput,
    max_object_bytes: u64,
) -> Result<u64, Failure> {
    if output.content_range.is_some() {
        return Err(LocalFailure::ContentRange.into());
    }
    let size = response_size(output.content_length)?;
    if size > max_object_bytes {
        return Err(Failure::too_large());
    }
    Ok(size)
}

fn https_client() -> SharedHttpClient {
    aws_smithy_http_client::Builder::new()
        .tls_provider(tls::Provider::Rustls(
            tls::rustls_provider::CryptoMode::AwsLc,
        ))
        .per_request_connect_timeout()
        .build_https()
}
