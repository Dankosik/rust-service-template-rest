use std::time::Duration;

use bytesize::{ByteSize, GIB, MIB};
use secrecy::SecretString;
use serde::Deserialize;

use crate::app::local_development_only;
use crate::de::{
    BlankAsNoneVerbatim, StrictBool, StrictCount, StrictDuration, StrictEnum, StrictString,
};
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

#[serde_with::serde_as]
#[derive(Clone, derive_more::Debug, Deserialize, educe::Educe)]
#[educe(Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ObjectStorageInput {
    #[serde_as(as = "StrictEnum")]
    provider: ProviderKind,
    /// S3 bucket name. On Railway, the bucket's `BUCKET` variable.
    #[serde_as(as = "StrictString")]
    bucket: String,
    /// Signing region. Required for `amazon_s3`; `auto` or empty for
    /// `cloudflare_r2`; optional for `railway`, `s3_compatible`, and `local`.
    #[serde(default)]
    #[serde_as(as = "BlankAsNoneVerbatim")]
    region: Option<String>,
    #[serde(default)]
    #[serde_as(as = "BlankAsNoneVerbatim")]
    #[debug(skip)]
    endpoint: Option<String>,
    #[serde(default)]
    #[serde_as(as = "BlankAsNoneVerbatim")]
    expected_bucket_owner: Option<String>,
    #[serde_as(as = "StrictBool")]
    path_style: bool,
    #[serde_as(as = "StrictEnum")]
    credentials: CredentialsKind,
    #[serde(default)]
    #[serde_as(as = "BlankAsNoneVerbatim")]
    access_key_id: Option<String>,
    /// Set `APP__OBJECT_STORAGE__SECRET_ACCESS_KEY` in the environment or via
    /// `--secrets-dir`. Blank is absent. Absent under `workload_identity`.
    #[serde(default)]
    #[serde_as(as = "BlankAsNoneVerbatim")]
    secret_access_key: Option<SecretString>,
    #[educe(Default(expression = ByteSize::mib(8)))]
    max_object_bytes: ByteSize,
    #[serde_as(as = "StrictCount")]
    #[educe(Default = 8)]
    max_concurrency: u64,
    /// Bound for one call, retries included; GET covers the entire body
    /// through confirmed EOF from its original start.
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(5)))]
    operation_timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct ObjectStorageAccessKey {
    pub access_key_id: String,
    pub secret_access_key: SecretString,
}

#[derive(Clone, Debug)]
pub enum ObjectStorageAmazonCredentials {
    AccessKey(ObjectStorageAccessKey),
    WorkloadIdentity,
}

#[derive(Clone, derive_more::Debug)]
pub enum ObjectStorageProvider {
    AmazonS3 {
        region: String,
        expected_bucket_owner: String,
        credentials: ObjectStorageAmazonCredentials,
    },
    CloudflareR2 {
        #[debug(skip)]
        endpoint: String,
        credentials: ObjectStorageAccessKey,
    },
    Railway {
        #[debug(skip)]
        endpoint: String,
        region: Option<String>,
        credentials: ObjectStorageAccessKey,
    },
    S3Compatible {
        #[debug(skip)]
        endpoint: String,
        region: Option<String>,
        path_style: bool,
        credentials: ObjectStorageAccessKey,
    },
    Local {
        #[debug(skip)]
        endpoint: String,
        region: Option<String>,
        credentials: ObjectStorageAccessKey,
    },
}

impl ObjectStorageProvider {
    #[must_use]
    pub const fn credentials_label(&self) -> &'static str {
        match self {
            Self::AmazonS3 {
                credentials: ObjectStorageAmazonCredentials::WorkloadIdentity,
                ..
            } => "workload_identity",
            Self::AmazonS3 {
                credentials: ObjectStorageAmazonCredentials::AccessKey(_),
                ..
            }
            | Self::CloudflareR2 { .. }
            | Self::Railway { .. }
            | Self::S3Compatible { .. }
            | Self::Local { .. } => "access_key",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ObjectStorageConfig {
    pub provider: ObjectStorageProvider,
    pub bucket: String,
    pub max_object_bytes: ByteSize,
    pub max_concurrency: u64,
    pub operation_timeout: Duration,
}

struct ProviderFields {
    endpoint: Option<String>,
    region: Option<String>,
    expected_bucket_owner: Option<String>,
    path_style: bool,
}

struct CredentialFields {
    kind: CredentialsKind,
    access_key_id: Option<String>,
    secret_access_key: Option<SecretString>,
}

impl CredentialFields {
    fn admit_access_key(self, v: &mut Violations) -> Result<ObjectStorageAccessKey, Rejected> {
        match self.kind {
            CredentialsKind::AccessKey => {
                let access_key_id =
                    required_text(v, "object_storage.access_key_id", self.access_key_id);
                let secret_access_key = v.record(required(
                    "object_storage.secret_access_key",
                    self.secret_access_key,
                    "object_storage.credentials = access_key",
                ));
                Ok(ObjectStorageAccessKey {
                    access_key_id: access_key_id?,
                    secret_access_key: secret_access_key?,
                })
            }
            CredentialsKind::WorkloadIdentity => {
                let rejected = v.reject(ValidationError::new(
                    "object_storage.credentials",
                    "workload_identity is accepted only by amazon_s3",
                ));
                refuse_access_key(
                    self.access_key_id.is_some(),
                    self.secret_access_key.is_some(),
                    v,
                );
                Err(rejected)
            }
        }
    }

    fn admit_amazon(self, v: &mut Violations) -> Result<ObjectStorageAmazonCredentials, Rejected> {
        match self.kind {
            CredentialsKind::AccessKey => self
                .admit_access_key(v)
                .map(ObjectStorageAmazonCredentials::AccessKey),
            CredentialsKind::WorkloadIdentity => {
                refuse_access_key(
                    self.access_key_id.is_some(),
                    self.secret_access_key.is_some(),
                    v,
                );
                Ok(ObjectStorageAmazonCredentials::WorkloadIdentity)
            }
        }
    }
}

impl ProviderFields {
    fn admit(
        self,
        provider: ProviderKind,
        bucket: &str,
        credentials: CredentialFields,
        v: &mut Violations,
    ) -> Result<Option<ObjectStorageProvider>, Rejected> {
        let path_style = self.path_style;
        match provider {
            ProviderKind::None => Ok(None),
            ProviderKind::AmazonS3 => self.admit_amazon_s3(bucket, credentials, v).map(Some),
            ProviderKind::CloudflareR2 => {
                self.admit_cloudflare_r2(bucket, credentials, v).map(Some)
            }
            ProviderKind::Railway => self
                .admit_endpoint(
                    bucket,
                    credentials,
                    false,
                    v,
                    |endpoint, region, credentials| ObjectStorageProvider::Railway {
                        endpoint,
                        region,
                        credentials,
                    },
                )
                .map(Some),
            ProviderKind::S3Compatible => self
                .admit_endpoint(
                    bucket,
                    credentials,
                    true,
                    v,
                    |endpoint, region, credentials| ObjectStorageProvider::S3Compatible {
                        endpoint,
                        region,
                        path_style,
                        credentials,
                    },
                )
                .map(Some),
            ProviderKind::Local => self
                .admit_endpoint(
                    bucket,
                    credentials,
                    false,
                    v,
                    |endpoint, region, credentials| ObjectStorageProvider::Local {
                        endpoint,
                        region,
                        credentials,
                    },
                )
                .map(Some),
        }
    }

    fn admit_amazon_s3(
        self,
        bucket: &str,
        credentials: CredentialFields,
        v: &mut Violations,
    ) -> Result<ObjectStorageProvider, Rejected> {
        v.reject_present(
            &[("object_storage.endpoint", self.endpoint.is_some())],
            "must be empty when object_storage.provider = amazon_s3",
        );
        let region = required_text(v, "object_storage.region", self.region);
        let owner = required_text(
            v,
            "object_storage.expected_bucket_owner",
            self.expected_bucket_owner,
        );
        refuse_path_style(self.path_style, v);
        v.check(non_blank("object_storage.bucket", bucket));
        let credentials = credentials.admit_amazon(v);
        Ok(ObjectStorageProvider::AmazonS3 {
            region: region?,
            expected_bucket_owner: owner?,
            credentials: credentials?,
        })
    }

    fn admit_cloudflare_r2(
        self,
        bucket: &str,
        credentials: CredentialFields,
        v: &mut Violations,
    ) -> Result<ObjectStorageProvider, Rejected> {
        if self
            .region
            .as_deref()
            .is_some_and(|region| region.trim() != "auto")
        {
            v.reject(ValidationError::new(
                "object_storage.region",
                "must be empty or auto for cloudflare_r2",
            ));
        }
        let endpoint = required_text(v, "object_storage.endpoint", self.endpoint);
        refuse_owner(self.expected_bucket_owner.is_some(), v);
        refuse_path_style(self.path_style, v);
        v.check(non_blank("object_storage.bucket", bucket));
        let credentials = credentials.admit_access_key(v);
        Ok(ObjectStorageProvider::CloudflareR2 {
            endpoint: endpoint?,
            credentials: credentials?,
        })
    }

    fn admit_endpoint(
        self,
        bucket: &str,
        credentials: CredentialFields,
        allow_path_style: bool,
        v: &mut Violations,
        build: impl FnOnce(String, Option<String>, ObjectStorageAccessKey) -> ObjectStorageProvider,
    ) -> Result<ObjectStorageProvider, Rejected> {
        let endpoint = required_text(v, "object_storage.endpoint", self.endpoint);
        refuse_owner(self.expected_bucket_owner.is_some(), v);
        if !allow_path_style {
            refuse_path_style(self.path_style, v);
        }
        v.check(non_blank("object_storage.bucket", bucket));
        let credentials = credentials.admit_access_key(v);
        Ok(build(endpoint?, self.region, credentials?))
    }
}

fn refuse_owner(present: bool, v: &mut Violations) {
    v.reject_present(
        &[("object_storage.expected_bucket_owner", present)],
        "is accepted only when object_storage.provider = amazon_s3",
    );
}

fn refuse_path_style(path_style: bool, v: &mut Violations) {
    if path_style {
        v.reject(ValidationError::new(
            "object_storage.path_style",
            "must be false unless object_storage.provider = s3_compatible",
        ));
    }
}

fn refuse_access_key(has_id: bool, has_secret: bool, v: &mut Violations) {
    v.reject_present(
        &[
            ("object_storage.access_key_id", has_id),
            ("object_storage.secret_access_key", has_secret),
        ],
        "is not accepted with workload_identity",
    );
}

impl ObjectStorageInput {
    pub(crate) fn admit(
        self,
        local_development: bool,
        v: &mut Violations,
    ) -> Result<Option<ObjectStorageConfig>, Rejected> {
        let Self {
            provider,
            bucket,
            region,
            endpoint,
            expected_bucket_owner,
            path_style,
            credentials,
            access_key_id,
            secret_access_key,
            max_object_bytes,
            max_concurrency,
            operation_timeout,
        } = self;
        v.check(int_range(
            "object_storage.max_object_bytes",
            max_object_bytes.as_u64(),
            1..=MAX_OBJECT_BYTES_CEILING,
        ));
        v.check(int_range(
            "object_storage.max_concurrency",
            max_concurrency,
            1..=512,
        ));
        v.check(duration_range(
            "object_storage.operation_timeout",
            operation_timeout,
            Duration::from_secs(1)..=Duration::from_mins(15),
        ));
        v.check(local_development_only(
            "object_storage.provider",
            provider == ProviderKind::Local,
            local_development,
        ));
        let Some(provider) = ProviderFields {
            endpoint,
            region,
            expected_bucket_owner,
            path_style,
        }
        .admit(
            provider,
            &bucket,
            CredentialFields {
                kind: credentials,
                access_key_id,
                secret_access_key,
            },
            v,
        )
        .transpose() else {
            return Ok(None);
        };
        Ok(Some(ObjectStorageConfig {
            provider: provider?,
            bucket,
            max_object_bytes,
            max_concurrency,
            operation_timeout,
        }))
    }
}

fn required_text(v: &mut Violations, key: &str, value: Option<String>) -> Result<String, Rejected> {
    v.record(value.ok_or_else(|| ValidationError::new(key, "cannot be empty")))
}

#[cfg(test)]
mod tests;
