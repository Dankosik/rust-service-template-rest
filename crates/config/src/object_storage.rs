//! Optional S3-compatible object storage: provider, bucket, credentials, and
//! limits.
//!
//! The section is inert while `provider` is `none`. It owns which keys a
//! provider and a credential source accept, the environment-only secret, the
//! local-only emulator provider, and the ranges. The shape of each accepted value (endpoint
//! origin, region, bucket name, owner account) is admitted by
//! `infra-object-storage`, the crate that builds the client from it.

use std::time::Duration;

use bytesize::ByteSize;
use secrecy::SecretString;
use serde::Deserialize;

use crate::app::local_development_only;
use crate::de::blank_secret_as_none;
use crate::validate::{ValidationError, duration_range, int_range, non_blank, required};

/// Largest object without multipart: R2's single-upload limit (5 GiB minus
/// 5 MiB), the smallest documented across the supported providers.
const MAX_OBJECT_BYTES_CEILING: u64 = 5 * 1024 * 1024 * 1024 - 5 * 1024 * 1024;

/// Where the bucket lives.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectStorageProvider {
    /// The profile is inert.
    #[default]
    None,
    /// Amazon S3: `region` and `expected_bucket_owner`, no endpoint.
    AmazonS3,
    /// Cloudflare R2: `endpoint` only; the region is always `auto`.
    CloudflareR2,
    /// Railway Buckets: `endpoint` and optional `region` from the bucket.
    Railway,
    /// Any other S3-compatible store: an HTTPS `endpoint`, optional `region`
    /// and `path_style`.
    S3Compatible,
    /// An S3 emulator; `app.env` must be `local` or `development`.
    Local,
}

/// How the client authenticates.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectStorageCredentials {
    /// `access_key_id` and `secret_access_key`.
    #[default]
    AccessKey,
    /// The AWS identity the platform gives the workload. `amazon_s3` only;
    /// the access key pair must be empty.
    WorkloadIdentity,
}

/// Optional object storage for one bucket at one fixed endpoint.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ObjectStorageConfig {
    /// `none` (default), `amazon_s3`, `cloudflare_r2`, `railway`,
    /// `s3_compatible`, or `local`.
    pub provider: ObjectStorageProvider,
    /// S3 bucket name. On Railway, the bucket's `BUCKET` variable.
    pub bucket: String,
    /// Signing region. Required for `amazon_s3`; `auto` or empty for
    /// `cloudflare_r2`; optional for `railway`, `s3_compatible`, and `local`.
    pub region: String,
    /// Endpoint origin. Empty for `amazon_s3`; required otherwise.
    pub endpoint: String,
    /// Amazon account id that must own the bucket. `amazon_s3` only.
    pub expected_bucket_owner: String,
    /// Path-style addressing. `s3_compatible` only.
    pub path_style: bool,
    /// `access_key` (default) or, for `amazon_s3`, `workload_identity`.
    pub credentials: ObjectStorageCredentials,
    /// Access key id. Empty under `workload_identity`.
    pub access_key_id: String,
    /// Secret access key. Environment only
    /// (`APP__OBJECT_STORAGE__SECRET_ACCESS_KEY`); blank is absent. Absent
    /// under `workload_identity`.
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    pub secret_access_key: Option<SecretString>,
    /// Largest object a put may send or a get may return.
    pub max_object_bytes: ByteSize,
    /// Operations admitted at once; the excess is refused, not queued.
    pub max_concurrency: u64,
    /// Bound for one call, retries included; GET covers the entire body
    /// through confirmed EOF from its original start.
    #[serde(with = "humantime_serde")]
    pub operation_timeout: Duration,
}

impl Default for ObjectStorageConfig {
    fn default() -> Self {
        Self {
            provider: ObjectStorageProvider::None,
            bucket: String::new(),
            region: String::new(),
            endpoint: String::new(),
            expected_bucket_owner: String::new(),
            path_style: false,
            credentials: ObjectStorageCredentials::AccessKey,
            access_key_id: String::new(),
            secret_access_key: None,
            // Both production consumers stay under 2 MiB; buffered reads
            // cost up to max_concurrency x max_object_bytes of memory.
            max_object_bytes: ByteSize::mib(8),
            max_concurrency: 8,
            // The shortest per-call budget of the production consumers.
            operation_timeout: Duration::from_secs(5),
        }
    }
}

impl ObjectStorageConfig {
    /// The secret that `access_key` credentials require.
    ///
    /// # Errors
    ///
    /// Returns an error when `object_storage.secret_access_key` is absent.
    pub fn required_secret_access_key(&self) -> Result<&SecretString, ValidationError> {
        required(
            "object_storage.secret_access_key",
            self.secret_access_key.as_ref(),
            "object_storage.credentials = access_key",
        )
    }

    pub(crate) fn validate(&self, app_env: &str) -> Result<(), ValidationError> {
        use FieldRequirement::{Forbidden, Optional, Required};

        int_range(
            "object_storage.max_object_bytes",
            self.max_object_bytes.as_u64(),
            1,
            MAX_OBJECT_BYTES_CEILING,
        )?;
        int_range(
            "object_storage.max_concurrency",
            self.max_concurrency,
            1,
            512,
        )?;
        duration_range(
            "object_storage.operation_timeout",
            self.operation_timeout,
            Duration::from_secs(1),
            Duration::from_mins(15),
        )?;
        local_development_only(
            "object_storage.provider",
            self.provider == ObjectStorageProvider::Local,
            app_env,
        )?;
        if self.provider == ObjectStorageProvider::CloudflareR2
            && !matches!(self.region.as_str(), "" | "auto")
        {
            return Err(ValidationError::new(
                "object_storage.region",
                "must be empty or auto for cloudflare_r2",
            ));
        }
        let (endpoint_requirement, region_requirement, owner_requirement) = match self.provider {
            ObjectStorageProvider::None => return Ok(()),
            ObjectStorageProvider::AmazonS3 => (Forbidden, Required, Required),
            ObjectStorageProvider::CloudflareR2
            | ObjectStorageProvider::Railway
            | ObjectStorageProvider::S3Compatible
            | ObjectStorageProvider::Local => (Required, Optional, Forbidden),
        };
        endpoint_requirement.check("object_storage.endpoint", &self.endpoint)?;
        region_requirement.check("object_storage.region", &self.region)?;
        owner_requirement.check(
            "object_storage.expected_bucket_owner",
            &self.expected_bucket_owner,
        )?;
        if self.path_style && self.provider != ObjectStorageProvider::S3Compatible {
            return Err(ValidationError::new(
                "object_storage.path_style",
                "is not accepted by the selected provider",
            ));
        }
        non_blank("object_storage.bucket", &self.bucket)?;
        self.validate_credentials()
    }

    fn validate_credentials(&self) -> Result<(), ValidationError> {
        match self.credentials {
            ObjectStorageCredentials::AccessKey => {
                non_blank("object_storage.access_key_id", &self.access_key_id)?;
                self.required_secret_access_key()?;
            }
            ObjectStorageCredentials::WorkloadIdentity => {
                if self.provider != ObjectStorageProvider::AmazonS3 {
                    return Err(ValidationError::new(
                        "object_storage.credentials",
                        "workload_identity is accepted only by amazon_s3",
                    ));
                }
                if !self.access_key_id.trim().is_empty() {
                    return Err(ValidationError::new(
                        "object_storage.access_key_id",
                        "is not accepted with workload_identity",
                    ));
                }
                if self.secret_access_key.is_some() {
                    return Err(ValidationError::new(
                        "object_storage.secret_access_key",
                        "is not accepted with workload_identity",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum FieldRequirement {
    Required,
    Optional,
    Forbidden,
}

impl FieldRequirement {
    fn check(self, key: &str, value: &str) -> Result<(), ValidationError> {
        match self {
            Self::Required => non_blank(key, value),
            Self::Optional => Ok(()),
            Self::Forbidden if value.trim().is_empty() => Ok(()),
            Self::Forbidden => Err(ValidationError::new(
                key,
                "is not accepted by the selected provider",
            )),
        }
    }
}

#[cfg(test)]
mod tests;
