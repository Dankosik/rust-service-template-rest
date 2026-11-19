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
use std::time::{Duration, SystemTime};

use aws_sdk_s3::config::{
    BehaviorVersion, Region, RequestChecksumCalculation, ResponseChecksumValidation,
    SharedHttpClient, StalledStreamProtectionConfig,
};
use aws_sdk_s3::operation::get_object::GetObjectOutput;
use aws_sdk_s3::operation::{RequestId, RequestIdExt};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::DateTime;
use aws_sdk_s3::types::{ChecksumAlgorithm, ChecksumMode};
use aws_smithy_http_client::tls;
use operation_context::OperationContext;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use self::call::{CallContext, Interrupted, call_policy, within, within_mutation};
pub use self::content_type::{ContentType, InvalidContentType};
pub use self::credentials::CredentialSource;
pub use self::download::Download;
use self::error::{Call, Failure, LocalFailure};
pub use self::error::{ConfigError, MutationError, MutationFailure, ObjectStorageError};
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
    #[debug(skip)]
    pub credentials: CredentialSource,
    pub max_object_bytes: u64,
    pub max_concurrency: usize,
    pub operation_timeout: Duration,
}

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

impl std::fmt::Debug for ObjectStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObjectStorage")
            .field("provider", &self.inner.provider_name)
            .field("max_object_bytes", &self.inner.max_object_bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct PutOptions {
    content_type: Option<ContentType>,
    create_only: bool,
}

impl PutOptions {
    #[must_use]
    pub const fn create_only() -> Self {
        Self {
            content_type: None,
            create_only: true,
        }
    }

    #[must_use]
    pub const fn overwrite() -> Self {
        Self {
            content_type: None,
            create_only: false,
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
pub struct PresignedUrl(String);

impl PresignedUrl {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl ObjectStorage {
    pub fn try_new(options: ObjectStorageOptions) -> Result<Self, ConfigError> {
        let ObjectStorageOptions {
            provider,
            bucket,
            credentials,
            max_object_bytes,
            max_concurrency,
            operation_timeout,
        } = options;
        let admitted = provider::admit(&provider, &bucket)?;
        if !(1..=Semaphore::MAX_PERMITS).contains(&max_concurrency) {
            return Err(ConfigError::MaxConcurrency);
        }
        let http_client = https_client();
        let credentials = credentials.credentials_provider(&admitted, &http_client)?;
        let (timeout, retry) = call_policy(Call::Read, operation_timeout, operation_timeout);
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
        let (cloud_region, expected_bucket_owner) = match admitted.target {
            provider::Target::AmazonS3 {
                expected_bucket_owner,
            } => {
                config = config.force_path_style(false);
                (Some(admitted.region.clone()), Some(expected_bucket_owner))
            }
            provider::Target::Endpoint { url, path_style } => {
                config = config.endpoint_url(url).force_path_style(path_style);
                (None, None)
            }
        };
        config = config.region(Region::new(admitted.region));
        observe::describe();
        Ok(Self {
            inner: Arc::new(Inner {
                client: aws_sdk_s3::Client::from_conf(config.build()),
                provider_name: provider.name(),
                cloud_region,
                bucket,
                expected_bucket_owner,
                checksum: admitted.checksum,
                max_object_bytes,
                operation_timeout,
                admission: Arc::new(Semaphore::new(max_concurrency)),
                histograms: Arc::default(),
            }),
        })
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
        let (context, mut guard) = self.begin_mutation(parent, Operation::Put)?;
        if body.len > self.inner.max_object_bytes {
            return Err(guard.fail_mutation(Failure::TOO_LARGE));
        }
        let _permit = self.admit_mutation(&mut guard)?;
        let body = within(&context, body.normalize_empty_stream())
            .await
            .map_err(|interrupted| {
                guard.fail_mutation(Failure::mutation_stopped(
                    interrupted.stopped,
                    MutationError::NotApplied,
                ))
            })?
            .map_err(|failure| guard.fail_mutation(failure))?;
        let checksum = self
            .inner
            .checksum
            .applies_to(&body.source)
            .then_some(ChecksumAlgorithm::Crc64Nvme);
        let (call, if_none_match) = if options.create_only {
            (Call::CreateOnly, Some("*".to_owned()))
        } else {
            (Call::UnconditionalMutation, None)
        };
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
            .set_if_none_match(if_none_match)
            .set_expected_bucket_owner(self.inner.expected_bucket_owner.clone())
            .customize()
            .config_override(call_config)
            .interceptor(ResponseLimit::SuccessBody)
            .send();
        let mut dispatched = None;
        let result = within_mutation(&context, async {
            if let Some(outcomes) = &body.outcomes {
                outcomes.dispatched();
                dispatched = Some(outcomes);
            }
            request.await
        })
        .await;
        if let Ok((answer, _)) = &result {
            guard.answered_by(answer);
        }
        let outcome = match error::interpret_put_result(&result, call, body.source.mismatched()) {
            Ok(()) => {
                guard.succeed();
                Ok(())
            }
            Err(failure) => Err(guard.fail_mutation(failure)),
        };
        if let Some(dispatched) = dispatched
            && !matches!(outcome, Err(MutationError::OutcomeUnknown(_)))
        {
            dispatched.settled();
        }
        outcome
    }

    pub async fn get(
        &self,
        parent: &OperationContext,
        key: &ObjectKey,
    ) -> Result<Download, ObjectStorageError> {
        let call = Call::Read;
        let (context, mut guard) = self.begin(parent, Operation::Get)?;
        let permit = self.admit(&mut guard)?;
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
        let result =
            Self::checked_read_reply(within(&context, request).await, &context, &mut guard)?;
        let output = result.map_err(|sdk_error| guard.fail(error::from_sdk(&sdk_error)))?;
        let metadata = download_metadata(&output, self.inner.max_object_bytes)
            .map_err(|failure| guard.fail(failure))?;
        let mut download = Download::open(metadata, output.body, guard, permit, context);
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
        let result =
            Self::checked_read_reply(within(&context, request).await, &context, &mut guard)?;
        let output = result.map_err(|sdk_error| guard.fail(error::from_sdk(&sdk_error)))?;
        let metadata = ObjectMetadata::from_response_fields(
            output.content_length(),
            output.content_type(),
            output.last_modified(),
            output.e_tag(),
        )
        .map_err(|failure| guard.fail(failure))?;
        guard.succeed();
        Ok(metadata)
    }

    pub async fn delete(
        &self,
        parent: &OperationContext,
        key: &ObjectKey,
    ) -> Result<(), MutationError> {
        let call = Call::UnconditionalMutation;
        let (context, mut guard) = self.begin_mutation(parent, Operation::Delete)?;
        let _permit = self.admit_mutation(&mut guard)?;
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
        let (result, stopped_on_reply) = within_mutation(&context, request)
            .await
            .map_err(|interrupted| guard.fail_mutation(interrupted))?;
        guard.answered_by(&result);
        error::interpret_delete_result(&result, stopped_on_reply)
            .map_err(|failure| guard.fail_mutation(failure))?;
        guard.succeed();
        Ok(())
    }

    pub async fn presign_get(
        &self,
        parent: &OperationContext,
        key: &ObjectKey,
        expires_in: Duration,
    ) -> Result<PresignedUrl, ObjectStorageError> {
        let call = Call::Read;
        let (context, mut guard) = self.begin(parent, Operation::PresignGet)?;
        if !(PRESIGN_MIN..=PRESIGN_MAX).contains(&expires_in) {
            return Err(guard.fail(LocalFailure::PresignExpiry));
        }
        let config = PresigningConfig::expires_in(expires_in)
            .map_err(|_| guard.fail(LocalFailure::PresignExpiry))?;
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
            .map_err(|interrupted| guard.fail(interrupted))?;
        Self::fail_if_stopped(&context, &mut guard)?;
        match result {
            Ok(request) if request.headers().next().is_some() => {
                Err(guard.fail(LocalFailure::PresignedHeaders))
            }
            Ok(request) => {
                let url = PresignedUrl(request.uri().to_owned());
                guard.succeed();
                Ok(url)
            }
            Err(sdk_error) => Err(guard.fail(error::from_sdk(&sdk_error))),
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
            .map_err(|_| guard.fail(LocalFailure::Busy))
    }

    fn admit_mutation(
        &self,
        guard: &mut OperationGuard,
    ) -> Result<OwnedSemaphorePermit, MutationError> {
        Arc::clone(&self.inner.admission)
            .try_acquire_owned()
            .map_err(|_| guard.fail_mutation(Failure::BUSY))
    }

    fn begin(
        &self,
        parent: &OperationContext,
        operation: Operation,
    ) -> Result<(CallContext, OperationGuard), ObjectStorageError> {
        let context = CallContext::new(parent, self.inner.operation_timeout);
        let mut guard = self.start_observation(operation);
        Self::fail_if_stopped(&context, &mut guard)?;
        Ok((context, guard))
    }

    fn begin_mutation(
        &self,
        parent: &OperationContext,
        operation: Operation,
    ) -> Result<(CallContext, OperationGuard), MutationError> {
        let context = CallContext::new(parent, self.inner.operation_timeout);
        let mut guard = self.start_observation(operation);
        if let Some(stopped) = context.stopped() {
            return Err(guard.fail_mutation(Failure::mutation_stopped(
                stopped,
                MutationError::NotApplied,
            )));
        }
        Ok((context, guard))
    }

    fn checked_read_reply<R: RequestId + RequestIdExt>(
        result: Result<R, Interrupted>,
        context: &CallContext,
        guard: &mut OperationGuard,
    ) -> Result<R, ObjectStorageError> {
        let reply = result.map_err(|interrupted| guard.fail(interrupted))?;
        guard.answered_by(&reply);
        Self::fail_if_stopped(context, guard)?;
        Ok(reply)
    }

    fn fail_if_stopped(
        context: &CallContext,
        guard: &mut OperationGuard,
    ) -> Result<(), ObjectStorageError> {
        if let Some(stopped) = context.stopped() {
            return Err(guard.fail(Failure::stopped(stopped)));
        }
        Ok(())
    }

    fn operation_config(&self, context: &CallContext, call: Call) -> aws_sdk_s3::config::Builder {
        let remaining = context.remaining().unwrap_or(self.inner.operation_timeout);
        let (timeout, retry) = call_policy(call, remaining, self.inner.operation_timeout);
        aws_sdk_s3::Config::builder()
            .timeout_config(timeout)
            .retry_config(retry)
    }
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
        return Err(LocalFailure::TooLarge.into());
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
