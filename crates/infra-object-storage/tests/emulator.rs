#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a broken fixture or setup fails the test"
)]

use std::time::Duration;

use bytes::Bytes;
use infra_object_storage::{
    ContentType, CredentialSource, MutationError, MutationFailure, ObjectKey, ObjectStorage,
    ObjectStorageError, ObjectStorageOptions, Provider, PutBody, PutOptions,
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
            panic!(
                "OBJECT_STORAGE_TEST_ENDPOINT is unset; run scripts/integration.sh object-storage"
            )
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
    let context = OperationContext::unbounded();
    let emulator = Emulator::from_env();
    let storage = emulator.storage(&emulator.secret_access_key);
    let key = unique_key("round-trip.json");
    let body = Bytes::from_static(br#"{"price":"1.25"}"#);
    let options =
        PutOptions::create_only().content_type(ContentType::try_new("application/json").unwrap());

    storage
        .put(&context, &key, body.clone().into(), options.clone())
        .await
        .unwrap();
    let replacement = Bytes::from_static(br#"{"price":"2.50"}"#);
    assert_eq!(
        storage
            .put(&context, &key, replacement.clone().into(), options)
            .await,
        Err(MutationError::NotApplied(MutationFailure::AlreadyExists))
    );

    let metadata = storage.head(&context, &key).await.unwrap();
    assert_eq!(metadata.size, body.len() as u64);
    assert_eq!(metadata.content_type.as_deref(), Some("application/json"));

    let download = storage.get(&context, &key).await.unwrap();
    assert_eq!(download.into_bytes(body.len()).await.unwrap(), body);

    storage
        .put(
            &context,
            &key,
            replacement.clone().into(),
            PutOptions::overwrite(),
        )
        .await
        .unwrap();
    let download = storage.get(&context, &key).await.unwrap();
    assert_eq!(
        download.into_bytes(replacement.len()).await.unwrap(),
        replacement
    );

    storage.delete(&context, &key).await.unwrap();
    assert_eq!(
        storage.head(&context, &key).await,
        Err(ObjectStorageError::NotFound)
    );
    assert_eq!(
        storage.get(&context, &key).await.err(),
        Some(ObjectStorageError::NotFound)
    );
    storage.delete(&context, &key).await.unwrap();
}

#[tokio::test]
async fn streamed_upload_round_trips() {
    let context = OperationContext::unbounded();
    let emulator = Emulator::from_env();
    let storage = emulator.storage(&emulator.secret_access_key);
    let key = unique_key("streamed.bin");
    let payload = Bytes::from(vec![7_u8; 256 * 1024]);
    let body = http_body_util::Full::new(payload.clone());
    storage
        .put(
            &context,
            &key,
            PutBody::stream(payload.len() as u64, body),
            PutOptions::overwrite(),
        )
        .await
        .unwrap();
    let mut download = storage.get(&context, &key).await.unwrap();
    let mut received = Vec::new();
    while let Some(chunk) = download.next_chunk().await.unwrap() {
        received.extend_from_slice(&chunk);
    }
    assert_eq!(received, payload);
    storage.delete(&context, &key).await.unwrap();
}

#[tokio::test]
async fn presigned_get_works_until_it_expires() {
    let context = OperationContext::unbounded();
    let emulator = Emulator::from_env();
    let storage = emulator.storage(&emulator.secret_access_key);
    let key = unique_key("presigned.txt");
    storage
        .put(
            &context,
            &key,
            Bytes::from_static(b"presigned").into(),
            PutOptions::overwrite(),
        )
        .await
        .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();

    let url = storage
        .presign_get(&context, &key, Duration::from_mins(1))
        .await
        .unwrap();
    let response = http
        .get(url.expose())
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .bytes()
            .await
            .map_err(reqwest::Error::without_url)
            .unwrap(),
        Bytes::from_static(b"presigned")
    );

    let short = storage
        .presign_get(&context, &key, Duration::from_secs(1))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let expired = http
        .get(short.expose())
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .unwrap();
    assert_eq!(expired.status(), 403);
    storage.delete(&context, &key).await.unwrap();
}

#[tokio::test]
async fn wrong_credentials_are_rejected_while_valid_credentials_succeed() {
    let context = OperationContext::unbounded();
    let emulator = Emulator::from_env();
    let key = unique_key("denied.txt");
    assert_eq!(
        emulator
            .storage(&emulator.secret_access_key)
            .head(&context, &key)
            .await,
        Err(ObjectStorageError::NotFound)
    );

    let storage = emulator.storage("not-the-secret");
    assert_eq!(
        storage.head(&context, &key).await,
        Err(ObjectStorageError::Rejected)
    );
    assert_eq!(
        storage
            .put(
                &context,
                &key,
                Bytes::from_static(b"x").into(),
                PutOptions::overwrite(),
            )
            .await,
        Err(MutationError::NotApplied(MutationFailure::Rejected))
    );
}
