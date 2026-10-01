//! Optional object storage client construction.

use infra_object_storage::{CredentialSource, ObjectStorage, ObjectStorageOptions, Provider};
use service_config::{Config, ObjectStorageCredentials, ObjectStorageProvider};

use super::BootstrapError;

/// Build the optional object storage client. Nothing is sent: the bucket is
/// not a readiness dependency, and a service that cannot serve without it
/// pushes `storage.probe()` into the readiness probes instead.
pub(super) fn open(config: &Config) -> Result<Option<ObjectStorage>, BootstrapError> {
    let settings = &config.object_storage;
    let provider = match settings.provider {
        ObjectStorageProvider::None => return Ok(None),
        ObjectStorageProvider::AmazonS3 => Provider::AmazonS3 {
            region: settings.region.clone(),
            expected_bucket_owner: settings.expected_bucket_owner.clone(),
        },
        ObjectStorageProvider::CloudflareR2 => Provider::CloudflareR2 {
            endpoint: settings.endpoint.clone(),
        },
        ObjectStorageProvider::Railway => Provider::Railway {
            endpoint: settings.endpoint.clone(),
            region: settings.region.clone(),
        },
        ObjectStorageProvider::S3Compatible => Provider::S3Compatible {
            endpoint: settings.endpoint.clone(),
            region: settings.region.clone(),
            path_style: settings.path_style,
        },
        ObjectStorageProvider::Local => Provider::Local {
            endpoint: settings.endpoint.clone(),
            region: settings.region.clone(),
        },
    };
    let (credentials, credentials_label) = match settings.credentials {
        ObjectStorageCredentials::AccessKey => {
            let Some(secret_access_key) = settings.secret_access_key.clone() else {
                return Err(infra_object_storage::ConfigError::SecretAccessKey.into());
            };
            let credentials = CredentialSource::AccessKey {
                access_key_id: settings.access_key_id.clone(),
                secret_access_key,
            };
            (credentials, "access_key")
        }
        ObjectStorageCredentials::WorkloadIdentity => {
            (CredentialSource::WorkloadIdentity, "workload_identity")
        }
    };
    let storage = ObjectStorage::new(ObjectStorageOptions {
        provider,
        bucket: settings.bucket.clone(),
        credentials,
        max_object_bytes: settings.max_object_bytes.as_u64(),
        max_concurrency: usize::try_from(settings.max_concurrency).unwrap_or(usize::MAX),
        operation_timeout: settings.operation_timeout,
    })?;
    tracing::info!(
        object_storage.provider = storage.provider(),
        object_storage.credentials = credentials_label,
        object_storage.max_object_bytes = settings.max_object_bytes.as_u64(),
        object_storage.max_concurrency = settings.max_concurrency,
        "object_storage_configured"
    );
    Ok(Some(storage))
}
