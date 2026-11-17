#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a broken fixture or setup fails the test"
)]

use std::time::Duration;

use bytes::Bytes;
use infra_object_storage::{
    ContentType, CredentialSource, ObjectKey, ObjectStorage, ObjectStorageError,
    ObjectStorageOptions, Provider, PutBody, PutOptions,
};
use operation_context::OperationContext;
use secrecy::SecretString;

const BUCKET: &str = "template-bucket";

struct Emulator {
    endpoint: String,
    access_key_id: String,
    secret_access_key: String,
}

impl Emulator {
    fn from_env() -> Self {
        let endpoint = std::env::var("OBJECT_STORAGE_TEST_ENDPOINT").unwrap_or_else(|_| {
            panic!("OBJECT_STORAGE_TEST_ENDPOINT is unset; run scripts/profiles/integration.sh object-storage")
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
        ObjectStorage::try_new(ObjectStorageOptions {
            provider: Provider::Local {
                endpoint: self.endpoint.clone(),
                region: String::new(),
            },
            bucket: BUCKET.to_owned(),
            credentials: CredentialSource::AccessKey {
                access_key_id: self.access_key_id.clone(),
                secret_access_key: SecretString::from(secret.to_owned()),
            },
            max_object_bytes: 1024 * 1024,
            max_concurrency: 8,
            operation_timeout: Duration::from_secs(10),
        })
        .unwrap()
    }
}

fn unique_key(name: &str) -> ObjectKey {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    ObjectKey::try_new(format!("emulator/{nanos}/{name}")).unwrap()
}

#[tokio::test]
async fn round_trip_create_only_and_delete() {
    let parent = OperationContext::unbounded();
    let emulator = Emulator::from_env();
    let storage = emulator.storage(&emulator.secret_access_key);
    let key = unique_key("round-trip.json");
    let body = Bytes::from_static(br#"{"price":"1.25"}"#);
    let options = PutOptions::default()
        .content_type(ContentType::try_new("application/json").unwrap())
        .create_only();

    storage
        .put(&parent, &key, body.clone().into(), options.clone())
        .await
        .unwrap();
    assert_eq!(
        storage
            .put(&parent, &key, body.clone().into(), options)
            .await,
        Err(ObjectStorageError::AlreadyExists)
    );

    let metadata = storage.head(&parent, &key).await.unwrap();
    assert_eq!(metadata.size, body.len() as u64);
    assert_eq!(metadata.content_type.as_deref(), Some("application/json"));

    let download = storage.get(&parent, &key).await.unwrap();
    assert_eq!(download.into_bytes().await.unwrap(), body);

    storage.delete(&parent, &key).await.unwrap();
    assert_eq!(
        storage.head(&parent, &key).await,
        Err(ObjectStorageError::NotFound)
    );
    assert_eq!(
        storage.get(&parent, &key).await.err(),
        Some(ObjectStorageError::NotFound)
    );
    storage.delete(&parent, &key).await.unwrap();
}

#[tokio::test]
async fn streamed_upload_round_trips() {
    let parent = OperationContext::unbounded();
    let emulator = Emulator::from_env();
    let storage = emulator.storage(&emulator.secret_access_key);
    let key = unique_key("streamed.bin");
    let payload = Bytes::from(vec![7_u8; 256 * 1024]);
    let body = http_body_util::Full::new(payload.clone());
    storage
        .put(
            &parent,
            &key,
            PutBody::stream(payload.len() as u64, body),
            PutOptions::default(),
        )
        .await
        .unwrap();
    let mut download = storage.get(&parent, &key).await.unwrap();
    let mut received = Vec::new();
    while let Some(chunk) = download.next_chunk().await.unwrap() {
        received.extend_from_slice(&chunk);
    }
    assert_eq!(received, payload);
    storage.delete(&parent, &key).await.unwrap();
}

#[tokio::test]
async fn presigned_get_works_until_it_expires() {
    let parent = OperationContext::unbounded();
    let emulator = Emulator::from_env();
    let storage = emulator.storage(&emulator.secret_access_key);
    let key = unique_key("presigned.txt");
    storage
        .put(
            &parent,
            &key,
            Bytes::from_static(b"presigned").into(),
            PutOptions::default(),
        )
        .await
        .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();

    let url = storage
        .presign_get(&parent, &key, Duration::from_mins(1))
        .await
        .unwrap();
    let response = http.get(url.expose()).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.bytes().await.unwrap(),
        Bytes::from_static(b"presigned")
    );

    let short = storage
        .presign_get(&parent, &key, Duration::from_secs(1))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let expired = http.get(short.expose()).send().await.unwrap();
    assert_eq!(expired.status(), 403);
    storage.delete(&parent, &key).await.unwrap();
}

#[tokio::test]
async fn wrong_credentials_are_rejected_while_valid_credentials_succeed() {
    let parent = OperationContext::unbounded();
    let emulator = Emulator::from_env();
    let key = unique_key("denied.txt");
    assert_eq!(
        emulator
            .storage(&emulator.secret_access_key)
            .head(&parent, &key)
            .await,
        Err(ObjectStorageError::NotFound)
    );

    let storage = emulator.storage("not-the-secret");
    assert_eq!(
        storage.head(&parent, &key).await,
        Err(ObjectStorageError::Rejected)
    );
    assert_eq!(
        storage
            .put(
                &parent,
                &key,
                Bytes::from_static(b"x").into(),
                PutOptions::default()
            )
            .await,
        Err(ObjectStorageError::Rejected)
    );
}
