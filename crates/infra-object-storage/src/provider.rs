use aws_sdk_s3::config::{SharedCredentialsProvider, SharedHttpClient};
use url::Url;

use crate::credentials::{AccessKey, AmazonCredentials};
use crate::error::ConfigErrorKind;
use crate::put_body::Payload;

#[derive(Clone, derive_more::Debug)]
pub enum Provider {
    AmazonS3 {
        region: String,
        expected_bucket_owner: String,
        credentials: AmazonCredentials,
    },
    CloudflareR2 {
        #[debug(skip)]
        endpoint: String,
        credentials: AccessKey,
    },
    Railway {
        #[debug(skip)]
        endpoint: String,
        region: Option<String>,
        credentials: AccessKey,
    },
    S3Compatible {
        #[debug(skip)]
        endpoint: String,
        region: Option<String>,
        path_style: bool,
        credentials: AccessKey,
    },
    Local {
        #[debug(skip)]
        endpoint: String,
        region: Option<String>,
        credentials: AccessKey,
    },
}

impl Provider {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::AmazonS3 { .. } => "amazon_s3",
            Self::CloudflareR2 { .. } => "cloudflare_r2",
            Self::Railway { .. } => "railway",
            Self::S3Compatible { .. } => "s3_compatible",
            Self::Local { .. } => "local",
        }
    }

    pub(crate) fn credentials_provider(
        &self,
        region: &str,
        http_client: &SharedHttpClient,
    ) -> Result<SharedCredentialsProvider, ConfigErrorKind> {
        match self {
            Self::AmazonS3 { credentials, .. } => {
                credentials.credentials_provider(region, http_client)
            }
            Self::CloudflareR2 { credentials, .. }
            | Self::Railway { credentials, .. }
            | Self::S3Compatible { credentials, .. }
            | Self::Local { credentials, .. } => credentials.credentials_provider(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UploadChecksum {
    Always,
    InMemoryOnly,
    Never,
}

impl UploadChecksum {
    pub(crate) fn applies_to(self, payload: &Payload) -> bool {
        match (self, payload) {
            (Self::Always, _) | (Self::InMemoryOnly, Payload::InMemory(_)) => true,
            (Self::InMemoryOnly, Payload::Streamed { .. }) | (Self::Never, _) => false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Target {
    AmazonS3 { expected_bucket_owner: String },
    Endpoint { url: String, path_style: bool },
}

#[derive(Debug)]
pub(crate) struct ResolvedProvider {
    pub(crate) target: Target,
    pub(crate) region: String,
    pub(crate) checksum: UploadChecksum,
}

impl ResolvedProvider {
    fn at_endpoint(
        url: String,
        region: String,
        path_style: bool,
        checksum: UploadChecksum,
    ) -> Self {
        Self {
            target: Target::Endpoint { url, path_style },
            region,
            checksum,
        }
    }
}

pub(crate) fn resolve(
    provider: &Provider,
    bucket: &str,
) -> Result<ResolvedProvider, ConfigErrorKind> {
    if !valid_bucket(bucket) {
        return Err(ConfigErrorKind::Bucket);
    }
    match provider {
        Provider::AmazonS3 {
            region,
            expected_bucket_owner,
            ..
        } => {
            let region = validate_region(region)?;
            if expected_bucket_owner.len() != 12
                || !expected_bucket_owner
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())
            {
                return Err(ConfigErrorKind::ExpectedBucketOwner);
            }
            Ok(ResolvedProvider {
                target: Target::AmazonS3 {
                    expected_bucket_owner: expected_bucket_owner.clone(),
                },
                region,
                checksum: UploadChecksum::Always,
            })
        }
        Provider::CloudflareR2 { endpoint, .. } => {
            let origin = parse_endpoint_origin(endpoint, OriginScheme::HttpsOnly)?;
            if !origin.host_str().is_some_and(r2_host) || origin.port().is_some() {
                return Err(ConfigErrorKind::Endpoint);
            }
            Ok(ResolvedProvider::at_endpoint(
                origin_without_path(&origin),
                "auto".to_owned(),
                false,
                UploadChecksum::InMemoryOnly,
            ))
        }
        Provider::Railway {
            endpoint, region, ..
        } => {
            let origin = parse_endpoint_origin(endpoint, OriginScheme::HttpsOnly)?;
            if origin.port().is_some() {
                return Err(ConfigErrorKind::Endpoint);
            }
            Ok(ResolvedProvider::at_endpoint(
                origin_without_path(&origin),
                optional_region(region.as_deref(), "auto")?,
                false,
                UploadChecksum::Never,
            ))
        }
        Provider::S3Compatible {
            endpoint,
            region,
            path_style,
            ..
        } => Ok(ResolvedProvider::at_endpoint(
            origin_without_path(&parse_endpoint_origin(endpoint, OriginScheme::HttpsOnly)?),
            optional_region(region.as_deref(), "us-east-1")?,
            *path_style,
            UploadChecksum::Never,
        )),
        Provider::Local {
            endpoint, region, ..
        } => Ok(ResolvedProvider::at_endpoint(
            origin_without_path(&parse_endpoint_origin(endpoint, OriginScheme::HttpOrHttps)?),
            optional_region(region.as_deref(), "us-east-1")?,
            true,
            UploadChecksum::Always,
        )),
    }
}

#[derive(Clone, Copy)]
enum OriginScheme {
    HttpsOnly,
    HttpOrHttps,
}

fn parse_endpoint_origin(endpoint: &str, scheme: OriginScheme) -> Result<Url, ConfigErrorKind> {
    if shared_rules::uri::has_whitespace_or_control(endpoint)
        || shared_rules::uri::authority_has_userinfo(endpoint)
    {
        return Err(ConfigErrorKind::Endpoint);
    }
    let url = Url::parse(endpoint).map_err(|_| ConfigErrorKind::Endpoint)?;
    let scheme_allowed = match scheme {
        OriginScheme::HttpsOnly => url.scheme() == "https",
        OriginScheme::HttpOrHttps => matches!(url.scheme(), "http" | "https"),
    };
    let bare = url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.host_str().is_some_and(|host| !host.is_empty());
    if scheme_allowed && bare {
        Ok(url)
    } else {
        Err(ConfigErrorKind::Endpoint)
    }
}

fn origin_without_path(origin: &Url) -> String {
    origin[..url::Position::BeforePath].to_owned()
}

fn optional_region(region: Option<&str>, default: &'static str) -> Result<String, ConfigErrorKind> {
    region.map_or_else(|| Ok(default.to_owned()), validate_region)
}

fn validate_region(region: &str) -> Result<String, ConfigErrorKind> {
    let valid = !region.is_empty()
        && region.len() <= 32
        && region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(region.to_owned())
    } else {
        Err(ConfigErrorKind::Region)
    }
}

fn r2_host(host: &str) -> bool {
    let Some(rest) = host.strip_suffix(".r2.cloudflarestorage.com") else {
        return false;
    };
    let account = match rest.split_once('.') {
        None => rest,
        Some((account, "eu" | "fedramp")) => account,
        Some(_) => return false,
    };
    account.len() == 32
        && account
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_bucket(bucket: &str) -> bool {
    let bytes = bucket.as_bytes();
    (3..=63).contains(&bytes.len())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::{AccessKey, AmazonCredentials, Provider, Target, UploadChecksum, resolve};
    use crate::ConfigErrorKind;

    const R2_ENDPOINT: &str = "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com";

    fn access_key() -> AccessKey {
        AccessKey {
            access_key_id: "AKIDEXAMPLE".to_owned(),
            secret_access_key: "secret".into(),
        }
    }

    #[test]
    fn amazon_requires_a_region_and_an_owner() {
        let admitted = resolve(
            &Provider::AmazonS3 {
                region: "eu-central-1".to_owned(),
                expected_bucket_owner: "123456789012".to_owned(),
                credentials: AmazonCredentials::WorkloadIdentity,
            },
            "evidence-bucket",
        )
        .unwrap();
        assert_eq!(
            admitted.target,
            Target::AmazonS3 {
                expected_bucket_owner: "123456789012".to_owned(),
            }
        );
        assert_eq!(admitted.checksum, UploadChecksum::Always);

        for region in ["us-gov-west-1", "cn-north-1", "eusc-de-east-1"] {
            let admitted = resolve(
                &Provider::AmazonS3 {
                    region: region.to_owned(),
                    expected_bucket_owner: "123456789012".to_owned(),
                    credentials: AmazonCredentials::WorkloadIdentity,
                },
                "evidence-bucket",
            );
            assert_eq!(admitted.unwrap().region, region);
        }
        for region in ["", "EU-central-1", "eu central 1", "eu-central-1/"] {
            let refused = resolve(
                &Provider::AmazonS3 {
                    region: region.to_owned(),
                    expected_bucket_owner: "123456789012".to_owned(),
                    credentials: AmazonCredentials::WorkloadIdentity,
                },
                "evidence-bucket",
            );
            assert_eq!(refused.unwrap_err(), ConfigErrorKind::Region, "{region:?}");
        }
        for owner in ["", "12345678901", "1234567890123", "12345678901a"] {
            let refused = resolve(
                &Provider::AmazonS3 {
                    region: "us-east-1".to_owned(),
                    expected_bucket_owner: owner.to_owned(),
                    credentials: AmazonCredentials::WorkloadIdentity,
                },
                "evidence-bucket",
            );
            assert_eq!(
                refused.unwrap_err(),
                ConfigErrorKind::ExpectedBucketOwner,
                "{owner:?}"
            );
        }
    }

    #[test]
    fn r2_admits_only_its_account_origin() {
        for endpoint in [
            R2_ENDPOINT,
            "https://0123456789abcdef0123456789abcdef.eu.r2.cloudflarestorage.com/",
            "https://0123456789abcdef0123456789abcdef.fedramp.r2.cloudflarestorage.com",
        ] {
            let admitted = resolve(
                &Provider::CloudflareR2 {
                    endpoint: endpoint.to_owned(),
                    credentials: access_key(),
                },
                "evidence-bucket",
            )
            .unwrap();
            assert_eq!(admitted.region, "auto");
            assert_eq!(admitted.checksum, UploadChecksum::InMemoryOnly);
            let Target::Endpoint { url, path_style } = admitted.target else {
                panic!("expected endpoint target");
            };
            assert!(!path_style);
            assert!(!url.ends_with('/'));
        }
        for endpoint in [
            "http://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com",
            "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com/bucket",
            "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com:8443",
            "https://0123456789abcdef.r2.cloudflarestorage.com",
            "https://0123456789abcdef0123456789abcdef.xx.r2.cloudflarestorage.com",
            "https://evil.example/0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com",
            "https://user@0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com",
            "https://@0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com",
            "https://0123456789abcdef0123456789abcdef.r2.cloud\tflarestorage.com",
            "",
        ] {
            let refused = resolve(
                &Provider::CloudflareR2 {
                    endpoint: endpoint.to_owned(),
                    credentials: access_key(),
                },
                "evidence-bucket",
            );
            assert_eq!(
                refused.unwrap_err(),
                ConfigErrorKind::Endpoint,
                "{endpoint:?}"
            );
        }
    }

    #[test]
    fn railway_takes_its_bucket_endpoint_and_defaults_the_region() {
        let admitted = resolve(
            &Provider::Railway {
                endpoint: "https://t3.storageapi.dev".to_owned(),
                region: None,
                credentials: access_key(),
            },
            "evidence-bucket-jdhhd8oe18xi",
        )
        .unwrap();
        assert_eq!(
            admitted.target,
            Target::Endpoint {
                url: "https://t3.storageapi.dev".to_owned(),
                path_style: false,
            }
        );
        assert_eq!(admitted.region, "auto");
        assert_eq!(admitted.checksum, UploadChecksum::Never);
        for endpoint in [
            "http://t3.storageapi.dev",
            "https://t3.storageapi.dev/x",
            "t3.storageapi.dev",
        ] {
            let refused = resolve(
                &Provider::Railway {
                    endpoint: endpoint.to_owned(),
                    region: Some("auto".to_owned()),
                    credentials: access_key(),
                },
                "evidence-bucket",
            );
            assert_eq!(
                refused.unwrap_err(),
                ConfigErrorKind::Endpoint,
                "{endpoint:?}"
            );
        }
        for region in ["", "AMS 1"] {
            let refused = resolve(
                &Provider::Railway {
                    endpoint: "https://t3.storageapi.dev".to_owned(),
                    region: Some(region.to_owned()),
                    credentials: access_key(),
                },
                "evidence-bucket",
            );
            assert_eq!(refused.unwrap_err(), ConfigErrorKind::Region, "{region:?}");
        }
    }

    #[test]
    fn local_allows_plaintext_and_path_style() {
        let admitted = resolve(
            &Provider::Local {
                endpoint: "http://127.0.0.1:7070".to_owned(),
                region: None,
                credentials: access_key(),
            },
            "evidence-bucket",
        )
        .unwrap();
        assert_eq!(
            admitted.target,
            Target::Endpoint {
                url: "http://127.0.0.1:7070".to_owned(),
                path_style: true,
            }
        );
        assert_eq!(admitted.region, "us-east-1");
    }

    #[test]
    fn a_generic_provider_takes_an_https_origin_and_the_common_subset() {
        let admitted = resolve(
            &Provider::S3Compatible {
                endpoint: "https://s3.eu-central-003.backblazeb2.com".to_owned(),
                region: Some("eu-central-003".to_owned()),
                path_style: false,
                credentials: access_key(),
            },
            "evidence-bucket",
        )
        .unwrap();
        assert_eq!(
            admitted.target,
            Target::Endpoint {
                url: "https://s3.eu-central-003.backblazeb2.com".to_owned(),
                path_style: false,
            }
        );
        assert_eq!(admitted.region, "eu-central-003");
        assert_eq!(admitted.checksum, UploadChecksum::Never);

        let admitted = resolve(
            &Provider::S3Compatible {
                endpoint: "https://ceph.internal.example:8443".to_owned(),
                region: None,
                path_style: true,
                credentials: access_key(),
            },
            "evidence-bucket",
        )
        .unwrap();
        assert_eq!(
            admitted.target,
            Target::Endpoint {
                url: "https://ceph.internal.example:8443".to_owned(),
                path_style: true,
            }
        );
        assert_eq!(admitted.region, "us-east-1");

        for endpoint in [
            "http://ceph.internal.example",
            "https://ceph.internal.example/bucket",
            "https://user@ceph.internal.example",
            "",
        ] {
            let refused = resolve(
                &Provider::S3Compatible {
                    endpoint: endpoint.to_owned(),
                    region: None,
                    path_style: false,
                    credentials: access_key(),
                },
                "evidence-bucket",
            );
            assert_eq!(
                refused.unwrap_err(),
                ConfigErrorKind::Endpoint,
                "{endpoint:?}"
            );
        }
    }

    #[test]
    fn buckets_are_dotless_dns_names() {
        let provider = Provider::Local {
            endpoint: "http://127.0.0.1:7070".to_owned(),
            region: None,
            credentials: access_key(),
        };
        for bucket in [
            "abc",
            "pricing-evidence",
            "document-processing-results",
            &"a".repeat(63),
        ] {
            resolve(&provider, bucket).unwrap_or_else(|_| panic!("{bucket:?}"));
        }
        for bucket in [
            "ab",
            &"a".repeat(64),
            "has.dot",
            "Upper",
            "-leading",
            "trailing-",
        ] {
            assert_eq!(
                resolve(&provider, bucket).unwrap_err(),
                ConfigErrorKind::Bucket,
                "{bucket:?}"
            );
        }
    }
}
