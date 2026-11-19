use infra_object_storage::{
    ConfigError, CredentialSource, ObjectStorage, ObjectStorageOptions, Provider,
};
use service_config::{ObjectStorageConfig, ObjectStorageCredentials, ObjectStorageProvider};

pub(super) fn open(settings: &ObjectStorageConfig) -> Result<ObjectStorage, ConfigError> {
    let provider = match &settings.provider {
        ObjectStorageProvider::AmazonS3 {
            region,
            expected_bucket_owner,
        } => Provider::AmazonS3 {
            region: region.clone(),
            expected_bucket_owner: expected_bucket_owner.clone(),
        },
        ObjectStorageProvider::CloudflareR2 { endpoint } => Provider::CloudflareR2 {
            endpoint: endpoint.clone(),
        },
        ObjectStorageProvider::Railway { endpoint, region } => Provider::Railway {
            endpoint: endpoint.clone(),
            region: region.clone().unwrap_or_default(),
        },
        ObjectStorageProvider::S3Compatible {
            endpoint,
            region,
            path_style,
        } => Provider::S3Compatible {
            endpoint: endpoint.clone(),
            region: region.clone().unwrap_or_default(),
            path_style: *path_style,
        },
        ObjectStorageProvider::Local { endpoint, region } => Provider::Local {
            endpoint: endpoint.clone(),
            region: region.clone().unwrap_or_default(),
        },
    };
    let credentials = match &settings.credentials {
        ObjectStorageCredentials::AccessKey {
            access_key_id,
            secret_access_key,
        } => CredentialSource::AccessKey {
            access_key_id: access_key_id.clone(),
            secret_access_key: secret_access_key.clone(),
        },
        ObjectStorageCredentials::WorkloadIdentity => CredentialSource::WorkloadIdentity,
    };
    let storage = ObjectStorage::try_new(ObjectStorageOptions {
        provider,
        bucket: settings.bucket.clone(),
        credentials,
        max_object_bytes: settings.max_object_bytes.as_u64(),
        max_concurrency: usize::try_from(settings.max_concurrency).unwrap_or(usize::MAX),
        operation_timeout: settings.operation_timeout,
    })?;
    tracing::info!(
        object_storage.provider = storage.provider_name(),
        object_storage.credentials = settings.credentials.label(),
        object_storage.max_object_bytes = settings.max_object_bytes.as_u64(),
        object_storage.max_concurrency = settings.max_concurrency,
        "object_storage_configured"
    );
    Ok(storage)
}
