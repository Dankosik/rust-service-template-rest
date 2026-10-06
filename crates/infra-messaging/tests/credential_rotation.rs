#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration assertions describe synthetic fixture failures"
)]

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::{ConnectErrorKind, ConnectOptions, jetstream};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use domain_events::{Event, EventPayload};
use health::Probe as _;
use infra_messaging::{CloseOutcome, Messaging, MessagingOptions, Registry, Route};
use nkeys::KeyPair;
use serde_json::json;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

const ACCOUNT_SEED: &str = include_str!("fixtures/synthetic-account.seed");

struct User {
    credentials: String,
    expires: u64,
}

impl User {
    fn expiring_after(seconds: u64) -> Self {
        let account = KeyPair::from_seed(ACCOUNT_SEED.trim()).unwrap();
        let user = KeyPair::new_user();
        let issued = unix_now();
        let expires = issued + seconds;
        let claims = json!({
            "iss": account.public_key(), "sub": user.public_key(),
            "iat": issued, "exp": expires,
            "nats": { "type": "user", "version": 2, "bearer_token": false,
                "subs": -1, "data": -1, "payload": -1 }
        });
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ed25519-nkey"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let signed = format!("{header}.{payload}");
        let signature = URL_SAFE_NO_PAD.encode(account.sign(signed.as_bytes()).unwrap());
        let jwt = format!("{signed}.{signature}");
        Self {
            credentials: format!(
                "-----BEGIN NATS USER JWT-----\n{jwt}\n------END NATS USER JWT------\n\n\
                 -----BEGIN USER NKEY SEED-----\n{}\n------END USER NKEY SEED------\n",
                user.seed().unwrap()
            ),
            expires,
        }
    }

    async fn publish_file(&self, path: &Path) {
        let replacement = path.with_extension("next");
        tokio::fs::write(&replacement, &self.credentials)
            .await
            .unwrap();
        tokio::fs::rename(replacement, path).await.unwrap();
    }

    fn options(&self) -> ConnectOptions {
        ConnectOptions::new()
            .credentials(&self.credentials)
            .unwrap()
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[derive(serde::Serialize)]
struct Payload {
    value: &'static str,
}

impl EventPayload for Payload {
    const EVENT_TYPE: &'static str = "test.credentials.rotated";
    const SCHEMA_VERSION: u16 = 1;
}

// Independent published upstream framing/signature reference. This verifies
// the JWT bytes without issuing a second fixture token from the same helper.
fn verify_upstream_reference() {
    let creds = include_str!("../../../vendor/async-nats/tests/configs/TestUser.creds");
    let jwt = creds.lines().find(|line| line.starts_with("eyJ")).unwrap();
    let (signed, signature) = jwt.rsplit_once('.').unwrap();
    let (header, _) = signed.split_once('.').unwrap();
    assert_eq!(
        URL_SAFE_NO_PAD.decode(header).unwrap(),
        br#"{"typ":"JWT","alg":"ed25519-nkey"}"#
    );
    KeyPair::from_public_key("ADTQS7ZCFVJNW5726GOYXW5TSCZFNIQSHK2ZGYUBCD5D77OTNLOOKZOZ")
        .unwrap()
        .verify(
            signed.as_bytes(),
            &URL_SAFE_NO_PAD.decode(signature).unwrap(),
        )
        .expect("the published NATS reference signature verifies over the compact token bytes");
}

#[tokio::test]
async fn expired_old_credentials_are_refused_and_file_replacement_recovers_the_client() {
    verify_upstream_reference();
    let url = std::env::var("NATS_AUTH_URL")
        .expect("NATS_AUTH_URL is required; use test-integration-messaging.sh");
    let anonymous = timeout(Duration::from_secs(5), async_nats::connect(&url))
        .await
        .unwrap()
        .expect_err("the authenticated segment must reject anonymous clients");
    assert_eq!(anonymous.kind(), ConnectErrorKind::AuthorizationViolation);

    let administrator = User::expiring_after(300);
    let admin = administrator
        .options()
        .connect(&url)
        .await
        .expect("the synthetic account must authenticate against the configured operator");
    let jetstream = jetstream::new(admin.clone());
    let suffix = KeyPair::new_user().public_key();
    let stream = format!("ROTATION_{suffix}");
    let subject = format!("test.rotation.{suffix}");
    jetstream
        .create_stream(jetstream::stream::Config {
            name: stream.clone(),
            subjects: vec![subject.clone()],
            storage: jetstream::stream::StorageType::Memory,
            max_messages: 10,
            ..Default::default()
        })
        .await
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("user.creds");
    // Broker expiration uses wall time; the bounded poll below observes it
    // instead of assuming a scheduler sleep is sufficient.
    let old = User::expiring_after(8);
    old.publish_file(&path).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        MessagingOptions {
            connection_name: "synthetic-credential-rotation".to_owned(),
            servers: vec![url.clone()],
            credentials: None,
            credentials_file: Some(path.clone()),
            root_ca_path: None,
            allow_plaintext: true,
            source_stream: stream.clone(),
            dlq_stream: None,
            max_payload_bytes: 1024,
            consumer: None,
        },
        deadline(),
        cancel.clone(),
    ))
    .await
    .unwrap();
    let registry = Registry::new([Route::new::<Payload>(subject)]).unwrap();
    let event = |id: &str| Event {
        id: id.to_owned(),
        occurred_at: time::UtcDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        payload: Payload {
            value: "authenticated",
        },
    };
    let first = registry.prepare(&event("before-expiry"), 1024).unwrap();
    messaging
        .producer()
        .publish(&first, deadline(), &cancel)
        .await
        .expect("old credentials must work before their native expiration");

    timeout(Duration::from_secs(20), async {
        let mut poll = tokio::time::interval(Duration::from_millis(100));
        while unix_now() <= old.expires || messaging.probe().check().await.is_ok() {
            poll.tick().await;
        }
    })
    .await
    .expect("the old authenticated session must end after native user expiry");
    let refused = timeout(Duration::from_secs(5), old.options().connect(&url))
        .await
        .unwrap()
        .expect_err("previously working, now expired credentials must be refused");
    assert_eq!(
        refused.kind(),
        ConnectErrorKind::AuthorizationViolation,
        "refusal must come from broker authentication, not unavailable transport"
    );

    let replacement = User::expiring_after(300);
    replacement.publish_file(&path).await;
    let after = registry.prepare(&event("after-replacement"), 1024).unwrap();
    timeout(Duration::from_secs(15), async {
        let mut poll = tokio::time::interval(Duration::from_millis(50));
        loop {
            if messaging
                .producer()
                .publish(&after, deadline(), &cancel)
                .await
                .is_ok()
            {
                break;
            }
            poll.tick().await;
        }
    })
    .await
    .expect("the same adapter must authenticate with the corrected current file and publish");
    assert_eq!(
        messaging.close(deadline(), &cancel).await,
        CloseOutcome::Complete
    );
    jetstream.delete_stream(&stream).await.unwrap();
    drop(jetstream);
    admin.drain().await.unwrap();
}
