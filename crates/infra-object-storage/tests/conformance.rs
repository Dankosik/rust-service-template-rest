//! Live provider conformance. It writes to a real bucket, so it is ignored
//! and runs only through `make test-object-storage-conformance
//! PROVIDER=<provider>` with `OBJECT_STORAGE_CONFORMANCE_WRITES=allow` and the
//! service's own `APP__OBJECT_STORAGE__*` variables for that bucket.
//!
//! Every key lives under `conformance/<run>/` and is deleted at the end. The
//! test asserts the client contract and prints what the provider does beyond
//! it (checksum echo, trailer acceptance, expected-owner handling), so a
//! later change can enable a provider feature from recorded evidence. One
//! provider's result never qualifies another.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use std::time::Duration;

use bytes::Bytes;
use health::Probe;
use infra_object_storage::{
    ContentType, ObjectKey, ObjectStorage, ObjectStorageError, ObjectStorageOptions, Provider,
    PutOptions,
};
use secrecy::{ExposeSecret, SecretString};

fn var(name: &str) -> String {
    std::env::var(format!("APP__OBJECT_STORAGE__{name}")).unwrap_or_default()
}

struct Target {
    provider: Provider,
    bucket: String,
    access_key_id: String,
    secret_access_key: SecretString,
}

impl Target {
    fn from_env() -> Self {
        assert_eq!(
            std::env::var("OBJECT_STORAGE_CONFORMANCE_WRITES").as_deref(),
            Ok("allow"),
            "live conformance writes to a real bucket: set OBJECT_STORAGE_CONFORMANCE_WRITES=allow"
        );
        let expected = std::env::var("OBJECT_STORAGE_CONFORMANCE_PROVIDER").unwrap_or_default();
        let provider = match var("PROVIDER").as_str() {
            "amazon_s3" => Provider::AmazonS3 {
                region: var("REGION"),
                expected_bucket_owner: var("EXPECTED_BUCKET_OWNER"),
            },
            "cloudflare_r2" => Provider::CloudflareR2 {
                endpoint: var("ENDPOINT"),
            },
            "railway" => Provider::Railway {
                endpoint: var("ENDPOINT"),
                region: var("REGION"),
            },
            other => panic!("APP__OBJECT_STORAGE__PROVIDER {other:?} is not a live provider"),
        };
        assert_eq!(
            provider.name(),
            expected,
            "PROVIDER must match the configured provider"
        );
        Self {
            provider,
            bucket: var("BUCKET"),
            access_key_id: var("ACCESS_KEY_ID"),
            secret_access_key: SecretString::from(var("SECRET_ACCESS_KEY")),
        }
    }

    fn storage(&self) -> ObjectStorage {
        ObjectStorage::new(ObjectStorageOptions {
            provider: self.provider.clone(),
            bucket: self.bucket.clone(),
            access_key_id: self.access_key_id.clone(),
            secret_access_key: self.secret_access_key.clone(),
            max_object_bytes: 8 * 1024 * 1024,
            max_concurrency: 4,
            operation_timeout: Duration::from_secs(15),
        })
        .expect("the configured provider tuple must be admitted")
    }

    /// A raw SDK client with the same identity, for recording provider facts
    /// the client deliberately does not depend on.
    fn raw(&self) -> aws_sdk_s3::Client {
        let mut config = aws_sdk_s3::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::v2026_01_12())
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                &self.access_key_id,
                self.secret_access_key.expose_secret(),
                None,
                None,
                "conformance",
            ))
            // Compute a checksum whenever a request names one.
            .request_checksum_calculation(
                aws_sdk_s3::config::RequestChecksumCalculation::WhenSupported,
            )
            .response_checksum_validation(
                aws_sdk_s3::config::ResponseChecksumValidation::WhenRequired,
            );
        config = match &self.provider {
            Provider::AmazonS3 { region, .. } => {
                config.region(aws_sdk_s3::config::Region::new(region.clone()))
            }
            Provider::CloudflareR2 { endpoint } => config
                .region(aws_sdk_s3::config::Region::new("auto"))
                .endpoint_url(endpoint),
            Provider::Railway { endpoint, region } | Provider::Local { endpoint, region } => config
                .region(aws_sdk_s3::config::Region::new(if region.is_empty() {
                    "auto".to_owned()
                } else {
                    region.clone()
                }))
                .endpoint_url(endpoint),
        };
        aws_sdk_s3::Client::from_conf(config.build())
    }
}

fn fact(name: &str, value: impl std::fmt::Display) {
    println!("conformance fact: {name} = {value}");
}

fn outcome<T, E: aws_sdk_s3::error::ProvideErrorMetadata>(result: &Result<T, E>) -> String {
    match result {
        Ok(_) => "accepted".to_owned(),
        Err(error) => format!("rejected ({})", error.code().unwrap_or("no code")),
    }
}

#[tokio::test]
#[ignore = "writes to a live provider bucket; requires separate authorization"]
async fn live_provider_conformance() {
    let target = Target::from_env();
    let storage = target.storage();
    let run = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let key = |name: &str| ObjectKey::new(format!("conformance/{run}/{name}")).unwrap();
    fact("provider", target.provider.name());

    let object = key("create-only.json");
    let mut written = vec![object.clone()];
    contract(&storage, &object).await;
    written.extend(record_facts(&target, &object, key).await);

    // Contract: delete, then not found; delete of a missing key succeeds.
    for written in &written {
        storage.delete(written).await.unwrap();
    }
    assert_eq!(
        storage.head(&object).await,
        Err(ObjectStorageError::NotFound)
    );
    storage.delete(&object).await.unwrap();
}

/// What every provider must do for the client's contract to hold.
async fn contract(storage: &ObjectStorage, object: &ObjectKey) {
    let body = Bytes::from_static(br#"{"conformance":true}"#);
    let options = PutOptions::default()
        .content_type(ContentType::new("application/json").unwrap())
        .create_only();
    storage
        .put(object, body.clone().into(), options.clone())
        .await
        .unwrap();
    assert_eq!(
        storage.put(object, body.clone().into(), options).await,
        Err(ObjectStorageError::AlreadyExists),
        "If-None-Match: * must answer 412 on an existing key"
    );
    let metadata = storage.head(object).await.unwrap();
    assert_eq!(metadata.size, body.len() as u64);
    assert_eq!(
        storage.get(object).await.unwrap().bytes().await.unwrap(),
        body
    );
    storage.probe().check().await.unwrap();

    let http = reqwest::Client::new();
    let url = storage
        .presign_get(object, Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(http.get(url.expose()).send().await.unwrap().status(), 200);
    let short = storage
        .presign_get(object, Duration::from_secs(1))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let expired = http.get(short.expose()).send().await.unwrap().status();
    assert!(
        expired == 403 || expired == 400,
        "expired presign answered {expired}"
    );
}

/// Provider behavior the client does not rely on everywhere, printed so a
/// later change can enable it from evidence. Returns the keys it wrote.
async fn record_facts(
    target: &Target,
    object: &ObjectKey,
    key: impl Fn(&str) -> ObjectKey,
) -> Vec<ObjectKey> {
    let raw = target.raw();
    let header = key("crc64nvme-header.bin");
    let result = raw
        .put_object()
        .bucket(&target.bucket)
        .key(header.as_str())
        .checksum_algorithm(aws_sdk_s3::types::ChecksumAlgorithm::Crc64Nvme)
        .body(Bytes::from_static(b"header checksum").into())
        .send()
        .await;
    fact("put_crc64nvme_header", outcome(&result));

    let trailer = key("crc64nvme-trailer.bin");
    let result = raw
        .put_object()
        .bucket(&target.bucket)
        .key(trailer.as_str())
        .checksum_algorithm(aws_sdk_s3::types::ChecksumAlgorithm::Crc64Nvme)
        .content_length(16)
        .body(aws_sdk_s3::primitives::ByteStream::from_body_1_x(
            http_body_util::Full::new(Bytes::from_static(b"trailer checksum")),
        ))
        .send()
        .await;
    fact("put_crc64nvme_trailer", outcome(&result));

    let echoed = raw
        .get_object()
        .bucket(&target.bucket)
        .key(header.as_str())
        .checksum_mode(aws_sdk_s3::types::ChecksumMode::Enabled)
        .send()
        .await;
    fact(
        "get_checksum_echo",
        match &echoed {
            Ok(output) => format!(
                "crc64nvme={} type={:?}",
                output.checksum_crc64_nvme().is_some(),
                output.checksum_type()
            ),
            Err(_) => outcome(&echoed),
        },
    );

    // An owner that is not this account: S3 answers 403; others may ignore it.
    let result = raw
        .head_object()
        .bucket(&target.bucket)
        .key(object.as_str())
        .expected_bucket_owner("000000000000")
        .send()
        .await;
    fact("expected_bucket_owner_mismatch", outcome(&result));
    vec![header, trailer]
}
