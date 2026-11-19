use std::time::Duration;

use bytesize::{ByteSize, GIB, MIB};
use secrecy::SecretString;
use serde::Deserialize;

use crate::app::local_development_only;
use crate::de::blank_secret_as_none;
use crate::validate::{
    Rejected, ValidationError, Violations, duration_range, int_range, non_blank, required,
};

const MAX_OBJECT_BYTES_CEILING: u64 = 5 * GIB - 5 * MIB;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProviderKind {
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
    /// An S3 emulator requiring `app.allow_insecure_development`.
    Local,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CredentialsKind {
    #[default]
    AccessKey,
    /// The AWS identity the platform gives the workload. `amazon_s3` only;
    /// the access key pair must be empty.
    WorkloadIdentity,
}

#[derive(Clone, derive_more::Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ObjectStorageInput {
    #[serde(deserialize_with = "crate::de::string_enum")]
    provider: ProviderKind,
    /// S3 bucket name. On Railway, the bucket's `BUCKET` variable.
    #[serde(deserialize_with = "crate::de::string")]
    bucket: String,
    /// Signing region. Required for `amazon_s3`; `auto` or empty for
    /// `cloudflare_r2`; optional for `railway`, `s3_compatible`, and `local`.
    #[serde(deserialize_with = "crate::de::string")]
    region: String,
    #[serde(deserialize_with = "crate::de::string")]
    #[debug(skip)]
    endpoint: String,
    #[serde(deserialize_with = "crate::de::string")]
    expected_bucket_owner: String,
    #[serde(deserialize_with = "crate::de::boolean")]
    path_style: bool,
    #[serde(deserialize_with = "crate::de::string_enum")]
    credentials: CredentialsKind,
    #[serde(deserialize_with = "crate::de::string")]
    access_key_id: String,
    /// Set `APP__OBJECT_STORAGE__SECRET_ACCESS_KEY` in the environment or via
    /// `--secrets-dir`. Blank is absent. Absent under `workload_identity`.
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    secret_access_key: Option<SecretString>,
    max_object_bytes: ByteSize,
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    max_concurrency: u64,
    /// Bound for one call, retries included; GET covers the entire body
    /// through confirmed EOF from its original start.
    #[serde(deserialize_with = "crate::de::duration")]
    operation_timeout: Duration,
}

impl Default for ObjectStorageInput {
    fn default() -> Self {
        Self {
            provider: ProviderKind::None,
            bucket: String::new(),
            region: String::new(),
            endpoint: String::new(),
            expected_bucket_owner: String::new(),
            path_style: false,
            credentials: CredentialsKind::AccessKey,
            access_key_id: String::new(),
            secret_access_key: None,
            max_object_bytes: ByteSize::mib(8),
            max_concurrency: 8,
            operation_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, derive_more::Debug, PartialEq, Eq)]
pub enum ObjectStorageProvider {
    AmazonS3 {
        region: String,
        expected_bucket_owner: String,
    },
    CloudflareR2 {
        #[debug(skip)]
        endpoint: String,
    },
    Railway {
        #[debug(skip)]
        endpoint: String,
        region: Option<String>,
    },
    S3Compatible {
        #[debug(skip)]
        endpoint: String,
        region: Option<String>,
        path_style: bool,
    },
    Local {
        #[debug(skip)]
        endpoint: String,
        region: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub enum ObjectStorageCredentials {
    AccessKey {
        access_key_id: String,
        secret_access_key: SecretString,
    },
    WorkloadIdentity,
}

impl ObjectStorageCredentials {
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::AccessKey { .. } => "access_key",
            Self::WorkloadIdentity => "workload_identity",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ObjectStorageConfig {
    pub provider: ObjectStorageProvider,
    pub bucket: String,
    pub credentials: ObjectStorageCredentials,
    pub max_object_bytes: ByteSize,
    pub max_concurrency: u64,
    pub operation_timeout: Duration,
}

struct ProviderFields {
    endpoint: String,
    region: String,
    expected_bucket_owner: String,
    path_style: bool,
}

impl ProviderFields {
    fn admit_amazon_s3(self, v: &mut Violations) -> Result<ObjectStorageProvider, Rejected> {
        v.check(forbidden(
            "object_storage.endpoint",
            &self.endpoint,
            "must be empty when object_storage.provider = amazon_s3",
        ));
        let region = required_text(v, "object_storage.region", self.region);
        let owner = required_text(
            v,
            "object_storage.expected_bucket_owner",
            self.expected_bucket_owner,
        );
        refuse_path_style(self.path_style, v);
        Ok(ObjectStorageProvider::AmazonS3 {
            region: region?,
            expected_bucket_owner: owner?,
        })
    }

    fn admit_cloudflare_r2(self, v: &mut Violations) -> Result<ObjectStorageProvider, Rejected> {
        if !matches!(self.region.trim(), "" | "auto") {
            v.reject(ValidationError::new(
                "object_storage.region",
                "must be empty or auto for cloudflare_r2",
            ));
        }
        let endpoint = required_text(v, "object_storage.endpoint", self.endpoint);
        refuse_owner(&self.expected_bucket_owner, v);
        refuse_path_style(self.path_style, v);
        Ok(ObjectStorageProvider::CloudflareR2 {
            endpoint: endpoint?,
        })
    }

    fn admit_endpoint_provider(
        self,
        provider: ProviderKind,
        v: &mut Violations,
    ) -> Result<(String, Option<String>), Rejected> {
        let endpoint = required_text(v, "object_storage.endpoint", self.endpoint);
        refuse_owner(&self.expected_bucket_owner, v);
        if provider != ProviderKind::S3Compatible {
            refuse_path_style(self.path_style, v);
        }
        Ok((endpoint?, optional_text(self.region)))
    }
}

fn refuse_owner(expected_bucket_owner: &str, v: &mut Violations) {
    v.check(forbidden(
        "object_storage.expected_bucket_owner",
        expected_bucket_owner,
        "is accepted only when object_storage.provider = amazon_s3",
    ));
}

fn refuse_path_style(path_style: bool, v: &mut Violations) {
    if path_style {
        v.reject(ValidationError::new(
            "object_storage.path_style",
            "must be false unless object_storage.provider = s3_compatible",
        ));
    }
}

impl ObjectStorageInput {
    pub(crate) fn admit(
        self,
        local_development: bool,
        v: &mut Violations,
    ) -> Result<Option<ObjectStorageConfig>, Rejected> {
        v.check(int_range(
            "object_storage.max_object_bytes",
            self.max_object_bytes.as_u64(),
            1..=MAX_OBJECT_BYTES_CEILING,
        ));
        v.check(int_range(
            "object_storage.max_concurrency",
            self.max_concurrency,
            1..=512,
        ));
        v.check(duration_range(
            "object_storage.operation_timeout",
            self.operation_timeout,
            Duration::from_secs(1)..=Duration::from_mins(15),
        ));
        v.check(local_development_only(
            "object_storage.provider",
            self.provider == ProviderKind::Local,
            local_development,
        ));
        let path_style = self.path_style;
        let provider_fields = ProviderFields {
            endpoint: self.endpoint,
            region: self.region,
            expected_bucket_owner: self.expected_bucket_owner,
            path_style,
        };
        let provider = match self.provider {
            ProviderKind::None => return Ok(None),
            ProviderKind::AmazonS3 => provider_fields.admit_amazon_s3(v),
            ProviderKind::CloudflareR2 => provider_fields.admit_cloudflare_r2(v),
            ProviderKind::Railway => provider_fields
                .admit_endpoint_provider(self.provider, v)
                .map(|(endpoint, region)| ObjectStorageProvider::Railway { endpoint, region }),
            ProviderKind::S3Compatible => provider_fields
                .admit_endpoint_provider(self.provider, v)
                .map(|(endpoint, region)| ObjectStorageProvider::S3Compatible {
                    endpoint,
                    region,
                    path_style,
                }),
            ProviderKind::Local => provider_fields
                .admit_endpoint_provider(self.provider, v)
                .map(|(endpoint, region)| ObjectStorageProvider::Local { endpoint, region }),
        };
        v.check(non_blank("object_storage.bucket", &self.bucket));
        let credentials = Self::admit_credentials(
            self.credentials,
            self.provider,
            self.access_key_id,
            self.secret_access_key,
            v,
        );
        Ok(Some(ObjectStorageConfig {
            provider: provider?,
            bucket: self.bucket,
            credentials: credentials?,
            max_object_bytes: self.max_object_bytes,
            max_concurrency: self.max_concurrency,
            operation_timeout: self.operation_timeout,
        }))
    }

    fn admit_credentials(
        credentials: CredentialsKind,
        provider: ProviderKind,
        access_key_id: String,
        secret_access_key: Option<SecretString>,
        v: &mut Violations,
    ) -> Result<ObjectStorageCredentials, Rejected> {
        match credentials {
            CredentialsKind::AccessKey => {
                let access_key_id = required_text(v, "object_storage.access_key_id", access_key_id);
                let secret_access_key = v.take(required(
                    "object_storage.secret_access_key",
                    secret_access_key,
                    "object_storage.credentials = access_key",
                ));
                Ok(ObjectStorageCredentials::AccessKey {
                    access_key_id: access_key_id?,
                    secret_access_key: secret_access_key?,
                })
            }
            CredentialsKind::WorkloadIdentity => {
                if provider != ProviderKind::AmazonS3 {
                    v.reject(ValidationError::new(
                        "object_storage.credentials",
                        "workload_identity is accepted only by amazon_s3",
                    ));
                }
                v.reject_present(
                    &[
                        (
                            "object_storage.access_key_id",
                            !access_key_id.trim().is_empty(),
                        ),
                        (
                            "object_storage.secret_access_key",
                            secret_access_key.is_some(),
                        ),
                    ],
                    "is not accepted with workload_identity",
                );
                Ok(ObjectStorageCredentials::WorkloadIdentity)
            }
        }
    }
}

fn required_text(v: &mut Violations, key: &str, value: String) -> Result<String, Rejected> {
    v.take(non_blank(key, &value).map(|()| value))
}

fn optional_text(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn forbidden(key: &str, value: &str, reason: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Ok(());
    }
    Err(ValidationError::new(key, reason))
}

#[cfg(test)]
mod tests;
