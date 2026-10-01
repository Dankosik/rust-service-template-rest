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

use crate::de::blank_secret_as_none;
use crate::validate::{ValidationError, duration_range, int_range, non_empty};

/// Largest object without multipart: R2's single-upload limit (5 GiB minus
/// 5 MiB), the smallest documented across the supported providers.
pub const MAX_OBJECT_BYTES_CEILING: u64 = 5 * 1024 * 1024 * 1024 - 5 * 1024 * 1024;

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
    /// Bound for one call up to its response headers, retries included.
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
    /// Whether a provider is selected.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.provider != ObjectStorageProvider::None
    }

    pub(crate) fn validate(&self, app_env: &str) -> Result<(), ValidationError> {
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
        let (endpoint, region, owner) = match self.provider {
            ObjectStorageProvider::None => return Ok(()),
            ObjectStorageProvider::AmazonS3 => (Field::Forbidden, Field::Required, Field::Required),
            ObjectStorageProvider::CloudflareR2 => {
                if !matches!(self.region.as_str(), "" | "auto") {
                    return Err(ValidationError::new(
                        "object_storage.region",
                        "must be empty or auto for cloudflare_r2",
                    ));
                }
                (Field::Required, Field::Optional, Field::Forbidden)
            }
            ObjectStorageProvider::Railway | ObjectStorageProvider::S3Compatible => {
                (Field::Required, Field::Optional, Field::Forbidden)
            }
            ObjectStorageProvider::Local => {
                if !matches!(app_env, "local" | "development") {
                    return Err(ValidationError::new(
                        "object_storage.provider",
                        "local is local/development-only",
                    ));
                }
                (Field::Required, Field::Optional, Field::Forbidden)
            }
        };
        endpoint.check("object_storage.endpoint", &self.endpoint)?;
        region.check("object_storage.region", &self.region)?;
        owner.check(
            "object_storage.expected_bucket_owner",
            &self.expected_bucket_owner,
        )?;
        if self.path_style && self.provider != ObjectStorageProvider::S3Compatible {
            return Err(ValidationError::new(
                "object_storage.path_style",
                "is not accepted by the selected provider",
            ));
        }
        non_empty("object_storage.bucket", &self.bucket)?;
        match self.credentials {
            ObjectStorageCredentials::AccessKey => {
                non_empty("object_storage.access_key_id", &self.access_key_id)?;
                if self.secret_access_key.is_none() {
                    return Err(ValidationError::new(
                        "object_storage.secret_access_key",
                        "is required; set APP__OBJECT_STORAGE__SECRET_ACCESS_KEY",
                    ));
                }
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

/// Whether the selected provider takes a key.
#[derive(Clone, Copy)]
enum Field {
    Required,
    Optional,
    Forbidden,
}

impl Field {
    fn check(self, key: &str, value: &str) -> Result<(), ValidationError> {
        match self {
            Self::Required => non_empty(key, value),
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
mod tests {
    use super::*;

    fn railway() -> ObjectStorageConfig {
        ObjectStorageConfig {
            provider: ObjectStorageProvider::Railway,
            bucket: "results-jdhhd8oe18xi".to_owned(),
            endpoint: "https://t3.storageapi.dev".to_owned(),
            access_key_id: "tid_example".to_owned(),
            secret_access_key: Some(SecretString::from("hunter2".to_owned())),
            ..ObjectStorageConfig::default()
        }
    }

    fn key_of(config: &ObjectStorageConfig, env: &str) -> String {
        config.validate(env).unwrap_err().key
    }

    #[test]
    fn defaults_are_inert() {
        let config = ObjectStorageConfig::default();
        assert!(!config.is_active());
        assert_eq!(config.max_object_bytes, ByteSize::mib(8));
        assert_eq!(config.max_concurrency, 8);
        assert_eq!(config.operation_timeout, Duration::from_secs(5));
        config.validate("production").unwrap();
    }

    #[test]
    fn an_active_provider_needs_bucket_and_credentials() {
        railway().validate("production").unwrap();
        for (mutate, key) in [
            (
                (|config: &mut ObjectStorageConfig| config.bucket.clear()) as fn(&mut _),
                "object_storage.bucket",
            ),
            (
                |config| config.access_key_id.clear(),
                "object_storage.access_key_id",
            ),
            (
                |config| config.secret_access_key = None,
                "object_storage.secret_access_key",
            ),
            (|config| config.endpoint.clear(), "object_storage.endpoint"),
        ] {
            let mut config = railway();
            mutate(&mut config);
            assert_eq!(key_of(&config, "production"), key);
        }
    }

    #[test]
    fn providers_accept_only_their_own_keys() {
        let mut amazon = railway();
        amazon.provider = ObjectStorageProvider::AmazonS3;
        assert_eq!(key_of(&amazon, "production"), "object_storage.endpoint");
        amazon.endpoint.clear();
        assert_eq!(key_of(&amazon, "production"), "object_storage.region");
        amazon.region = "eu-central-1".to_owned();
        assert_eq!(
            key_of(&amazon, "production"),
            "object_storage.expected_bucket_owner"
        );
        amazon.expected_bucket_owner = "123456789012".to_owned();
        amazon.validate("production").unwrap();

        let mut r2 = railway();
        r2.provider = ObjectStorageProvider::CloudflareR2;
        r2.region = "eu-central-1".to_owned();
        assert_eq!(key_of(&r2, "production"), "object_storage.region");
        r2.region = "auto".to_owned();
        r2.validate("production").unwrap();
        r2.expected_bucket_owner = "123456789012".to_owned();
        assert_eq!(
            key_of(&r2, "production"),
            "object_storage.expected_bucket_owner"
        );
    }

    #[test]
    fn workload_identity_is_amazon_only_and_takes_no_access_key() {
        let mut amazon = railway();
        amazon.provider = ObjectStorageProvider::AmazonS3;
        amazon.endpoint.clear();
        amazon.region = "eu-central-1".to_owned();
        amazon.expected_bucket_owner = "123456789012".to_owned();
        amazon.credentials = ObjectStorageCredentials::WorkloadIdentity;
        assert_eq!(
            key_of(&amazon, "production"),
            "object_storage.access_key_id"
        );
        amazon.access_key_id.clear();
        assert_eq!(
            key_of(&amazon, "production"),
            "object_storage.secret_access_key"
        );
        amazon.secret_access_key = None;
        amazon.validate("production").unwrap();

        let mut other = railway();
        other.credentials = ObjectStorageCredentials::WorkloadIdentity;
        other.access_key_id.clear();
        other.secret_access_key = None;
        assert_eq!(key_of(&other, "production"), "object_storage.credentials");
    }

    #[test]
    fn path_style_belongs_to_the_generic_provider() {
        let mut generic = railway();
        generic.provider = ObjectStorageProvider::S3Compatible;
        generic.path_style = true;
        generic.validate("production").unwrap();
        generic.expected_bucket_owner = "123456789012".to_owned();
        assert_eq!(
            key_of(&generic, "production"),
            "object_storage.expected_bucket_owner"
        );

        let mut other = railway();
        other.path_style = true;
        assert_eq!(key_of(&other, "production"), "object_storage.path_style");
    }

    #[test]
    fn the_emulator_provider_is_local_only() {
        let mut local = railway();
        local.provider = ObjectStorageProvider::Local;
        assert_eq!(key_of(&local, "production"), "object_storage.provider");
        local.validate("local").unwrap();
        local.validate("development").unwrap();
    }

    #[test]
    fn limits_are_bounded_even_when_inert() {
        for (mutate, key) in [
            (
                (|config: &mut ObjectStorageConfig| config.max_object_bytes = ByteSize::b(0))
                    as fn(&mut _),
                "object_storage.max_object_bytes",
            ),
            (
                |config| config.max_object_bytes = ByteSize::b(MAX_OBJECT_BYTES_CEILING + 1),
                "object_storage.max_object_bytes",
            ),
            (
                |config| config.max_concurrency = 0,
                "object_storage.max_concurrency",
            ),
            (
                |config| config.max_concurrency = 513,
                "object_storage.max_concurrency",
            ),
            (
                |config| config.operation_timeout = Duration::from_millis(999),
                "object_storage.operation_timeout",
            ),
            (
                |config| config.operation_timeout = Duration::from_secs(901),
                "object_storage.operation_timeout",
            ),
        ] {
            let mut config = ObjectStorageConfig::default();
            mutate(&mut config);
            assert_eq!(key_of(&config, "production"), key);
        }
        assert_eq!(MAX_OBJECT_BYTES_CEILING, 5_363_466_240);
    }

    #[test]
    fn debug_output_redacts_the_secret() {
        let rendered = format!("{:?}", railway());
        assert!(!rendered.contains("hunter2"), "{rendered}");
    }
}
