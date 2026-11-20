use infra_object_storage::{
    AccessKey, AmazonCredentials, ConfigError, ObjectStorage, ObjectStorageOptions, Provider,
};
use service_config::{
    ObjectStorageAccessKey, ObjectStorageAmazonCredentials, ObjectStorageConfig,
    ObjectStorageProvider,
};

fn access_key(credentials: &ObjectStorageAccessKey) -> AccessKey {
    AccessKey {
        access_key_id: credentials.access_key_id.clone(),
        secret_access_key: credentials.secret_access_key.clone(),
    }
}

pub(super) fn open(settings: &ObjectStorageConfig) -> Result<ObjectStorage, ConfigError> {
    let provider = match &settings.provider {
        ObjectStorageProvider::AmazonS3 {
            region,
            expected_bucket_owner,
            credentials,
        } => Provider::AmazonS3 {
            region: region.clone(),
            expected_bucket_owner: expected_bucket_owner.clone(),
            credentials: match credentials {
                ObjectStorageAmazonCredentials::AccessKey(credentials) => {
                    AmazonCredentials::AccessKey(access_key(credentials))
                }
                ObjectStorageAmazonCredentials::WorkloadIdentity => {
                    AmazonCredentials::WorkloadIdentity
                }
            },
        },
        ObjectStorageProvider::CloudflareR2 {
            endpoint,
            credentials,
        } => Provider::CloudflareR2 {
            endpoint: endpoint.clone(),
            credentials: access_key(credentials),
        },
        ObjectStorageProvider::Railway {
            endpoint,
            region,
            credentials,
        } => Provider::Railway {
            endpoint: endpoint.clone(),
            region: region.clone(),
            credentials: access_key(credentials),
        },
        ObjectStorageProvider::S3Compatible {
            endpoint,
            region,
            path_style,
            credentials,
        } => Provider::S3Compatible {
            endpoint: endpoint.clone(),
            region: region.clone(),
            path_style: *path_style,
            credentials: access_key(credentials),
        },
        ObjectStorageProvider::Local {
            endpoint,
            region,
            credentials,
        } => Provider::Local {
            endpoint: endpoint.clone(),
            region: region.clone(),
            credentials: access_key(credentials),
        },
    };
    let storage = ObjectStorage::try_new(ObjectStorageOptions {
        provider,
        bucket: settings.bucket.clone(),
        max_object_bytes: settings.max_object_bytes.as_u64(),
        max_concurrency: usize::try_from(settings.max_concurrency).unwrap_or(usize::MAX),
        operation_timeout: settings.operation_timeout,
    })?;
    tracing::info!(
        object_storage.provider = storage.provider_name(),
        object_storage.credentials = settings.provider.credentials_label(),
        object_storage.max_object_bytes = settings.max_object_bytes.as_u64(),
        object_storage.max_concurrency = settings.max_concurrency,
        "object_storage_configured"
    );
    Ok(storage)
}
