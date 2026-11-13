//! Credential sources accepted and refused by the broker.

use super::*;

const AUTH_USER_A: &str = "UDBCQIADO7SFCGGE5FNGOTL66SCF6F3FL6WWOT75R36TCLHAVKM6TACY";
const AUTH_USER_B: &str = "UDKZULILKPV3BALVMY6JQ63JRXETGP3E4EF4REATS2YFC43HLFIZPCXC";

fn auth_fixture_path(name: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("runner must set {name}")),
    );
    assert!(path.is_absolute(), "runner must supply an absolute {name}");
    path
}

fn credentials_section<'a>(credentials: &'a str, begin: &str, end: &str) -> &'a str {
    credentials
        .split_once(begin)
        .and_then(|(_, rest)| rest.split_once(end).map(|(value, _)| value.trim()))
        .filter(|value| !value.is_empty())
        .expect("runner fixture has one complete credentials section")
}

fn credentials_jwt(credentials: &str) -> &str {
    credentials_section(
        credentials,
        "-----BEGIN NATS USER JWT-----",
        "------END NATS USER JWT------",
    )
}

fn credentials_seed(credentials: &str) -> &str {
    credentials_section(
        credentials,
        "-----BEGIN USER NKEY SEED-----",
        "------END USER NKEY SEED------",
    )
}

fn credentials_public_user(credentials: &str) -> String {
    use base64::Engine as _;

    let payload = credentials_jwt(credentials)
        .split('.')
        .nth(1)
        .expect("fixture JWT has a payload");
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .expect("fixture JWT payload is base64url");
    serde_json::from_slice::<serde_json::Value>(&payload)
        .expect("fixture JWT payload is JSON")["sub"]
        .as_str()
        .expect("fixture JWT names a user subject")
        .to_owned()
}

fn credentials_document(jwt: &str, seed: &str) -> String {
    format!(
        "-----BEGIN NATS USER JWT-----\n{jwt}\n------END NATS USER JWT------\n\n\
         -----BEGIN USER NKEY SEED-----\n{seed}\n------END USER NKEY SEED------\n"
    )
}

#[expect(
    clippy::disallowed_methods,
    reason = "test-owned credential copies rotate atomically before the next reconnect"
)]
fn replace_credentials(path: &std::path::Path, credentials: &[u8]) {
    let replacement = path.with_extension("next");
    std::fs::write(&replacement, credentials).expect("test credential file is written");
    std::fs::rename(replacement, path).expect("test credential file is replaced");
}

#[tokio::test]
#[ignore = "requires the runner-owned JWT auth fixture"]
#[expect(
    clippy::print_stdout,
    reason = "the runner captures bounded phase and cleanup receipts with --no-capture"
)]
#[expect(
    clippy::too_many_lines,
    reason = "one broker-authentication cycle retains its admission, refusal, recovery, and cleanup observations"
)]
async fn credentials_file_rotation_is_authenticated_by_the_broker() {
    let auth_url = std::env::var("NATS_AUTH_URL").expect("runner must set NATS_AUTH_URL");
    let credentials_a = auth_fixture_path("NATS_AUTH_CREDS_A");
    let credentials_b = auth_fixture_path("NATS_AUTH_CREDS_B");
    let a_contents = tokio::fs::read_to_string(&credentials_a)
        .await
        .expect("runner-provided A credentials are readable");
    let b_contents = tokio::fs::read_to_string(&credentials_b)
        .await
        .expect("runner-provided B credentials are readable");
    assert_eq!(credentials_public_user(&a_contents), AUTH_USER_A);
    assert_eq!(credentials_public_user(&b_contents), AUTH_USER_B);

    let unauthenticated = async_nats::ConnectOptions::new()
        .connection_timeout(Duration::from_secs(2))
        .connect(&auth_url)
        .await
        .expect_err("the runner-owned broker rejects an unauthenticated client");
    std::assert_matches!(
        unauthenticated.kind(),
        async_nats::ConnectErrorKind::Authentication
            | async_nats::ConnectErrorKind::AuthorizationViolation
    );
    println!("auth_rotation phase=unauthenticated_denied");

    let (fixture, admin) =
        Fixture::create_with_authenticated_admin(&auth_url, &credentials_a).await;
    let source = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .expect("authenticated admin reads its source stream");
    assert_eq!(
        source.cached_info().config.storage,
        stream::StorageType::File
    );
    assert_eq!(source.cached_info().config.persist_mode, None);
    println!("auth_rotation phase=authenticated_admin_stream_ready");

    let directory = tempfile::tempdir().expect("test credentials directory");
    let active_credentials = directory.path().join("active.creds");
    replace_credentials(&active_credentials, a_contents.as_bytes());
    let relay = AuthObservingRelay::start(&auth_url).await;
    let cancel = CancellationToken::new();
    let mut options = options_with_servers(&fixture, vec![relay.url.clone()], None, 1024);
    options.credentials = Some(Credentials::File(active_credentials.clone()));
    let messaging = Messaging::connect(options, deadline(), cancel.clone())
        .await
        .expect("tuple A authenticates through the relay");
    relay.wait_for_latest_public_user(AUTH_USER_A).await;
    messaging
        .probe()
        .check()
        .await
        .expect("tuple A receives fresh source metadata");
    println!("auth_rotation phase=tuple_a_probe_ready");
    let work_a = registry(&fixture)
        .prepare(&event("authenticated-work-a"), 1024)
        .expect("A work has a distinct immutable identity");
    messaging
        .producer()
        .publish(&work_a, &context(deadline(), &cancel))
        .await
        .expect("tuple A receives a broker publication acknowledgment");
    println!("auth_rotation phase=tuple_a_acknowledged");

    let mismatch =
        credentials_document(credentials_jwt(&b_contents), credentials_seed(&a_contents));
    assert!(
        async_nats::ConnectOptions::with_credentials(&mismatch).is_ok(),
        "the B-JWT/A-seed control reaches native JWT signing instead of a parser refusal"
    );
    replace_credentials(&active_credentials, mismatch.as_bytes());
    relay.close_old_connection().await;
    println!("auth_rotation phase=old_relay_connection_closed");
    relay.wait_for_broker_refusal().await;
    println!("auth_rotation phase=broker_refusal_observed");

    let cutoff = Instant::now() + Duration::from_secs(2);
    let mut contenders = tokio::task::JoinSet::new();
    for _ in 0..2 {
        let probe = messaging.probe();
        contenders.spawn(async move {
            matches!(
                tokio::time::timeout_at(cutoff, probe.check()).await,
                Err(_) | Ok(Err(_))
            )
        });
    }
    while let Some(contender) = contenders.join_next().await {
        assert!(
            contender.expect("read-only contender joins"),
            "a contender must not report fresh readiness through the rejected session"
        );
    }
    println!("auth_rotation phase=read_only_contenders_joined");

    replace_credentials(&active_credentials, b_contents.as_bytes());
    timeout(Duration::from_secs(10), async {
        loop {
            if messaging.probe().check().await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the same Messaging client authenticates tuple B and reads fresh metadata");
    println!("auth_rotation phase=tuple_b_probe_ready");
    let work_b = registry(&fixture)
        .prepare(&event("authenticated-work-b"), 1024)
        .expect("B work has a distinct immutable identity");
    messaging
        .producer()
        .publish(&work_b, &context(deadline(), &cancel))
        .await
        .expect("tuple B receives a broker publication acknowledgment");
    assert_eq!(
        relay.latest_public_user().as_deref(),
        Some(AUTH_USER_B),
        "fresh B metadata and acknowledged B work must use B on the current relay connection"
    );
    println!("auth_rotation phase=tuple_b_connection_associated");
    println!("auth_rotation phase=tuple_b_acknowledged");

    close(messaging).await;
    println!("auth_rotation cleanup=messaging_native_closed");
    relay.finish().await;
    println!("auth_rotation cleanup=relay_joined");
    fixture.cleanup().await;
    println!("auth_rotation cleanup=fixture_streams_deleted");
    admin
        .drain()
        .await
        .expect("authenticated admin begins native drain");
    assert!(
        timeout(Duration::from_secs(5), admin.wait_closed())
            .await
            .expect("authenticated admin close remains bounded"),
        "authenticated admin native runner reports completion"
    );
    println!("auth_rotation cleanup=admin_native_closed");
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
async fn an_unusable_credentials_source_refuses_the_connection() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let dir = tempfile::tempdir().expect("temporary directory is created");

    let mut missing = options(&fixture, None, 1024);
    missing.credentials = Some(Credentials::File(dir.path().join("absent.creds")));
    let refused = Box::pin(Messaging::connect(missing, deadline(), cancel.clone()))
        .await
        .expect_err("a missing credentials file refuses the connection");
    std::assert_matches!(
        refused,
        infra_messaging::MessagingError::Configuration(_),
        "{refused:?}"
    );

    let malformed_path = dir.path().join("malformed.creds");
    std::fs::write(&malformed_path, "not credentials").expect("fixture file is written");
    let mut malformed = options(&fixture, None, 1024);
    malformed.credentials = Some(Credentials::File(malformed_path));
    let refused = Box::pin(Messaging::connect(malformed, deadline(), cancel.clone()))
        .await
        .expect_err("a malformed credentials file refuses the connection");
    std::assert_matches!(
        refused,
        infra_messaging::MessagingError::Authentication,
        "{refused:?}"
    );

    fixture.cleanup().await;
}
