#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a broken fixture or setup fails the test"
)]

use std::time::Duration;

use bytes::Bytes;
use infra_object_storage::{
    ContentType, CredentialSource, MutationError, MutationFailure, ObjectKey, ObjectStorage,
    ObjectStorageError, ObjectStorageOptions, Provider, PutOptions,
};
use operation_context::OperationContext;
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
            "s3_compatible" => Provider::S3Compatible {
                endpoint: var("ENDPOINT"),
                region: var("REGION"),
                path_style: var("PATH_STYLE") == "true",
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
        ObjectStorage::try_new(ObjectStorageOptions {
            provider: self.provider.clone(),
            bucket: self.bucket.clone(),
            credentials: CredentialSource::AccessKey {
                access_key_id: self.access_key_id.clone(),
                secret_access_key: self.secret_access_key.clone(),
            },
            max_object_bytes: 8 * 1024 * 1024,
            max_concurrency: 4,
            operation_timeout: Duration::from_secs(15),
        })
        .expect("the configured provider tuple must be admitted")
    }

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
            .request_checksum_calculation(
                aws_sdk_s3::config::RequestChecksumCalculation::WhenSupported,
            )
            .response_checksum_validation(
                aws_sdk_s3::config::ResponseChecksumValidation::WhenRequired,
            );
        config = match &self.provider {
            Provider::AmazonS3 { region, .. } => {
                let region = region.trim();
                config.region(aws_sdk_s3::config::Region::new(region.to_owned()))
            }
            Provider::CloudflareR2 { endpoint } => config
                .region(aws_sdk_s3::config::Region::new("auto"))
                .endpoint_url(endpoint),
            Provider::Railway { endpoint, region } => {
                let region = region.trim();
                config
                    .region(aws_sdk_s3::config::Region::new(if region.is_empty() {
                        "auto".to_owned()
                    } else {
                        region.to_owned()
                    }))
                    .endpoint_url(endpoint)
            }
            Provider::S3Compatible {
                endpoint,
                region,
                path_style,
            } => {
                let region = region.trim();
                config
                    .region(aws_sdk_s3::config::Region::new(if region.is_empty() {
                        "us-east-1".to_owned()
                    } else {
                        region.to_owned()
                    }))
                    .force_path_style(*path_style)
                    .endpoint_url(endpoint)
            }
            Provider::Local { .. } => unreachable!("Target::from_env rejects local"),
        };
        aws_sdk_s3::Client::from_conf(config.build())
    }
}

#[expect(
    clippy::print_stdout,
    reason = "conformance fixture emits its finite observation receipt on test stdout"
)]
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
    let context = OperationContext::unbounded();
    let target = Target::from_env();
    let storage = target.storage();
    let run = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let key = |name: &str| ObjectKey::try_new(format!("conformance/{run}/{name}")).unwrap();
    fact("provider", target.provider.name());

    let object = key("create-only.json");
    let mut written = vec![object.clone()];
    contract(&context, &storage, &object).await;
    written.extend(record_facts(&target, &object, key).await);

    for written in &written {
        storage.delete(&context, written).await.unwrap();
    }
    assert_eq!(
        storage.head(&context, &object).await,
        Err(ObjectStorageError::NotFound)
    );
    storage.delete(&context, &object).await.unwrap();
}

async fn contract(context: &OperationContext, storage: &ObjectStorage, object: &ObjectKey) {
    let body = Bytes::from_static(br#"{"conformance":true}"#);
    let options =
        PutOptions::create_only().content_type(ContentType::try_new("application/json").unwrap());
    storage
        .put(context, object, body.clone().into(), options.clone())
        .await
        .unwrap();
    assert_eq!(
        storage
            .put(context, object, body.clone().into(), options)
            .await,
        Err(MutationError::NotApplied(MutationFailure::AlreadyExists)),
        "If-None-Match: * must answer 412 on an existing key"
    );
    let metadata = storage.head(context, object).await.unwrap();
    assert_eq!(metadata.size, body.len() as u64);
    let downloaded = storage
        .get(context, object)
        .await
        .unwrap()
        .into_bytes(body.len())
        .await
        .unwrap();
    let matches_upload = downloaded == body;
    assert!(matches_upload, "download differs from the written object");

    let http = reqwest::Client::new();
    let url = storage
        .presign_get(context, object, Duration::from_mins(1))
        .await
        .unwrap();
    let response = http
        .get(url.expose())
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .unwrap();
    assert_eq!(response.status(), 200);
    let short = storage
        .presign_get(context, object, Duration::from_secs(1))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let expired = http
        .get(short.expose())
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .unwrap()
        .status();
    assert!(
        expired == 403 || expired == 400,
        "expired presign answered {expired}"
    );
}

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
