//! The client against a real S3 implementation: versitygw from
//! `env/docker-compose.yml`. Run through `make test-integration-object-storage`,
//! which starts the emulator and sets `OBJECT_STORAGE_TEST_ENDPOINT`.
//!
//! This proves signing, the wire protocol, create-only, CRC64NVME in a
//! header and in an `aws-chunked` trailer, response validation, and presigned
//! expiry. Provider-specific behavior needs the live conformance test.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use bytes::Bytes;
use health::Probe;
use infra_object_storage::{
    ContentType, ObjectKey, ObjectStorage, ObjectStorageError, ObjectStorageOptions, Provider,
    PutBody, PutOptions,
};
use secrecy::SecretString;

const BUCKET: &str = "template-emulator";

struct Emulator {
    endpoint: String,
    access_key_id: String,
    secret_access_key: String,
}

impl Emulator {
    fn from_env() -> Self {
        let endpoint = std::env::var("OBJECT_STORAGE_TEST_ENDPOINT").unwrap_or_else(|_| {
            panic!("OBJECT_STORAGE_TEST_ENDPOINT is unset; run scripts/ci/test-integration-object-storage.sh")
        });
        Self {
            endpoint,
            access_key_id: std::env::var("OBJECT_STORAGE_TEST_ACCESS_KEY_ID")
                .unwrap_or_else(|_| "template".to_owned()),
            secret_access_key: std::env::var("OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY")
                .unwrap_or_else(|_| "template-secret".to_owned()),
        }
    }

    fn storage(&self, secret: &str) -> ObjectStorage {
        ObjectStorage::new(ObjectStorageOptions {
            provider: Provider::Local {
                endpoint: self.endpoint.clone(),
                region: String::new(),
            },
            bucket: BUCKET.to_owned(),
            access_key_id: self.access_key_id.clone(),
            secret_access_key: SecretString::from(secret.to_owned()),
            max_object_bytes: 1024 * 1024,
            max_concurrency: 8,
            operation_timeout: Duration::from_secs(10),
        })
        .unwrap()
    }

    /// Create the bucket with the raw SDK: the client itself never manages buckets.
    async fn ensure_bucket(&self) {
        let config = aws_sdk_s3::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::v2026_01_12())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .endpoint_url(&self.endpoint)
            .force_path_style(true)
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                &self.access_key_id,
                &self.secret_access_key,
                None,
                None,
                "emulator",
            ))
            .build();
        let client = aws_sdk_s3::Client::from_conf(config);
        if let Err(error) = client.create_bucket().bucket(BUCKET).send().await {
            let code = aws_sdk_s3::error::ProvideErrorMetadata::code(&error);
            assert!(
                matches!(
                    code,
                    Some("BucketAlreadyOwnedByYou" | "BucketAlreadyExists")
                ),
                "create bucket: {code:?}"
            );
        }
    }
}

fn unique_key(name: &str) -> ObjectKey {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    ObjectKey::new(format!("emulator/{nanos}/{name}")).unwrap()
}

#[tokio::test]
async fn round_trip_create_only_and_delete() {
    let emulator = Emulator::from_env();
    emulator.ensure_bucket().await;
    let storage = emulator.storage(&emulator.secret_access_key);
    let key = unique_key("round-trip.json");
    let body = Bytes::from_static(br#"{"price":"1.25"}"#);
    let options = PutOptions::default()
        .content_type(ContentType::new("application/json").unwrap())
        .create_only();

    storage
        .put(&key, body.clone().into(), options.clone())
        .await
        .unwrap();
    assert_eq!(
        storage.put(&key, body.clone().into(), options).await,
        Err(ObjectStorageError::AlreadyExists)
    );

    let metadata = storage.head(&key).await.unwrap();
    assert_eq!(metadata.size, body.len() as u64);
    assert_eq!(metadata.content_type.as_deref(), Some("application/json"));

    let download = storage.get(&key).await.unwrap();
    assert_eq!(download.bytes().await.unwrap(), body);

    storage.delete(&key).await.unwrap();
    assert_eq!(storage.head(&key).await, Err(ObjectStorageError::NotFound));
    assert_eq!(
        storage.get(&key).await.err(),
        Some(ObjectStorageError::NotFound)
    );
    storage.delete(&key).await.unwrap();
}

#[tokio::test]
async fn streamed_upload_uses_a_trailing_checksum() {
    let emulator = Emulator::from_env();
    emulator.ensure_bucket().await;
    let storage = emulator.storage(&emulator.secret_access_key);
    let key = unique_key("streamed.bin");
    let payload = Bytes::from(vec![7_u8; 256 * 1024]);
    let body = http_body_util::Full::new(payload.clone());
    storage
        .put(
            &key,
            PutBody::stream(payload.len() as u64, body),
            PutOptions::default(),
        )
        .await
        .unwrap();
    let mut download = storage.get(&key).await.unwrap();
    let mut received = Vec::new();
    while let Some(chunk) = download.next_chunk().await.unwrap() {
        received.extend_from_slice(&chunk);
    }
    assert_eq!(received, payload);
    storage.delete(&key).await.unwrap();
}

#[tokio::test]
async fn presigned_get_works_until_it_expires() {
    let emulator = Emulator::from_env();
    emulator.ensure_bucket().await;
    let storage = emulator.storage(&emulator.secret_access_key);
    let key = unique_key("presigned.txt");
    storage
        .put(
            &key,
            Bytes::from_static(b"presigned").into(),
            PutOptions::default(),
        )
        .await
        .unwrap();
    let http = reqwest::Client::new();

    let url = storage
        .presign_get(&key, Duration::from_secs(60))
        .await
        .unwrap();
    let response = http.get(url.expose()).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.bytes().await.unwrap(),
        Bytes::from_static(b"presigned")
    );

    let short = storage
        .presign_get(&key, Duration::from_secs(1))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let expired = http.get(short.expose()).send().await.unwrap();
    assert_eq!(expired.status(), 403);
    storage.delete(&key).await.unwrap();
}

#[tokio::test]
async fn wrong_credentials_are_rejected_and_the_probe_passes() {
    let emulator = Emulator::from_env();
    emulator.ensure_bucket().await;
    emulator
        .storage(&emulator.secret_access_key)
        .probe()
        .check()
        .await
        .unwrap();

    let storage = emulator.storage("not-the-secret");
    let key = unique_key("denied.txt");
    assert_eq!(storage.head(&key).await, Err(ObjectStorageError::Rejected));
    assert_eq!(
        storage
            .put(&key, Bytes::from_static(b"x").into(), PutOptions::default())
            .await,
        Err(ObjectStorageError::Rejected)
    );
}
