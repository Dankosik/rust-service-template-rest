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
    SharedCredentialsProvider, SharedHttpClient, StalledStreamProtectionConfig,
};
use aws_sdk_s3::operation::get_object::GetObjectOutput;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::DateTime;
use aws_sdk_s3::types::{ChecksumAlgorithm, ChecksumMode};
use aws_smithy_http_client::tls;
use operation_context::OperationContext;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use self::call::{call_policy, within};
pub use self::content_type::{ContentType, InvalidContentType};
pub use self::credentials::CredentialSource;
pub use self::download::Download;
use self::error::{Call, Failure, LocalFailure};
pub use self::error::{ConfigError, ObjectStorageError};
pub use self::key::{InvalidObjectKey, ObjectKey};
pub use self::observe::HISTOGRAMS;
use self::observe::{Histograms, Operation, OperationGuard};
pub use self::provider::Provider;
pub use self::put_body::PutBody;
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

    #[must_use]
    pub fn create_only(mut self) -> Self {
        self.create_only = true;
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
        let admitted = provider::admit(&options.provider, &options.bucket)?;
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
        let (timeout, retry) = call_policy(Call::Read, operation_timeout, operation_timeout);
        let mut config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::v2026_01_12())
            .region(Region::new(admitted.region))
            .credentials_provider(credentials)
            .http_client(http_client)
            .interceptor(ResponseLimit::ErrorBody)
            .force_path_style(admitted.path_style)
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

    #[must_use]
    pub fn provider_name(&self) -> &'static str {
        self.inner.provider_name
    }

    pub async fn put(
        &self,
        key: &ObjectKey,
        body: PutBody,
        options: PutOptions,
    ) -> Result<(), ObjectStorageError> {
        self.put_with_context(key, body, options, &OperationContext::unbounded())
            .await
    }

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
        let result = within(&context, request).await;
        if let Ok(answer) = &result {
            guard.answered_by(answer);
        }
        match result {
            Ok(Ok(_)) => {
                guard.succeed();
                Ok(())
            }
            Ok(Err(_)) | Err(_) if body.source.mismatched() => {
                Err(guard.fail(&LocalFailure::UploadLengthMismatch.into()))
            }
            Ok(Err(sdk_error)) => Err(guard.fail(&error::from_sdk(call, &sdk_error))),
            Err(interrupted) => Err(interrupted.fail(&mut guard, call)),
        }
    }

    pub async fn get(&self, key: &ObjectKey) -> Result<Download, ObjectStorageError> {
        self.get_with_context(key, &OperationContext::unbounded())
            .await
    }

    pub async fn get_with_context(
        &self,
        key: &ObjectKey,
        parent: &OperationContext,
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
        let result = within(&context, request)
            .await
            .map_err(|interrupted| interrupted.fail(&mut guard, call))?;
        guard.answered_by(&result);
        Self::check(&context, &mut guard)?;
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
        if download.metadata().size == 0 {
            download.next_chunk().await?;
        }
        Ok(download)
    }

    pub async fn head(&self, key: &ObjectKey) -> Result<ObjectMetadata, ObjectStorageError> {
        self.head_with_context(key, &OperationContext::unbounded())
            .await
    }

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
        let output = result.map_err(|sdk_error| guard.fail(&error::from_sdk(call, &sdk_error)))?;
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

    pub async fn delete(&self, key: &ObjectKey) -> Result<(), ObjectStorageError> {
        self.delete_with_context(key, &OperationContext::unbounded())
            .await
    }

    pub async fn delete_with_context(
        &self,
        key: &ObjectKey,
        parent: &OperationContext,
    ) -> Result<(), ObjectStorageError> {
        let call = Call::UnconditionalMutation;
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
        error::interpret_delete_result(&result).map_err(|failure| guard.fail(&failure))?;
        guard.succeed();
        Ok(())
    }

    pub async fn presign_get(
        &self,
        key: &ObjectKey,
        expires_in: Duration,
    ) -> Result<PresignedUrl, ObjectStorageError> {
        self.presign_get_with_context(key, expires_in, &OperationContext::unbounded())
            .await
    }

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
                Err(health::ProbeError::format(format_args!(
                    "object storage bucket check failed: {}",
                    failure.error_type
                )))
            }
        }
    }
}
