//! Provider admission: endpoint, region, addressing, and which optional S3
//! features each provider receives.

use url::Url;

use crate::body::UploadSource;
use crate::error::ConfigError;

/// Where the bucket lives. Each provider carries only the fields it accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provider {
    /// Amazon S3. The SDK resolves the regional endpoint.
    AmazonS3 {
        /// Region, for example `eu-central-1`.
        region: String,
        /// The 12-digit account that must own the bucket.
        expected_bucket_owner: String,
    },
    /// Cloudflare R2. Region is always `auto`.
    CloudflareR2 {
        /// `https://<account id>[.eu|.fedramp].r2.cloudflarestorage.com`.
        endpoint: String,
    },
    /// Railway Buckets (Tigris). Endpoint and region come from the bucket's
    /// `ENDPOINT` and `REGION` variables.
    Railway {
        /// HTTPS origin, `https://t3.storageapi.dev` today.
        endpoint: String,
        /// Signing region; `auto` when empty.
        region: String,
    },
    /// Any other S3-compatible store at an HTTPS origin, such as Backblaze
    /// B2, Ceph, or Hetzner Object Storage. It receives only the common S3
    /// subset: no upload checksum and no expected bucket owner.
    S3Compatible {
        /// `https://` origin; a port is allowed.
        endpoint: String,
        /// Signing region; `us-east-1` when empty.
        region: String,
        /// Path-style addressing (`<endpoint>/<bucket>/<key>`) for a store
        /// that serves no `<bucket>.<endpoint>` host.
        path_style: bool,
    },
    /// An S3 emulator for local development and tests. Plaintext is allowed;
    /// the caller must already have applied the local-only policy.
    Local {
        /// `http://` or `https://` origin.
        endpoint: String,
        /// Signing region; `us-east-1` when empty.
        region: String,
    },
}

impl Provider {
    /// The configuration spelling, safe to log.
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
}

/// Which uploads name a CRC64NVME checksum for the SDK to compute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UploadChecksum {
    /// Every upload: a header for bytes, an `aws-chunked` trailer for a stream.
    Always,
    /// Bytes only, sent as a signed header: trailer support is undocumented.
    BytesOnly,
    /// None until a conformance run proves the provider accepts it.
    Never,
}

impl UploadChecksum {
    pub(crate) fn applies_to(self, source: &UploadSource) -> bool {
        match (self, source) {
            (Self::Always, _) | (Self::BytesOnly, UploadSource::InMemory) => true,
            (Self::BytesOnly, UploadSource::Streamed { .. }) | (Self::Never, _) => false,
        }
    }
}

/// The admitted provider tuple the SDK config is built from.
#[derive(Debug)]
pub(crate) struct Admitted {
    pub(crate) endpoint: Option<String>,
    pub(crate) region: String,
    pub(crate) path_style: bool,
    pub(crate) expected_bucket_owner: Option<String>,
    pub(crate) checksum: UploadChecksum,
    /// The span's `cloud.region`: set only for Amazon S3, where the signing
    /// region names a real one. Elsewhere it can be a placeholder (`auto`).
    pub(crate) cloud_region: Option<String>,
    /// [`crate::CredentialSource::WorkloadIdentity`] is accepted: only Amazon
    /// S3 has an AWS workload identity.
    pub(crate) workload_identity: bool,
}

pub(crate) fn admit(provider: &Provider, bucket: &str) -> Result<Admitted, ConfigError> {
    if !valid_bucket(bucket) {
        return Err(ConfigError::Bucket);
    }
    match provider {
        Provider::AmazonS3 {
            region,
            expected_bucket_owner,
        } => {
            let region = signing_region(region, None)?;
            if expected_bucket_owner.len() != 12
                || !expected_bucket_owner
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())
            {
                return Err(ConfigError::ExpectedBucketOwner);
            }
            Ok(Admitted {
                endpoint: None,
                cloud_region: Some(region.clone()),
                region,
                path_style: false,
                expected_bucket_owner: Some(expected_bucket_owner.clone()),
                checksum: UploadChecksum::Always,
                workload_identity: true,
            })
        }
        Provider::CloudflareR2 { endpoint } => {
            let origin = origin(endpoint, OriginScheme::HttpsOnly)?;
            if !origin.host_str().is_some_and(r2_host) || origin.port().is_some() {
                return Err(ConfigError::Endpoint);
            }
            Ok(Admitted {
                endpoint: Some(serialize(&origin)),
                region: "auto".to_owned(),
                path_style: false,
                expected_bucket_owner: None,
                checksum: UploadChecksum::BytesOnly,
                cloud_region: None,
                workload_identity: false,
            })
        }
        Provider::Railway { endpoint, region } => {
            let origin = origin(endpoint, OriginScheme::HttpsOnly)?;
            if origin.port().is_some() {
                return Err(ConfigError::Endpoint);
            }
            Ok(Admitted {
                endpoint: Some(serialize(&origin)),
                region: signing_region(region, Some("auto"))?,
                path_style: false,
                expected_bucket_owner: None,
                checksum: UploadChecksum::Never,
                cloud_region: None,
                workload_identity: false,
            })
        }
        Provider::S3Compatible {
            endpoint,
            region,
            path_style,
        } => Ok(Admitted {
            endpoint: Some(serialize(&origin(endpoint, OriginScheme::HttpsOnly)?)),
            region: signing_region(region, Some("us-east-1"))?,
            path_style: *path_style,
            expected_bucket_owner: None,
            checksum: UploadChecksum::Never,
            cloud_region: None,
            workload_identity: false,
        }),
        Provider::Local { endpoint, region } => Ok(Admitted {
            endpoint: Some(serialize(&origin(endpoint, OriginScheme::HttpOrHttps)?)),
            region: signing_region(region, Some("us-east-1"))?,
            path_style: true,
            expected_bucket_owner: None,
            checksum: UploadChecksum::Always,
            cloud_region: None,
            workload_identity: false,
        }),
    }
}

#[derive(Clone, Copy)]
enum OriginScheme {
    HttpsOnly,
    HttpOrHttps,
}

/// Reject whitespace, control characters and userinfo before parsing can
/// normalize them away. Check the remaining origin components after parsing.
fn origin(endpoint: &str, scheme: OriginScheme) -> Result<Url, ConfigError> {
    if shared_rules::uri::has_whitespace_or_control(endpoint)
        || shared_rules::uri::authority_has_userinfo(endpoint)
    {
        return Err(ConfigError::Endpoint);
    }
    let url = Url::parse(endpoint).map_err(|_| ConfigError::Endpoint)?;
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
        Err(ConfigError::Endpoint)
    }
}

/// `scheme://host[:port]` without the trailing slash `Url` adds.
fn serialize(origin: &Url) -> String {
    origin.as_str().trim_end_matches('/').to_owned()
}

/// A shape-validated signing region, or the provider's default when empty.
fn signing_region(region: &str, default: Option<&str>) -> Result<String, ConfigError> {
    if region.is_empty() {
        return default.map(str::to_owned).ok_or(ConfigError::Region);
    }
    let valid = region.len() <= 32
        && region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(region.to_owned())
    } else {
        Err(ConfigError::Region)
    }
}

/// `<32 hex>[.eu|.fedramp].r2.cloudflarestorage.com`.
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

/// Dotless DNS bucket names (3 to 63 of `[a-z0-9-]`, alphanumeric at both
/// ends), so virtual-hosted TLS wildcards hold.
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
    use super::{Provider, UploadChecksum, admit};
    use crate::ConfigError;

    const R2_ENDPOINT: &str = "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com";

    #[test]
    fn amazon_requires_a_region_and_an_owner() {
        let admitted = admit(
            &Provider::AmazonS3 {
                region: "eu-central-1".to_owned(),
                expected_bucket_owner: "123456789012".to_owned(),
            },
            "evidence-bucket",
        )
        .unwrap();
        assert_eq!(admitted.endpoint, None);
        assert_eq!(
            admitted.expected_bucket_owner.as_deref(),
            Some("123456789012")
        );
        assert_eq!(admitted.checksum, UploadChecksum::Always);
        assert!(!admitted.path_style);

        // Every partition is admitted; its credentials decide what it reaches.
        for region in ["us-gov-west-1", "cn-north-1", "eusc-de-east-1"] {
            let admitted = admit(
                &Provider::AmazonS3 {
                    region: region.to_owned(),
                    expected_bucket_owner: "123456789012".to_owned(),
                },
                "evidence-bucket",
            );
            assert_eq!(admitted.unwrap().region, region);
        }
        for region in ["", "EU-central-1", "eu central 1", "eu-central-1/"] {
            let refused = admit(
                &Provider::AmazonS3 {
                    region: region.to_owned(),
                    expected_bucket_owner: "123456789012".to_owned(),
                },
                "evidence-bucket",
            );
            assert_eq!(refused.unwrap_err(), ConfigError::Region, "{region:?}");
        }
        for owner in ["", "12345678901", "1234567890123", "12345678901a"] {
            let refused = admit(
                &Provider::AmazonS3 {
                    region: "us-east-1".to_owned(),
                    expected_bucket_owner: owner.to_owned(),
                },
                "evidence-bucket",
            );
            assert_eq!(
                refused.unwrap_err(),
                ConfigError::ExpectedBucketOwner,
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
            let admitted = admit(
                &Provider::CloudflareR2 {
                    endpoint: endpoint.to_owned(),
                },
                "evidence-bucket",
            )
            .unwrap();
            assert_eq!(admitted.region, "auto");
            assert_eq!(admitted.expected_bucket_owner, None);
            assert_eq!(admitted.checksum, UploadChecksum::BytesOnly);
            assert!(!admitted.endpoint.unwrap().ends_with('/'));
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
            let refused = admit(
                &Provider::CloudflareR2 {
                    endpoint: endpoint.to_owned(),
                },
                "evidence-bucket",
            );
            assert_eq!(refused.unwrap_err(), ConfigError::Endpoint, "{endpoint:?}");
        }
    }

    #[test]
    fn railway_takes_its_bucket_endpoint_and_defaults_the_region() {
        let admitted = admit(
            &Provider::Railway {
                endpoint: "https://t3.storageapi.dev".to_owned(),
                region: String::new(),
            },
            "evidence-bucket-jdhhd8oe18xi",
        )
        .unwrap();
        assert_eq!(
            admitted.endpoint.as_deref(),
            Some("https://t3.storageapi.dev")
        );
        assert_eq!(admitted.region, "auto");
        assert_eq!(admitted.checksum, UploadChecksum::Never);
        assert!(!admitted.path_style);
        for endpoint in [
            "http://t3.storageapi.dev",
            "https://t3.storageapi.dev/x",
            "t3.storageapi.dev",
        ] {
            let refused = admit(
                &Provider::Railway {
                    endpoint: endpoint.to_owned(),
                    region: "auto".to_owned(),
                },
                "evidence-bucket",
            );
            assert_eq!(refused.unwrap_err(), ConfigError::Endpoint, "{endpoint:?}");
        }
        let refused = admit(
            &Provider::Railway {
                endpoint: "https://t3.storageapi.dev".to_owned(),
                region: "AMS 1".to_owned(),
            },
            "evidence-bucket",
        );
        assert_eq!(refused.unwrap_err(), ConfigError::Region);
    }

    #[test]
    fn local_allows_plaintext_and_path_style() {
        let admitted = admit(
            &Provider::Local {
                endpoint: "http://127.0.0.1:7070".to_owned(),
                region: String::new(),
            },
            "evidence-bucket",
        )
        .unwrap();
        assert_eq!(admitted.endpoint.as_deref(), Some("http://127.0.0.1:7070"));
        assert_eq!(admitted.region, "us-east-1");
        assert!(admitted.path_style);
    }

    #[test]
    fn a_generic_provider_takes_an_https_origin_and_the_common_subset() {
        let admitted = admit(
            &Provider::S3Compatible {
                endpoint: "https://s3.eu-central-003.backblazeb2.com".to_owned(),
                region: "eu-central-003".to_owned(),
                path_style: false,
            },
            "evidence-bucket",
        )
        .unwrap();
        assert_eq!(
            admitted.endpoint.as_deref(),
            Some("https://s3.eu-central-003.backblazeb2.com")
        );
        assert_eq!(admitted.region, "eu-central-003");
        assert!(!admitted.path_style);
        assert_eq!(admitted.expected_bucket_owner, None);
        assert_eq!(admitted.checksum, UploadChecksum::Never);

        // A self-hosted store: its own port, path-style, and the default region.
        let admitted = admit(
            &Provider::S3Compatible {
                endpoint: "https://ceph.internal.example:8443".to_owned(),
                region: String::new(),
                path_style: true,
            },
            "evidence-bucket",
        )
        .unwrap();
        assert_eq!(
            admitted.endpoint.as_deref(),
            Some("https://ceph.internal.example:8443")
        );
        assert_eq!(admitted.region, "us-east-1");
        assert!(admitted.path_style);

        for endpoint in [
            "http://ceph.internal.example",
            "https://ceph.internal.example/bucket",
            "https://user@ceph.internal.example",
            "",
        ] {
            let refused = admit(
                &Provider::S3Compatible {
                    endpoint: endpoint.to_owned(),
                    region: String::new(),
                    path_style: false,
                },
                "evidence-bucket",
            );
            assert_eq!(refused.unwrap_err(), ConfigError::Endpoint, "{endpoint:?}");
        }
    }

    #[test]
    fn buckets_are_dotless_dns_names() {
        let provider = Provider::Local {
            endpoint: "http://127.0.0.1:7070".to_owned(),
            region: String::new(),
        };
        for bucket in [
            "abc",
            "pricing-evidence",
            "document-processing-results",
            &"a".repeat(63),
        ] {
            admit(&provider, bucket).unwrap_or_else(|_| panic!("{bucket:?}"));
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
                admit(&provider, bucket).unwrap_err(),
                ConfigError::Bucket,
                "{bucket:?}"
            );
        }
    }
}
