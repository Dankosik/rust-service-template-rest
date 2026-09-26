use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum_test::TestServer;
use bytes::Bytes;
use health::Readiness;
use http::{HeaderMap, HeaderValue, StatusCode};
use infra_jobs::{Engine, JobError, Kinds, Policy};
use infra_postgres::{Dsn, PgPool, Tx, connection};
use infra_webhooks::inbound::{
    Consumer, Consumers, Incoming, Processor, ReceiptOutcome, ReceiveError, Receiver,
};
use infra_webhooks::protocol::KeyRing;
use integration_tests::{DATABASE_URL, dsn_for};
use sqlx::Row;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use url::Url;

use super::commit_proxy::{CommitProxy, Fault};

const ENDPOINT: &str = "partner/a?#";
const KEY: &str = "whsec_d2ViaG9va19zZWNyZXQ=";

fn receiver(pool: PgPool) -> Receiver {
    Receiver::new(
        pool,
        [(
            ENDPOINT.to_owned(),
            KeyRing::from_encoded(KEY, None).expect("key"),
        )],
    )
}

fn signed_headers(keys: &KeyRing, message_id: impl AsRef<[u8]>, body: &[u8]) -> HeaderMap {
    let message_id = message_id.as_ref();
    let timestamp: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
        .try_into()
        .expect("timestamp fits i64");
    let mut headers = HeaderMap::new();
    headers.insert(
        "webhook-id",
        HeaderValue::from_bytes(message_id).expect("message id"),
    );
    headers.insert(
        "webhook-timestamp",
        HeaderValue::from_str(&timestamp.to_string()).expect("timestamp"),
    );
    headers.insert(
        "webhook-signature",
        HeaderValue::from_str(
            &keys
                .signatures(message_id, timestamp, body)
                .expect("signature"),
        )
        .expect("signature header"),
    );
    headers.insert(
        "content-type",
        HeaderValue::from_bytes(b"application/webhook\xff").expect("opaque content type"),
    );
    headers
}

async fn receipt_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM webhook_receipts")
        .fetch_one(pool)
        .await
        .expect("receipt count")
}

async fn job_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM background_jobs")
        .fetch_one(pool)
        .await
        .expect("job count")
}

async fn proxied_pool(pool: &PgPool) -> (CommitProxy, PgPool) {
    let dsn = dsn_for(pool).await;
    assert_eq!(
        dsn.ssl_mode_name(),
        "disable",
        "the proxy is plaintext only"
    );
    let host = dsn.host().trim_start_matches('[').trim_end_matches(']');
    let server = tokio::net::lookup_host((host, dsn.port()))
        .await
        .expect("server resolves")
        .next()
        .expect("server address");
    let proxy = CommitProxy::start(server).await;
    let raw = std::env::var(DATABASE_URL).expect("DATABASE_URL is set");
    let mut url = Url::parse(&raw).expect("DATABASE_URL is a URL");
    url.set_path(dsn.database());
    url.set_ip_host(proxy.address().ip())
        .expect("proxy is an IP host");
    url.set_port(Some(proxy.address().port()))
        .expect("proxy port");
    let proxied = Dsn::admit(url.as_str()).expect("proxied DSN");
    (proxy, super::template_pool(&proxied, 3).await)
}

async fn until(what: &str, mut ready: impl AsyncFnMut() -> Option<()>) {
    super::bounded(what, async {
        loop {
            if ready().await.is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn receiver_preserves_first_admission_on_authenticated_changed_replay(pool: PgPool) {
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let receiver = receiver(pool.clone());
    let body = b"\0{\"event\":\"created\"}\xff";
    let headers = signed_headers(&keys, "message-1", body);

    assert_eq!(
        receiver
            .receive(ENDPOINT, &headers, body, SystemTime::now())
            .await,
        Ok(ReceiptOutcome::Accepted)
    );
    let first: String = sqlx::query_scalar("SELECT received_at::text FROM webhook_receipts")
        .fetch_one(&pool)
        .await
        .expect("first admission time");
    assert_eq!(
        receiver
            .receive(ENDPOINT, &headers, body, SystemTime::now())
            .await,
        Ok(ReceiptOutcome::Duplicate)
    );
    let changed = b"\0{\"event\":\"changed\"}\xff";
    assert_eq!(
        receiver
            .receive(
                ENDPOINT,
                &signed_headers(&keys, "message-1", changed),
                changed,
                SystemTime::now(),
            )
            .await,
        Ok(ReceiptOutcome::Duplicate)
    );
    let mut replay_headers = signed_headers(&keys, "message-1", changed);
    replay_headers.insert("content-type", HeaderValue::from_static("text/plain"));
    assert_eq!(
        receiver
            .receive(ENDPOINT, &replay_headers, changed, SystemTime::now())
            .await,
        Ok(ReceiptOutcome::Duplicate)
    );
    replay_headers.insert("webhook-signature", HeaderValue::from_static("v1,invalid"));
    assert_eq!(
        receiver
            .receive(ENDPOINT, &replay_headers, changed, SystemTime::now())
            .await,
        Err(ReceiveError::Rejected)
    );
    assert_eq!(receipt_count(&pool).await, 1);
    assert_eq!(job_count(&pool).await, 1);
    let timestamp: String = sqlx::query_scalar("SELECT received_at::text FROM webhook_receipts")
        .fetch_one(&pool)
        .await
        .expect("unchanged admission time");
    assert_eq!(timestamp, first);
    let payload: serde_json::Value = sqlx::query_scalar("SELECT payload FROM background_jobs")
        .fetch_one(&pool)
        .await
        .expect("original processing job");
    let incoming: Incoming = serde_json::from_value(payload).expect("incoming");
    assert_eq!(incoming.body(), body);
    assert_eq!(
        incoming.content_type(),
        Some(b"application/webhook\xff".as_slice())
    );
    let row = sqlx::query("SELECT endpoint_id, message_id FROM webhook_receipts")
        .fetch_one(&pool)
        .await
        .expect("receipt row");
    assert_eq!(
        row.try_get::<String, _>("endpoint_id").expect("endpoint"),
        ENDPOINT
    );
    assert_eq!(
        row.try_get::<Vec<u8>, _>("message_id").expect("message"),
        b"message-1".to_vec()
    );
    super::close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn concurrent_verified_copies_converge_on_one_receipt_and_job(pool: PgPool) {
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let first = receiver(pool.clone());
    let second = receiver(pool.clone());
    let body = b"{\"same\":true}";
    let headers = signed_headers(&keys, "message-race", body);
    let (one, two) = tokio::join!(
        first.receive(ENDPOINT, &headers, body, SystemTime::now()),
        second.receive(ENDPOINT, &headers, body, SystemTime::now()),
    );
    assert!(
        matches!(one, Ok(ReceiptOutcome::Accepted)) && matches!(two, Ok(ReceiptOutcome::Duplicate))
            || matches!(one, Ok(ReceiptOutcome::Duplicate))
                && matches!(two, Ok(ReceiptOutcome::Accepted)),
        "outcomes were {one:?} and {two:?}"
    );
    assert_eq!(receipt_count(&pool).await, 1);
    assert_eq!(job_count(&pool).await, 1);
    super::close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn enqueue_failure_rolls_back_the_new_receipt(pool: PgPool) {
    sqlx::query("DROP TABLE background_jobs")
        .execute(&pool)
        .await
        .expect("remove jobs table to force the production enqueue failure");
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let body = b"{\"rollback\":true}";
    let outcome = receiver(pool.clone())
        .receive(
            ENDPOINT,
            &signed_headers(&keys, "message-rollback", body),
            body,
            SystemTime::now(),
        )
        .await;
    assert_eq!(outcome, Err(ReceiveError::Unavailable));
    assert_eq!(receipt_count(&pool).await, 0);
    super::close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn lost_receipt_commit_acknowledgement_returns_unavailable_and_retry_converges(pool: PgPool) {
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let body = b"{\"lost_ack\":true}";
    for (fault, message_id, committed, retry_outcome) in [
        (
            Fault::DropBeforeForward,
            "message-before-commit",
            false,
            ReceiptOutcome::Accepted,
        ),
        (
            Fault::ForwardThenDrop,
            "message-after-commit",
            true,
            ReceiptOutcome::Duplicate,
        ),
    ] {
        let previous = receipt_count(&pool).await;
        let (proxy, proxied) = proxied_pool(&pool).await;
        let headers = signed_headers(&keys, message_id, body);
        proxy.arm((fault, "INSERT INTO webhook_receipts"));
        assert_eq!(
            receiver(proxied.clone())
                .receive(ENDPOINT, &headers, body, SystemTime::now())
                .await,
            Err(ReceiveError::Unavailable)
        );
        assert_eq!(proxy.fired(), Some(fault));
        assert_eq!(receipt_count(&pool).await, previous + i64::from(committed));
        assert_eq!(job_count(&pool).await, previous + i64::from(committed));
        assert_eq!(
            receiver(pool.clone())
                .receive(ENDPOINT, &headers, body, SystemTime::now())
                .await,
            Ok(retry_outcome)
        );
        assert_eq!(receipt_count(&pool).await, previous + 1);
        assert_eq!(job_count(&pool).await, previous + 1);
        super::close(&[&proxied]).await;
        proxy.shutdown().await;
    }
    super::close(&[&pool]).await;
}

#[derive(Default)]
struct EffectConsumer {
    entered: Notify,
    release: Notify,
}

impl Consumer for EffectConsumer {
    fn process<'a>(
        &'a self,
        tx: &'a mut Tx<'_>,
        incoming: &'a Incoming,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), JobError>> + Send + 'a>>
    {
        Box::pin(async move {
            sqlx::query("INSERT INTO webhook_effects (message_id, content_type) VALUES ($1, $2)")
                .bind(incoming.message_id())
                .bind(incoming.content_type())
                .execute(&mut *connection(tx))
                .await
                .map_err(JobError::from)?;
            self.entered.notify_one();
            self.release.notified().await;
            Ok(())
        })
    }
}

struct RunningProcessor {
    started: infra_jobs::Started,
    cancel: CancellationToken,
    tracker: TaskTracker,
}

impl Drop for RunningProcessor {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.tracker.close();
    }
}

impl RunningProcessor {
    fn start(pool: &PgPool, consumers: Consumers) -> Self {
        let mut kinds = Kinds::new();
        kinds.register(Policy::default(), Processor::new(consumers));
        let engine = Engine::new(
            pool.clone(),
            kinds.validate().expect("webhook processor registry"),
            std::num::NonZeroU32::new(1).expect("one worker"),
        );
        let cancel = CancellationToken::new();
        let tracker = TaskTracker::new();
        let started = engine.start(&tracker, &cancel);
        Self {
            started,
            cancel,
            tracker,
        }
    }

    fn with_consumer(pool: &PgPool, consumer: Arc<dyn Consumer>) -> Self {
        let mut consumers = Consumers::new();
        assert!(consumers.insert(ENDPOINT, consumer).is_none());
        Self::start(pool, consumers)
    }

    async fn close(self, pools: &[&PgPool]) {
        self.cancel.cancel();
        self.tracker.close();
        super::bounded("the processor tasks join", self.tracker.wait()).await;
        super::close(pools).await;
    }
}

async fn accept_for_processing(pool: &PgPool, message_id: &str) {
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let body = b"{\"process\":true}";
    assert_eq!(
        receiver(pool.clone())
            .receive(
                ENDPOINT,
                &signed_headers(&keys, message_id, body),
                body,
                SystemTime::now(),
            )
            .await,
        Ok(ReceiptOutcome::Accepted)
    );
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn stale_fenced_completion_rolls_back_consumer_effects(pool: PgPool) {
    sqlx::query("CREATE TABLE webhook_effects (message_id bytea NOT NULL, content_type bytea)")
        .execute(&pool)
        .await
        .expect("consumer effects table");
    accept_for_processing(&pool, "message-stale").await;
    let consumer = Arc::new(EffectConsumer::default());
    let running =
        RunningProcessor::with_consumer(&pool, Arc::clone(&consumer) as Arc<dyn Consumer>);
    super::bounded(
        "the consumer effect is pending",
        consumer.entered.notified(),
    )
    .await;
    let stale = sqlx::query(
        "UPDATE background_jobs SET claim_generation = nextval('background_jobs_claim_generation') \
         WHERE kind = 'webhooks.process' AND state = 'running'",
    )
    .execute(&pool)
    .await
    .expect("claim becomes stale");
    assert_eq!(stale.rows_affected(), 1);
    consumer.release.notify_one();
    until("the stale processor attempt ends", async || {
        (running.started.in_flight() == 0).then_some(())
    })
    .await;
    let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM webhook_effects")
        .fetch_one(&pool)
        .await
        .expect("effects count");
    assert_eq!(effects, 0);
    let state: String =
        sqlx::query_scalar("SELECT state FROM background_jobs WHERE kind = 'webhooks.process'")
            .fetch_one(&pool)
            .await
            .expect("job state");
    assert_eq!(state, "running");
    running.close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn unknown_processor_commit_does_not_add_a_competing_retry(pool: PgPool) {
    sqlx::query("CREATE TABLE webhook_effects (message_id bytea NOT NULL, content_type bytea)")
        .execute(&pool)
        .await
        .expect("consumer effects table");
    // Historical accepted jobs retain IDs above the new admission bound.
    let legacy_message_id = vec![b'x'; 512];
    let incoming: Incoming = serde_json::from_value(serde_json::json!({
        "version": 1,
        "endpoint_id": ENDPOINT,
        "message_id": base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &legacy_message_id,
        ),
        "content_type": null,
        "body": "bGVnYWN5"
    }))
    .expect("historical inbound payload");
    infra_postgres::in_tx(&pool, async |tx| -> Result<(), infra_postgres::TxError> {
        infra_jobs::enqueue(tx, &incoming, infra_jobs::EnqueueOptions::default())
            .await
            .expect("enqueue historical payload");
        Ok(())
    })
    .await
    .expect("historical accepted job");
    let (proxy, worker) = proxied_pool(&pool).await;
    let consumer = Arc::new(EffectConsumer::default());
    let running =
        RunningProcessor::with_consumer(&worker, Arc::clone(&consumer) as Arc<dyn Consumer>);
    super::bounded(
        "the consumer is ready to commit",
        consumer.entered.notified(),
    )
    .await;
    proxy.arm((Fault::ForwardThenDrop, "SET state = 'completed'"));
    consumer.release.notify_one();
    until("the unknown processor attempt ends", async || {
        (running.started.in_flight() == 0).then_some(())
    })
    .await;
    assert_eq!(proxy.fired(), Some(Fault::ForwardThenDrop));
    let effects: Vec<Vec<u8>> = sqlx::query_scalar("SELECT message_id FROM webhook_effects")
        .fetch_all(&pool)
        .await
        .expect("committed consumer effects");
    assert_eq!(effects, vec![legacy_message_id]);
    let row =
        sqlx::query("SELECT state, attempts FROM background_jobs WHERE kind = 'webhooks.process'")
            .fetch_one(&pool)
            .await
            .expect("job state");
    assert_eq!(
        row.try_get::<String, _>("state").expect("state"),
        "completed"
    );
    assert_eq!(row.try_get::<i16, _>("attempts").expect("attempts"), 1);
    running.close(&[&pool, &worker]).await;
    proxy.shutdown().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn missing_consumer_spends_attempts_and_exhausts_the_normal_budget(pool: PgPool) {
    accept_for_processing(&pool, "message-missing-consumer").await;
    // Resume historical work with two of the normal 25 attempts remaining.
    sqlx::query("UPDATE background_jobs SET attempts = 23 WHERE kind = 'webhooks.process'")
        .execute(&pool)
        .await
        .expect("historical attempt count");
    let running = RunningProcessor::start(&pool, Consumers::new());
    until("the missing binding spends its retry attempt", async || {
        let row = sqlx::query(
            "SELECT state, attempts FROM background_jobs WHERE kind = 'webhooks.process'",
        )
        .fetch_one(&pool)
        .await
        .expect("job row");
        (row.try_get::<String, _>("state").expect("state") == "pending"
            && row.try_get::<i16, _>("attempts").expect("attempts") == 24)
            .then_some(())
    })
    .await;
    sqlx::query(
        "UPDATE background_jobs SET not_before = now() \
         WHERE kind = 'webhooks.process' AND state = 'pending'",
    )
    .execute(&pool)
    .await
    .expect("make the final retry eligible without waiting for backoff");
    let row = until("the missing binding exhausts its budget", async || {
        let row = sqlx::query(
            "SELECT state, attempts, failure_reason FROM background_jobs \
             WHERE kind = 'webhooks.process'",
        )
        .fetch_one(&pool)
        .await
        .expect("job row");
        (row.try_get::<String, _>("state").expect("state") == "failed").then_some(row)
    })
    .await;
    assert_eq!(row.try_get::<i16, _>("attempts").expect("attempts"), 25);
    assert_eq!(
        row.try_get::<String, _>("failure_reason")
            .expect("failure reason"),
        "exhausted"
    );
    running.close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn mounted_percent_decoded_endpoint_reaches_signature_rejection(pool: PgPool) {
    let router = infra_http::webhooks::router()
        .finalize_public()
        .expect("webhook operation is public");
    let app = infra_http::webhooks::with_webhook_state(
        router,
        infra_http::webhooks::WebhookState::active(receiver(pool.clone())),
    )
    .with_state(Readiness::new(Vec::new()).reader());
    let response = TestServer::new(app)
        .post("/webhooks/partner%2Fa%3F%23")
        .add_header("webhook-id", "message-decoded")
        .add_header("webhook-timestamp", "0")
        .add_header("webhook-signature", "v1,invalid")
        .bytes(Bytes::from_static(b"{}"))
        .await;
    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(receipt_count(&pool).await, 0);
    super::close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn receipt_identity_preserves_binary_ids_and_endpoint_scope(pool: PgPool) {
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let receiver = Receiver::new(
        pool.clone(),
        [
            ("partner".to_owned(), keys.clone()),
            ("Partner".to_owned(), keys.clone()),
        ],
    );
    for (endpoint, message) in [
        ("partner", b"id\x80".as_slice()),
        ("partner", b"id\xff".as_slice()),
        ("Partner", b"id\x80".as_slice()),
    ] {
        assert_eq!(
            receiver
                .receive(
                    endpoint,
                    &signed_headers(&keys, message, b"{}"),
                    b"{}",
                    SystemTime::now()
                )
                .await,
            Ok(ReceiptOutcome::Accepted)
        );
    }
    let identities: Vec<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT endpoint_id, message_id FROM webhook_receipts ORDER BY endpoint_id, message_id",
    )
    .fetch_all(&pool)
    .await
    .expect("exact identities");
    assert_eq!(
        identities,
        vec![
            ("Partner".to_owned(), b"id\x80".to_vec()),
            ("partner".to_owned(), b"id\x80".to_vec()),
            ("partner".to_owned(), b"id\xff".to_vec())
        ]
    );
    assert_eq!(job_count(&pool).await, 3);
    super::close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn mounted_admission_distinguishes_replay_id_bounds_and_body_failures(pool: PgPool) {
    use axum::body::{Body, to_bytes};
    use tower::ServiceExt as _;

    let app = infra_http::webhooks::with_webhook_state(
        infra_http::webhooks::router()
            .finalize_public()
            .expect("public contract"),
        infra_http::webhooks::WebhookState::active(receiver(pool.clone())),
    )
    .with_state(Readiness::new(Vec::new()).reader());
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    for (id, body, content_type, status) in [
        (
            vec![b'm'; 255],
            b"first".as_slice(),
            "text/plain",
            StatusCode::NO_CONTENT,
        ),
        (
            vec![b'm'; 255],
            b"changed".as_slice(),
            "application/json",
            StatusCode::NO_CONTENT,
        ),
        (
            vec![b'm'; 256],
            b"too long".as_slice(),
            "text/plain",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let mut request = http::Request::post("/webhooks/partner%2Fa%3F%23")
            .body(Body::from(body))
            .expect("request");
        *request.headers_mut() = signed_headers(&keys, &id, body);
        request
            .headers_mut()
            .insert("content-type", HeaderValue::from_static(content_type));
        let response = app
            .clone()
            .oneshot(request)
            .await
            .expect("mounted response");
        assert_eq!(response.status(), status);
        if status == StatusCode::NO_CONTENT {
            assert!(
                to_bytes(response.into_body(), 1024)
                    .await
                    .expect("body")
                    .is_empty()
            );
        } else {
            assert_eq!(
                response.headers()["content-type"],
                "application/problem+json"
            );
            let problem: serde_json::Value = serde_json::from_slice(
                &to_bytes(response.into_body(), 4096).await.expect("problem"),
            )
            .expect("JSON");
            assert_eq!(problem["code"], "webhook_rejected");
        }
    }
    for (path, body, status, code) in [
        (
            "/webhooks/partner%2Fa%3F%23",
            Body::from(vec![b'x'; infra_webhooks::protocol::MAX_BODY_BYTES + 1]),
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_entity_too_large",
        ),
        (
            "/webhooks/partner%2Fa%3F%23",
            Body::from_stream(futures_util::stream::once(async {
                Err::<Bytes, _>(std::io::Error::other("private transport failure"))
            })),
            StatusCode::BAD_REQUEST,
            "webhook_rejected",
        ),
        (
            "/webhooks/unknown",
            Body::from_stream(futures_util::stream::once(async {
                panic!("unknown endpoints must not poll the body");
                #[allow(unreachable_code)]
                Ok::<Bytes, std::io::Error>(Bytes::new())
            })),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(http::Request::post(path).body(body).expect("request"))
            .await
            .expect("mounted response");
        assert_eq!(response.status(), status);
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
        let bytes = to_bytes(response.into_body(), 4096).await.expect("problem");
        let problem: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(problem["code"], code);
        assert!(!String::from_utf8_lossy(&bytes).contains("private transport failure"));
    }
    assert_eq!(receipt_count(&pool).await, 1);
    assert_eq!(job_count(&pool).await, 1);
    super::close(&[&pool]).await;
}

const NATIVE_RECEIPTS_VERSION: i64 = 20_260_926_130_000;

async fn historical_schema(pool: &PgPool) {
    let historical = sqlx::migrate::Migrator::with_migrations(
        migrate::MIGRATOR
            .iter()
            .filter(|migration| migration.version < NATIVE_RECEIPTS_VERSION)
            .cloned()
            .collect(),
    );
    historical.run(pool).await.expect("historical migrations");
}

async fn historical_receipt(pool: &PgPool, hash: u8, endpoint: &str, message: &[u8]) {
    sqlx::query("INSERT INTO webhook_receipts (identity_hash, endpoint_id, message_id, body_sha256) VALUES ($1, $2, $3, $4)")
        .bind(vec![hash; 32]).bind(endpoint).bind(message).bind(vec![7_u8; 32])
        .execute(pool).await.expect("historical receipt");
}

#[sqlx::test(migrations = false)]
async fn receipt_migration_preserves_historical_pairs_jobs_and_admission_approximation(
    pool: PgPool,
) {
    historical_schema(&pool).await;
    let legacy = vec![b'x'; 512];
    for (hash, endpoint, message) in [
        (1, "Partner", b"id\x80".as_slice()),
        (2, "partner", b"id\x80".as_slice()),
        (3, ENDPOINT, legacy.as_slice()),
    ] {
        historical_receipt(&pool, hash, endpoint, message).await;
    }
    let incoming: Incoming = serde_json::from_value(serde_json::json!({
        "version": 1, "endpoint_id": ENDPOINT,
        "message_id": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &legacy),
        "content_type": null, "body": "bGVnYWN5"
    }))
    .expect("legacy inbound payload");
    infra_postgres::in_tx(&pool, async |tx| -> Result<(), infra_postgres::TxError> {
        infra_jobs::enqueue(tx, &incoming, infra_jobs::EnqueueOptions::default())
            .await
            .expect("enqueue legacy fixture");
        Ok(())
    })
    .await
    .expect("legacy job");
    let jobs_before: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT to_jsonb(background_jobs) FROM background_jobs ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("jobs before");
    let pairs_before: Vec<(String, Vec<u8>)> = sqlx::query_as("SELECT endpoint_id, message_id FROM webhook_receipts ORDER BY endpoint_id COLLATE \"C\", message_id")
        .fetch_all(&pool).await.expect("pairs before");
    let before: String = sqlx::query_scalar("SELECT now()::text")
        .fetch_one(&pool)
        .await
        .expect("migration lower bound");
    migrate::MIGRATOR
        .run(&pool)
        .await
        .expect("forward migration");
    let pairs_after: Vec<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT endpoint_id, message_id FROM webhook_receipts ORDER BY endpoint_id, message_id",
    )
    .fetch_all(&pool)
    .await
    .expect("pairs after");
    assert_eq!(pairs_after, pairs_before);
    let jobs_after: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT to_jsonb(background_jobs) FROM background_jobs ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("jobs after");
    assert_eq!(jobs_after, jobs_before);
    let approximation: bool = sqlx::query_scalar("SELECT count(DISTINCT received_at) = 1 AND bool_and(received_at >= $1::text::timestamptz AND received_at <= now()) FROM webhook_receipts")
        .bind(before).fetch_one(&pool).await.expect("migration timestamp approximation");
    assert!(approximation);
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    assert_eq!(
        receiver(pool.clone())
            .receive(
                ENDPOINT,
                &signed_headers(&keys, &legacy, b"legacy"),
                b"legacy",
                SystemTime::now()
            )
            .await,
        Err(ReceiveError::Rejected)
    );
    assert_eq!(receipt_count(&pool).await, 3);
    assert_eq!(job_count(&pool).await, 1);
    super::close(&[&pool]).await;
}

async fn assert_receipt_migration_rollback(pool: &PgPool, expected_code: &str) {
    let before: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(webhook_receipts) FROM webhook_receipts ORDER BY identity_hash",
    )
    .fetch_all(pool)
    .await
    .expect("old receipts");
    let error = migrate::MIGRATOR
        .run(pool)
        .await
        .expect_err("incompatible history blocks migration");
    let sqlx::migrate::MigrateError::ExecuteMigration(error, version) = error else {
        panic!("unexpected migration error: {error}")
    };
    assert_eq!(version, NATIVE_RECEIPTS_VERSION);
    let database = error.as_database_error().expect("database refusal");
    assert_eq!(database.code().as_deref(), Some(expected_code));
    if expected_code == "P0001" {
        assert_eq!(
            database.message(),
            "webhook receipt migration refused duplicate exact identities"
        );
        assert!(
            database
                .try_downcast_ref::<sqlx::postgres::PgDatabaseError>()
                .expect("postgres error")
                .detail()
                .is_none()
        );
    }
    let after: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(webhook_receipts) FROM webhook_receipts ORDER BY identity_hash",
    )
    .fetch_all(pool)
    .await
    .expect("old schema and receipts remain");
    assert_eq!(after, before);
    let applied: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version = $1)")
            .bind(NATIVE_RECEIPTS_VERSION)
            .fetch_one(pool)
            .await
            .expect("history unchanged");
    assert!(!applied);
    super::close(&[pool]).await;
}

#[sqlx::test(migrations = false)]
async fn receipt_migration_rejects_duplicate_exact_pairs_without_identity_diagnostics(
    pool: PgPool,
) {
    historical_schema(&pool).await;
    historical_receipt(&pool, 1, "private-endpoint", b"private-message").await;
    historical_receipt(&pool, 2, "private-endpoint", b"private-message").await;
    assert_receipt_migration_rollback(&pool, "P0001").await;
}

#[sqlx::test(migrations = false)]
async fn receipt_migration_uses_actual_index_admission_and_rolls_back_oversized_history(
    pool: PgPool,
) {
    historical_schema(&pool).await;
    let wide: Vec<u8> = sqlx::query_scalar("SELECT convert_to(string_agg(md5(n::text), '' ORDER BY n), 'UTF8') FROM generate_series(1, 400) n")
        .fetch_one(&pool).await.expect("incompressible historical identity");
    historical_receipt(&pool, 1, ENDPOINT, &wide).await;
    assert_receipt_migration_rollback(&pool, "54000").await;
}
