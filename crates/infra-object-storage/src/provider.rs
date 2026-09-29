//! Provider admission: endpoint, region, addressing, and which optional S3
//! features each provider receives.

use url::Url;

/// Where the bucket lives. Each provider carries only the fields it accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provider {
    /// Amazon S3. The SDK resolves the regional endpoint.
    AmazonS3 {
        /// Commercial region, for example `eu-central-1`.
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
            Self::Local { .. } => "local",
        }
    }
}

/// Why the options were refused. Display names the key, never its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// Not a dotless DNS bucket name, or an Amazon-reserved one.
    #[error("object_storage.bucket is not a valid dotless bucket name")]
    Bucket,
    /// Not a region this provider accepts.
    #[error("object_storage.region is not valid for the provider")]
    Region,
    /// Missing, malformed, or not the origin shape this provider requires.
    #[error("object_storage.endpoint is not valid for the provider")]
    Endpoint,
    /// Not a 12-digit account id.
    #[error("object_storage.expected_bucket_owner must be a 12-digit account id")]
    ExpectedBucketOwner,
    /// The access key id is empty.
    #[error("object_storage.access_key_id is required")]
    AccessKeyId,
    /// The secret access key is empty.
    #[error("object_storage.secret_access_key is required")]
    SecretAccessKey,
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

/// The admitted provider tuple the SDK config is built from.
#[derive(Debug)]
pub(crate) struct Admitted {
    pub(crate) endpoint: Option<String>,
    pub(crate) region: String,
    pub(crate) path_style: bool,
    pub(crate) expected_bucket_owner: Option<String>,
    pub(crate) checksum: UploadChecksum,
}

pub(crate) fn admit(provider: &Provider, bucket: &str) -> Result<Admitted, ConfigError> {
    let amazon = matches!(provider, Provider::AmazonS3 { .. });
    if !valid_bucket(bucket) || (amazon && amazon_reserved(bucket)) {
        return Err(ConfigError::Bucket);
    }
    match provider {
        Provider::AmazonS3 {
            region,
            expected_bucket_owner,
        } => {
            if !commercial_region(region) {
                return Err(ConfigError::Region);
            }
            if expected_bucket_owner.len() != 12
                || !expected_bucket_owner
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())
            {
                return Err(ConfigError::ExpectedBucketOwner);
            }
            Ok(Admitted {
                endpoint: None,
                region: region.clone(),
                path_style: false,
                expected_bucket_owner: Some(expected_bucket_owner.clone()),
                checksum: UploadChecksum::Always,
            })
        }
        Provider::CloudflareR2 { endpoint } => {
            let origin = origin(endpoint, false)?;
            if !r2_host(origin.host_str().unwrap_or_default()) || origin.port().is_some() {
                return Err(ConfigError::Endpoint);
            }
            Ok(Admitted {
                endpoint: Some(serialize(&origin)),
                region: "auto".to_owned(),
                path_style: false,
                expected_bucket_owner: None,
                checksum: UploadChecksum::BytesOnly,
            })
        }
        Provider::Railway { endpoint, region } => {
            let origin = origin(endpoint, false)?;
            if origin.port().is_some() {
                return Err(ConfigError::Endpoint);
            }
            Ok(Admitted {
                endpoint: Some(serialize(&origin)),
                region: signing_region(region, "auto")?,
                path_style: false,
                expected_bucket_owner: None,
                checksum: UploadChecksum::Never,
            })
        }
        Provider::Local { endpoint, region } => Ok(Admitted {
            endpoint: Some(serialize(&origin(endpoint, true)?)),
            region: signing_region(region, "us-east-1")?,
            path_style: true,
            expected_bucket_owner: None,
            checksum: UploadChecksum::Always,
        }),
    }
}

/// A bare origin: scheme and host, an optional port, and nothing else.
fn origin(endpoint: &str, allow_plaintext: bool) -> Result<Url, ConfigError> {
    let url = Url::parse(endpoint).map_err(|_| ConfigError::Endpoint)?;
    let scheme_allowed = url.scheme() == "https" || (allow_plaintext && url.scheme() == "http");
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

fn signing_region(region: &str, default: &str) -> Result<String, ConfigError> {
    if region.is_empty() {
        return Ok(default.to_owned());
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

/// `^[a-z]{2}-[a-z]+-[0-9]+$`: commercial partitions only, so `us-gov-*`
/// and `cn-*` endpoints and their separate credentials are refused.
fn commercial_region(region: &str) -> bool {
    let mut parts = region.split('-');
    let (Some(area), Some(place), Some(number), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    area.len() == 2
        && area.bytes().all(|byte| byte.is_ascii_lowercase())
        && area != "cn"
        && !place.is_empty()
        && place.bytes().all(|byte| byte.is_ascii_lowercase())
        && place != "gov"
        && !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
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

/// Prefixes and suffixes Amazon S3 reserves for its own bucket kinds.
fn amazon_reserved(bucket: &str) -> bool {
    const RESERVED_PREFIXES: [&str; 3] = ["xn--", "sthree-", "amzn-s3-demo-"];
    const RESERVED_SUFFIXES: [&str; 4] = ["-s3alias", "--ol-s3", "--x-s3", "--table-s3"];
    RESERVED_PREFIXES
        .iter()
        .any(|prefix| bucket.starts_with(prefix))
        || RESERVED_SUFFIXES
            .iter()
            .any(|suffix| bucket.ends_with(suffix))
}
