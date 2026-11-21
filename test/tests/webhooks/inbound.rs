use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, StatusCode};
use infra_jobs::{Engine, JobError, KindsBuilder};
use infra_postgres::{Pool, Tx};
use infra_webhooks::inbound::{
    Consumer, Consumers, Incoming, ReceiptOutcome, ReceiveError, Receiver, Rejection, Verifier,
    Verifiers,
};
use infra_webhooks::protocol::KeyRing;
use integration_tests::{dsn_for, poll_until};
use sqlx::{PgPool, Row};
use tokio::sync::Notify;
use tokio_util::sync::{CancellationToken, DropGuard};
use tokio_util::task::TaskTracker;

use super::commit_proxy::{Boundary, CommitProxy, Fault};

const ENDPOINT: &str = "partner/a?#";
const KEY: &str = "whsec_Q0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0M=";
const POLL: Duration = Duration::from_millis(20);

fn receiver(pool: Pool) -> Receiver {
    let mut verifiers = Verifiers::new();
    verifiers.bind(ENDPOINT, KeyRing::from_encoded(KEY, None).expect("key"));
    verifiers.into_receiver(pool)
}

fn admission_deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + infra_postgres::ACQUIRE_TIMEOUT
}

#[tokio::test(start_paused = true)]
async fn unfinished_webhook_upload_returns_the_declared_timeout() {
    use axum::body::{Body, to_bytes};
    use infra_webhooks::inbound::route::{WebhookState, router};
    use tower::ServiceExt as _;

    let pool = infra_postgres::test_support::connect_lazy("postgres://localhost/unused")
        .expect("lazy pool");
    let app = mounted_receiver(receiver(pool), Duration::from_secs(1));
    let upload =
        Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::io::Error>>());
    let response = app
        .oneshot(
            http::Request::post("/webhooks/partner%2Fa%3F%23")
                .body(upload)
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(
        response.headers()["content-type"],
        "application/problem+json"
    );
    let bytes = to_bytes(response.into_body(), 4096)
        .await
        .expect("problem body");
    let problem: serde_json::Value = serde_json::from_slice(&bytes).expect("problem JSON");
    assert_eq!(problem["code"], "request_timeout");
    let document = router::<WebhookState>()
        .expect("webhook operation registers")
        .into_openapi()
        .expect("valid webhook contract");
    let document = serde_json::to_value(document).expect("document");
    assert_eq!(
        document["paths"]["/webhooks/{endpoint_id}"]["post"]["responses"]["408"]["$ref"],
        "#/components/responses/RequestTimeout"
    );
}

fn mounted_receiver(receiver: Receiver, budget: Duration) -> infra_http::Application {
    mounted_receiver_with_limit(
        receiver,
        budget,
        infra_webhooks::protocol::MAX_BODY_BYTES + 1,
    )
}

fn mounted_receiver_with_limit(
    receiver: Receiver,
    budget: Duration,
    max_body_bytes: usize,
) -> infra_http::Application {
    infra_webhooks::inbound::route::router()
        .expect("webhook operation registers")
        .build(
            infra_webhooks::inbound::route::WebhookState::active(receiver),
            infra_http::Authentication::Public,
            &infra_http::HardenOptions {
                max_body_bytes,
                request_timeout: budget,
                max_in_flight: None,
                admission_wait: budget,
                log_health_probes: false,
                trust_trace_context: false,
                draining: None,
            },
        )
        .expect("webhook operation is public")
}

fn signed_headers(keys: &KeyRing, message_id: impl AsRef<[u8]>, body: &[u8]) -> HeaderMap {
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut headers = HeaderMap::new();
    keys.write_headers(&mut headers, message_id.as_ref(), timestamp, body)
        .expect("signed webhook headers");
    headers.insert(
        "content-type",
        HeaderValue::from_bytes(b"application/webhook\xff").expect("opaque content type"),
    );
    headers
}

fn unsigned_headers(message_id: impl AsRef<[u8]>) -> HeaderMap {
    let message_id = message_id.as_ref();
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut headers = HeaderMap::new();
    headers.insert(
        "webhook-id",
        HeaderValue::from_bytes(message_id).expect("message id"),
    );
    headers.insert(
        "webhook-timestamp",
        HeaderValue::from_str(&timestamp.to_string()).expect("timestamp"),
    );
    headers.insert("webhook-signature", HeaderValue::from_static("v1,unsigned"));
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

async fn proxied_pool(pool: &PgPool) -> (CommitProxy, Pool) {
    let (proxy, proxied) = CommitProxy::start_for(pool).await;
    (proxy, super::template_pool(&proxied, 3).await)
}

#[test]
fn incoming_preserves_base64_json_bytes_and_decodes_original_body() {
    for (body, encoded) in [
        (Vec::new(), String::new()),
        (vec![0], "AA==".to_owned()),
        (vec![0, 255], "AP8=".to_owned()),
        (vec![0, 255, 65], "AP9B".to_owned()),
        (vec![251, 255], "+/8=".to_owned()),
        ([0, 255, 65].repeat(21_846), "AP9B".repeat(21_846)),
        (
            [[0, 255, 65].repeat(21_846), vec![0]].concat(),
            format!("{}AA==", "AP9B".repeat(21_846)),
        ),
        (
            [[0, 255, 65].repeat(21_846), vec![0, 255]].concat(),
            format!("{}AP8=", "AP9B".repeat(21_846)),
        ),
    ] {
        for (content_type, encoded_type) in [
            (None, "null"),
            (Some(b"".as_slice()), "\"\""),
            (Some(b"\xff".as_slice()), "\"/w==\""),
        ] {
            let expected = format!(
                "{{\"version\":1,\"endpoint_id\":\"partner/a?#\",\"message_id\":\"aWSA/w==\",\"content_type\":{encoded_type},\"body\":\"{encoded}\"}}"
            );
            let incoming: Incoming = serde_json::from_str(&expected).expect("historical payload");
            assert_eq!(incoming.endpoint_id(), ENDPOINT);
            assert_eq!(incoming.message_id(), b"id\x80\xff");
            assert_eq!(incoming.content_type(), content_type);
            assert_eq!(incoming.body(), body);
            assert_eq!(
                serde_json::to_vec(&incoming).expect("payload bytes"),
                expected.as_bytes()
            );
        }
    }
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn receiver_preserves_first_admission_on_authenticated_changed_replay(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let receiver = receiver(app_pool.clone());
    let body = b"\0{\"event\":\"created\"}\xff";
    let headers = signed_headers(&keys, "message-1", body);

    assert_eq!(
        receiver
            .receive(
                ENDPOINT,
                &headers,
                body,
                SystemTime::now(),
                admission_deadline(),
            )
            .await,
        Ok(ReceiptOutcome::Accepted)
    );
    let first: String = sqlx::query_scalar("SELECT received_at::text FROM webhook_receipts")
        .fetch_one(&pool)
        .await
        .expect("first admission time");
    assert_eq!(
        receiver
            .receive_bytes(
                ENDPOINT,
                &headers,
                Bytes::from_static(body),
                SystemTime::now(),
                admission_deadline(),
            )
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
                admission_deadline(),
            )
            .await,
        Ok(ReceiptOutcome::Duplicate)
    );
    let mut replay_headers = signed_headers(&keys, "message-1", changed);
    replay_headers.insert("content-type", HeaderValue::from_static("text/plain"));
    assert_eq!(
        receiver
            .receive_bytes(
                ENDPOINT,
                &replay_headers,
                Bytes::from_static(changed),
                SystemTime::now(),
                admission_deadline(),
            )
            .await,
        Ok(ReceiptOutcome::Duplicate)
    );
    replay_headers.insert("webhook-signature", HeaderValue::from_static("v1,invalid"));
    assert_eq!(
        receiver
            .receive_bytes(
                ENDPOINT,
                &replay_headers,
                Bytes::from_static(changed),
                SystemTime::now(),
                admission_deadline(),
            )
            .await,
        Err(ReceiveError::Rejected(Rejection::new("invalid_signature")))
    );
    assert_eq!(receipt_count(&pool).await, 1);
    assert_eq!(job_count(&pool).await, 1);
    let timestamp: String = sqlx::query_scalar("SELECT received_at::text FROM webhook_receipts")
        .fetch_one(&pool)
        .await
        .expect("unchanged admission time");
    assert_eq!(timestamp, first);
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload_text::jsonb FROM background_jobs")
            .fetch_one(&pool)
            .await
            .expect("original processing job");
    assert_eq!(payload["version"], 1);
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
    super::close_pools(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn concurrent_verified_copies_converge_on_one_receipt_and_job(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let first = receiver(app_pool.clone());
    let second = receiver(app_pool.clone());
    let body = b"{\"winner\":\"borrowed\"}";
    let owned_body = b"{\"winner\":\"owned\"}\0\xff";
    let headers = signed_headers(&keys, "message-race", body);
    let owned_headers = signed_headers(&keys, "message-race", owned_body);
    let (one, two) = tokio::join!(
        first.receive(
            ENDPOINT,
            &headers,
            body,
            SystemTime::now(),
            admission_deadline(),
        ),
        second.receive_bytes(
            ENDPOINT,
            &owned_headers,
            Bytes::from_static(owned_body),
            SystemTime::now(),
            admission_deadline(),
        ),
    );
    std::assert_matches!(
        (&one, &two),
        (Ok(ReceiptOutcome::Accepted), Ok(ReceiptOutcome::Duplicate))
            | (Ok(ReceiptOutcome::Duplicate), Ok(ReceiptOutcome::Accepted))
    );
    assert_eq!(receipt_count(&pool).await, 1);
    assert_eq!(job_count(&pool).await, 1);
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload_text::jsonb FROM background_jobs")
            .fetch_one(&pool)
            .await
            .expect("winning processing job");
    let incoming: Incoming = serde_json::from_value(payload).expect("incoming");
    let expected = if one == Ok(ReceiptOutcome::Accepted) {
        body.as_slice()
    } else {
        owned_body.as_slice()
    };
    assert_eq!(incoming.body(), expected);
    assert_eq!(incoming.message_id(), b"message-race");
    super::close_pools(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn admission_waits_for_a_connection_until_the_callers_deadline(pool: PgPool) {
    let single = super::template_pool(&dsn_for(&pool).await, 1).await;
    let held = single
        .acquire("hold the only connection")
        .await
        .expect("hold the only connection");
    let release = tokio::spawn(async move {
        tokio::time::sleep(infra_postgres::ACQUIRE_TIMEOUT + Duration::from_millis(500)).await;
        drop(held);
    });
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let body = b"{}";
    let outcome = receiver(single.clone())
        .receive(
            ENDPOINT,
            &signed_headers(&keys, "message-late-connection", body),
            body,
            SystemTime::now(),
            tokio::time::Instant::now() + infra_postgres::ACQUIRE_TIMEOUT * 2,
        )
        .await;
    assert_eq!(outcome, Ok(ReceiptOutcome::Accepted));
    release.await.expect("release task");
    super::close_pools(&[&single]).await;
    pool.close().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn a_blocked_enqueue_expires_and_the_same_delivery_commits_once(pool: PgPool) {
    use sqlx::Connection as _;
    use tokio::time::Instant;

    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let mut connection = app_pool.acquire("hold webhook enqueue").await.unwrap();
    let mut holder = connection.begin().await.unwrap();
    sqlx::query("LOCK TABLE background_jobs IN SHARE MODE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let body = b"{\"rollback\":true}";
    let headers = signed_headers(&keys, "message-rollback", body);
    let receiver = receiver(app_pool.clone());
    let receive = |deadline| {
        receiver.receive_bytes(
            ENDPOINT,
            &headers,
            Bytes::from_static(body),
            SystemTime::now(),
            deadline,
        )
    };
    let cutoff = Instant::now() + Duration::from_secs(2);
    let observe_waiter =
        poll_until(
            "the receipt reached its blocked enqueue",
            Duration::from_secs(2),
            POLL,
            async || {
                let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) \
             AND query LIKE 'INSERT INTO background_jobs%')"
        ).bind(holder_pid).fetch_one(&pool).await.unwrap();
                waiting.then_some(())
            },
        );
    let ((), outcome) = tokio::join!(
        observe_waiter,
        tokio::time::timeout_at(cutoff + Duration::from_secs(1), receive(cutoff))
    );
    let outcome = outcome.expect("the enqueue's server deadline fires while the lock is held");
    assert_eq!(outcome, Err(ReceiveError::Unavailable));
    assert_eq!(receipt_count(&pool).await, 0);
    assert_eq!(job_count(&pool).await, 0);
    holder.rollback().await.unwrap();
    drop(connection);
    assert_eq!(
        receive(admission_deadline()).await,
        Ok(ReceiptOutcome::Accepted)
    );
    assert_eq!(
        receive(admission_deadline()).await,
        Ok(ReceiptOutcome::Duplicate)
    );
    assert_eq!(receipt_count(&pool).await, 1);
    assert_eq!(job_count(&pool).await, 1);
    super::close_pools(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn lost_receipt_commit_acknowledgement_returns_unavailable_and_retry_converges(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
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
        proxy.arm(
            fault,
            Boundary::Commit(Some("INSERT INTO webhook_receipts")),
        );
        assert_eq!(
            receiver(proxied.clone())
                .receive_bytes(
                    ENDPOINT,
                    &headers,
                    Bytes::from_static(body),
                    SystemTime::now(),
                    admission_deadline(),
                )
                .await,
            Err(ReceiveError::Unavailable)
        );
        assert_eq!(proxy.fired(), Some(fault));
        assert_eq!(receipt_count(&pool).await, previous + i64::from(committed));
        assert_eq!(job_count(&pool).await, previous + i64::from(committed));
        assert_eq!(
            receiver(app_pool.clone())
                .receive(
                    ENDPOINT,
                    &headers,
                    body,
                    SystemTime::now(),
                    admission_deadline(),
                )
                .await,
            Ok(retry_outcome)
        );
        assert_eq!(receipt_count(&pool).await, previous + 1);
        assert_eq!(job_count(&pool).await, previous + 1);
        super::close_pools(&[&proxied]).await;
        proxy.shutdown().await;
    }
    super::close_pools(&[&app_pool]).await;
}

#[derive(Default)]
struct EffectConsumer {
    entered: Notify,
    release: Notify,
}

impl Consumer for EffectConsumer {
    async fn process(&self, tx: &mut Tx<'_>, incoming: &Incoming) -> Result<(), JobError> {
        sqlx::query("INSERT INTO webhook_effects (message_id, content_type) VALUES ($1, $2)")
            .bind(incoming.message_id())
            .bind(incoming.content_type())
            .execute(&mut *tx)
            .await?;
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

struct RunningProcessor {
    started: infra_jobs::Started,
    cancel: CancellationToken,
    tracker: TaskTracker,
    _cancel_guard: DropGuard,
}

impl RunningProcessor {
    fn start(pool: &Pool, consumers: Consumers) -> Self {
        let mut kinds = KindsBuilder::new();
        consumers
            .into_processor([])
            .expect("webhook processor")
            .register(&mut kinds);
        let engine = Engine::new(
            pool.clone(),
            kinds.validate().expect("webhook processor registry"),
            std::num::NonZeroU32::MIN,
        );
        let cancel = CancellationToken::new();
        let tracker = TaskTracker::new();
        let started = engine.start(&tracker, &cancel);
        tracker.close();
        let cancel_guard = cancel.clone().drop_guard();
        Self {
            started,
            cancel,
            tracker,
            _cancel_guard: cancel_guard,
        }
    }

    fn with_consumer(pool: &Pool, consumer: impl Consumer) -> Self {
        let mut consumers = Consumers::new();
        consumers.bind(ENDPOINT, consumer).expect("first binding");
        Self::start(pool, consumers)
    }

    async fn close(self, pools: &[&Pool]) {
        self.cancel.cancel();
        super::bounded("the processor tasks join", self.tracker.wait()).await;
        super::close_pools(pools).await;
    }
}

async fn accept_for_processing(pool: &Pool, message_id: &str) {
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let body = b"{\"process\":true}";
    assert_eq!(
        receiver(pool.clone())
            .receive(
                ENDPOINT,
                &signed_headers(&keys, message_id, body),
                body,
                SystemTime::now(),
                admission_deadline(),
            )
            .await,
        Ok(ReceiptOutcome::Accepted)
    );
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn consumer_sql_errors_preserve_safe_codes_through_retry_and_exhaustion(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    sqlx::raw_sql(
        "CREATE TABLE webhook_effects (message_id bytea PRIMARY KEY, content_type bytea);
         INSERT INTO webhook_effects (message_id) VALUES ('private-message-id');
         CREATE FUNCTION inject_deadlock() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN RAISE deadlock_detected USING MESSAGE = 'private deadlock detail'; END $$;
         CREATE TRIGGER inject_deadlock BEFORE INSERT ON webhook_effects
             FOR EACH ROW EXECUTE FUNCTION inject_deadlock();",
    )
    .execute(&pool)
    .await
    .expect("consumer effect with a duplicate key and an injected deadlock");
    accept_for_processing(&app_pool, "private-message-id").await;
    sqlx::query("UPDATE background_jobs SET attempts = 23 WHERE kind = 'webhooks.process'")
        .execute(&pool)
        .await
        .expect("two attempts remain");
    let running = RunningProcessor::with_consumer(&app_pool, EffectConsumer::default());
    let summary = poll_until(
        "the deadlock schedules a retry",
        super::WAIT,
        POLL,
        async || {
            sqlx::query_scalar::<_, String>(
                "SELECT error_summary FROM background_jobs
                 WHERE kind = 'webhooks.process' AND state = 'pending' AND attempts = 24",
            )
            .fetch_optional(&pool)
            .await
            .expect("attempt summary")
        },
    )
    .await;
    assert_eq!(summary, "postgres 40P01");

    sqlx::raw_sql(
        "DROP TRIGGER inject_deadlock ON webhook_effects;
         UPDATE background_jobs SET not_before = now() WHERE kind = 'webhooks.process';",
    )
    .execute(&pool)
    .await
    .expect("the last attempt can reach the duplicate key");
    let (attempts, reason, summary, errors) = poll_until(
        "the attempt budget is exhausted",
        super::WAIT,
        POLL,
        async || {
            sqlx::query_as::<_, (i16, String, String, Vec<String>)>(
                "SELECT attempts, failure_reason, error_summary,
                        ARRAY(SELECT entry->>'error'
                              FROM jsonb_array_elements(errors) WITH ORDINALITY AS history(entry, position)
                              ORDER BY position)
                 FROM background_jobs WHERE kind = 'webhooks.process' AND state = 'failed'",
            )
            .fetch_optional(&pool)
            .await
            .expect("terminal attempt and history")
        },
    )
    .await;
    assert_eq!(attempts, 25);
    assert_eq!(reason, "exhausted");
    assert_eq!(summary, "postgres 23505");
    assert_eq!(errors, ["postgres 40P01", "postgres 23505"]);
    running.close(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn stale_fenced_completion_rolls_back_consumer_effects(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    sqlx::query("CREATE TABLE webhook_effects (message_id bytea NOT NULL, content_type bytea)")
        .execute(&pool)
        .await
        .expect("consumer effects table");
    accept_for_processing(&app_pool, "message-stale").await;
    let consumer = Arc::new(EffectConsumer::default());
    let running = RunningProcessor::with_consumer(&app_pool, Arc::clone(&consumer));
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
    poll_until(
        "the stale processor attempt ends",
        super::WAIT,
        POLL,
        async || (running.started.in_flight() == 0).then_some(()),
    )
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
    running.close(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn unknown_processor_commit_does_not_add_a_competing_retry(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    sqlx::query("CREATE TABLE webhook_effects (message_id bytea NOT NULL, content_type bytea)")
        .execute(&pool)
        .await
        .expect("consumer effects table");
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
    app_pool
        .in_tx(async |tx| -> Result<(), infra_postgres::TxError> {
            let _id = infra_jobs::enqueue(tx, &incoming)
                .await
                .expect("enqueue historical payload");
            Ok(())
        })
        .await
        .expect("historical accepted job");
    let (proxy, worker) = proxied_pool(&pool).await;
    let consumer = Arc::new(EffectConsumer::default());
    let running = RunningProcessor::with_consumer(&worker, Arc::clone(&consumer));
    super::bounded(
        "the consumer is ready to commit",
        consumer.entered.notified(),
    )
    .await;
    proxy.arm(
        Fault::ForwardThenDrop,
        Boundary::Commit(Some("SET state = 'completed'")),
    );
    consumer.release.notify_one();
    poll_until(
        "the unknown processor attempt ends",
        super::WAIT,
        POLL,
        async || (running.started.in_flight() == 0).then_some(()),
    )
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
    running.close(&[&app_pool, &worker]).await;
    proxy.shutdown().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn missing_consumer_spends_attempts_and_exhausts_the_normal_budget(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    accept_for_processing(&app_pool, "message-missing-consumer").await;
    sqlx::query("UPDATE background_jobs SET attempts = 23 WHERE kind = 'webhooks.process'")
        .execute(&pool)
        .await
        .expect("historical attempt count");
    let running = RunningProcessor::start(&app_pool, Consumers::new());
    poll_until(
        "the missing binding spends its retry attempt",
        super::WAIT,
        POLL,
        async || {
            let row = sqlx::query(
                "SELECT state, attempts FROM background_jobs WHERE kind = 'webhooks.process'",
            )
            .fetch_one(&pool)
            .await
            .expect("job row");
            (row.try_get::<String, _>("state").expect("state") == "pending"
                && row.try_get::<i16, _>("attempts").expect("attempts") == 24)
                .then_some(())
        },
    )
    .await;
    sqlx::query(
        "UPDATE background_jobs SET not_before = now() \
         WHERE kind = 'webhooks.process' AND state = 'pending'",
    )
    .execute(&pool)
    .await
    .expect("make the final retry eligible without waiting for backoff");
    let row = poll_until(
        "the missing binding exhausts its budget",
        super::WAIT,
        POLL,
        async || {
            let row = sqlx::query(
                "SELECT state, attempts, failure_reason FROM background_jobs \
                 WHERE kind = 'webhooks.process'",
            )
            .fetch_one(&pool)
            .await
            .expect("job row");
            (row.try_get::<String, _>("state").expect("state") == "failed").then_some(row)
        },
    )
    .await;
    assert_eq!(row.try_get::<i16, _>("attempts").expect("attempts"), 25);
    assert_eq!(
        row.try_get::<String, _>("failure_reason")
            .expect("failure reason"),
        "exhausted"
    );
    running.close(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn mounted_receipt_cutoff_reserves_response_time_and_same_identity_recovers(pool: PgPool) {
    use axum::body::{Body, to_bytes};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt as _;

    struct CountedVerifier(Arc<AtomicUsize>, KeyRing);
    impl Verifier for CountedVerifier {
        fn authenticate(
            &self,
            headers: &HeaderMap,
            body: &[u8],
            now: SystemTime,
        ) -> Result<Bytes, Rejection> {
            self.0.fetch_add(1, Ordering::SeqCst);
            self.1.authenticate(headers, body, now)
        }
    }

    // template:begin http-idempotency:test-webhook-cutoff-metric-recorder
    let recorder = metrics_util::debugging::DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _local = metrics::set_default_local_recorder(&recorder);
    // template:end http-idempotency:test-webhook-cutoff-metric-recorder
    let writer = super::template_pool(&dsn_for(&pool).await, 1).await;
    let held = infra_postgres::test_support::as_sqlx(&writer)
        .acquire()
        .await
        .expect("occupy the only permit");
    let calls = Arc::new(AtomicUsize::new(0));
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let mut verifiers = Verifiers::new();
    verifiers.bind(ENDPOINT, CountedVerifier(Arc::clone(&calls), keys.clone()));
    let receiver = verifiers.into_receiver(writer.clone());
    let request_with = |body| {
        let mut request = http::Request::post("/webhooks/partner%2Fa%3F%23")
            .body(body)
            .expect("request");
        *request.headers_mut() = signed_headers(&keys, "message-budget", b"{}");
        request
    };
    let request = || request_with(Body::from("{}"));

    tokio::time::pause();
    for (budget_ms, delay_ms, verifier_calls) in [(100, 50, 1), (1_000, 900, 2), (1_000, 950, 2)] {
        let budget = Duration::from_millis(budget_ms);
        let app = mounted_receiver(receiver.clone(), budget);
        let start = tokio::time::Instant::now();
        let request = if delay_ms == 950 {
            request_with(Body::from_stream(futures_util::stream::once(async move {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                Ok::<_, std::convert::Infallible>(Bytes::from_static(b"{}"))
            })))
        } else {
            request()
        };
        let response = app.oneshot(request);
        tokio::pin!(response);
        assert!(futures_util::poll!(response.as_mut()).is_pending());
        tokio::time::advance(Duration::from_millis(delay_ms)).await;
        let response = super::bounded("receipt cutoff response", response)
            .await
            .expect("response");
        assert_eq!(calls.load(Ordering::SeqCst), verifier_calls);
        assert!(tokio::time::Instant::now() < start + budget);
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
        let body = to_bytes(response.into_body(), 4096)
            .await
            .expect("problem body");
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem JSON");
        assert_eq!(problem["code"], "gateway_timeout");
    }
    tokio::time::resume();
    assert_eq!(receipt_count(&pool).await, 0);
    assert_eq!(job_count(&pool).await, 0);
    drop(held);
    let app = mounted_receiver(receiver, Duration::from_secs(8));
    for _ in 0..2 {
        let response = super::bounded("same-identity retry", app.clone().oneshot(request()))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    assert_eq!(receipt_count(&pool).await, 1);
    assert_eq!(job_count(&pool).await, 1);
    // template:begin http-idempotency:test-webhook-cutoff-metric-assertions
    let outcomes: std::collections::BTreeMap<String, u64> = snapshotter
        .snapshot()
        .into_vec()
        .into_iter()
        .filter(|(key, _, _, _)| key.key().name() == "webhook_ingress_outcomes_total")
        .filter_map(|(key, _, _, value)| {
            let outcome = key
                .key()
                .labels()
                .find(|label| label.key() == "outcome")?
                .value()
                .to_owned();
            let metrics_util::debugging::DebugValue::Counter(count) = value else {
                return None;
            };
            Some((outcome, count))
        })
        .collect();
    assert_eq!(
        outcomes,
        std::collections::BTreeMap::from([
            ("accepted".to_owned(), 1),
            ("deadline_exceeded".to_owned(), 3),
            ("duplicate".to_owned(), 1),
        ])
    );
    // template:end http-idempotency:test-webhook-cutoff-metric-assertions
    super::close_pools(&[&writer]).await;
    pool.close().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn receipt_identity_preserves_binary_ids_and_endpoint_scope(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let mut verifiers = Verifiers::new();
    verifiers
        .bind("partner", keys.clone())
        .bind("Partner", keys.clone());
    let receiver = verifiers.into_receiver(app_pool.clone());
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
                    SystemTime::now(),
                    admission_deadline(),
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
    super::close_pools(&[&app_pool]).await;
}

struct TokenVerifier;

impl Verifier for TokenVerifier {
    fn authenticate(
        &self,
        headers: &HeaderMap,
        _body: &[u8],
        _now: SystemTime,
    ) -> Result<Bytes, Rejection> {
        if headers.get("x-provider-token").map(HeaderValue::as_bytes) != Some(b"shared") {
            return Err(Rejection::new("invalid_token"));
        }
        let delivery = headers
            .get("x-provider-delivery")
            .ok_or(Rejection::new("missing_delivery_id"))?;
        Ok(Bytes::copy_from_slice(delivery.as_bytes()))
    }
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn a_provider_verifier_shares_the_receipt_path_beside_standard_webhooks(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let mut verifiers = Verifiers::new();
    verifiers
        .bind("standard", keys.clone())
        .bind("provider", TokenVerifier);
    let receiver = verifiers.into_receiver(app_pool.clone());
    let provider_headers = |token: &'static str, delivery: &str| {
        let mut headers = HeaderMap::new();
        headers.insert("x-provider-token", HeaderValue::from_static(token));
        headers.insert(
            "x-provider-delivery",
            HeaderValue::from_str(delivery).expect("delivery id"),
        );
        headers
    };
    let receive = async |endpoint: &str, headers: HeaderMap| {
        receiver
            .receive(
                endpoint,
                &headers,
                b"{}",
                SystemTime::now(),
                admission_deadline(),
            )
            .await
    };

    assert_eq!(
        receive("standard", signed_headers(&keys, "msg_1", b"{}")).await,
        Ok(ReceiptOutcome::Accepted)
    );
    assert_eq!(
        receive("provider", provider_headers("shared", "delivery-1")).await,
        Ok(ReceiptOutcome::Accepted)
    );
    assert_eq!(
        receive("provider", provider_headers("shared", "delivery-1")).await,
        Ok(ReceiptOutcome::Duplicate)
    );
    assert_eq!(
        receive("provider", signed_headers(&keys, "msg_2", b"{}")).await,
        Err(ReceiveError::Rejected(Rejection::new("invalid_token")))
    );
    assert_eq!(
        receive("standard", provider_headers("shared", "delivery-2")).await,
        Err(ReceiveError::Rejected(Rejection::new("missing_header")))
    );
    for identity in [String::new(), "d".repeat(256)] {
        assert_eq!(
            receive("provider", provider_headers("shared", &identity)).await,
            Err(ReceiveError::Rejected(Rejection::new("invalid_message_id")))
        );
    }
    let oversized = vec![b'x'; infra_webhooks::protocol::MAX_BODY_BYTES + 1];
    assert_eq!(
        receiver
            .receive(
                "provider",
                &provider_headers("shared", "delivery-3"),
                &oversized,
                SystemTime::now(),
                admission_deadline(),
            )
            .await,
        Err(ReceiveError::Rejected(Rejection::new("body_too_large")))
    );
    let mut padded = provider_headers("shared", "delivery-4");
    padded.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_str(&"a".repeat(200 * 1024)).expect("content type"),
    );
    assert_eq!(
        receive("provider", padded).await,
        Err(ReceiveError::Rejected(Rejection::new("payload_too_large")))
    );
    assert_eq!(receipt_count(&pool).await, 2);
    assert_eq!(job_count(&pool).await, 2);
    super::close_pools(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn late_redelivery_after_completed_job_retention_preserves_one_consumer_effect(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let id = "redelivery-after-retention";
    sqlx::query("CREATE TABLE webhook_effects (message_id bytea NOT NULL, content_type bytea)")
        .execute(&pool)
        .await
        .expect("consumer effects table");
    accept_for_processing(&app_pool, id).await;
    sqlx::query("UPDATE webhook_receipts SET received_at = now() - interval '1 year'")
        .execute(&pool)
        .await
        .expect("age the original receipt");

    let consumer = Arc::new(EffectConsumer::default());
    consumer.release.notify_one();
    let processing = RunningProcessor::with_consumer(&app_pool, consumer);
    poll_until(
        "the consumer commits its effect and completes the job",
        super::WAIT,
        POLL,
        async || {
            let aged = sqlx::query(
                "UPDATE background_jobs SET finished_at = now() - interval '1 year' \
                 WHERE state = 'completed'",
            )
            .execute(&pool)
            .await
            .expect("age the completed job");
            (aged.rows_affected() == 1).then_some(())
        },
    )
    .await;
    processing.close(&[]).await;

    let restarted = RunningProcessor::start(&app_pool, Consumers::new());
    poll_until(
        "job retention removes the completed work",
        super::WAIT,
        POLL,
        async || (job_count(&pool).await == 0).then_some(()),
    )
    .await;
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    let body = b"late redelivery";
    assert_eq!(
        receiver(app_pool.clone())
            .receive(
                ENDPOINT,
                &signed_headers(&keys, id, body),
                body,
                SystemTime::now(),
                admission_deadline(),
            )
            .await,
        Ok(ReceiptOutcome::Duplicate)
    );
    assert_eq!(job_count(&pool).await, 0);
    let effects: Vec<Vec<u8>> = sqlx::query_scalar("SELECT message_id FROM webhook_effects")
        .fetch_all(&pool)
        .await
        .expect("committed consumer effects");
    assert_eq!(effects, [id.as_bytes()]);
    restarted.close(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn mounted_admission_distinguishes_replay_id_bounds_and_body_failures(pool: PgPool) {
    use axum::body::{Body, to_bytes};
    use tower::ServiceExt as _;

    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let app = mounted_receiver(receiver(app_pool.clone()), Duration::from_secs(8));
    let keys = KeyRing::from_encoded(KEY, None).expect("key");
    for (id, body, content_type, status) in [
        (
            vec![b'm'; 255],
            b"\0first\xff".as_slice(),
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
        *request.headers_mut() = if id.len() > 255 {
            unsigned_headers(&id)
        } else {
            signed_headers(&keys, &id, body)
        };
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
                #[expect(unreachable_code, reason = "the item type names the stream's Result")]
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
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload_text::jsonb FROM background_jobs")
            .fetch_one(&pool)
            .await
            .expect("mounted processing job");
    let incoming: Incoming = serde_json::from_value(payload).expect("incoming");
    assert_eq!(incoming.endpoint_id(), ENDPOINT);
    assert_eq!(incoming.message_id(), vec![b'm'; 255]);
    assert_eq!(incoming.content_type(), Some(b"text/plain".as_slice()));
    assert_eq!(incoming.body(), b"\0first\xff");
    super::close_pools(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn a_server_limit_below_the_webhook_limit_answers_413_without_content_length(pool: PgPool) {
    use axum::body::{Body, to_bytes};
    use tower::ServiceExt as _;

    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let app = mounted_receiver_with_limit(receiver(app_pool.clone()), Duration::from_secs(8), 16);
    let body = Body::from_stream(futures_util::stream::once(async {
        Ok::<Bytes, std::io::Error>(Bytes::from_static(&[b'x'; 32]))
    }));
    let response = app
        .oneshot(
            http::Request::post("/webhooks/partner%2Fa%3F%23")
                .body(body)
                .expect("request"),
        )
        .await
        .expect("mounted response");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let problem: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.expect("problem"))
            .expect("JSON");
    assert_eq!(problem["code"], "request_entity_too_large");
    assert_eq!(receipt_count(&pool).await, 0);
    super::close_pools(&[&app_pool]).await;
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
    sqlx::query(
        "INSERT INTO background_jobs (kind, payload, not_before) \
         VALUES ($1, $2, statement_timestamp())",
    )
    .bind(<Incoming as infra_jobs::JobKind>::NAME.as_str())
    .bind(serde_json::to_value(&incoming).expect("serialize historical payload"))
    .execute(&pool)
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
    let mut jobs_after: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT to_jsonb(background_jobs) FROM background_jobs ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("jobs after");
    for job in &mut jobs_after {
        let row = job.as_object_mut().expect("job row is a JSON object");
        for column in ["payload_text", "claim_token", "predecessor_job_id"] {
            assert_eq!(
                row.remove(column),
                Some(serde_json::Value::Null),
                "{column}"
            );
        }
        for column in ["errors", "recovery_history"] {
            assert_eq!(row.remove(column), Some(serde_json::json!([])), "{column}");
        }
    }
    assert_eq!(jobs_after, jobs_before);
    let approximation: bool = sqlx::query_scalar("SELECT count(DISTINCT received_at) = 1 AND bool_and(received_at >= $1::text::timestamptz AND received_at <= now()) FROM webhook_receipts")
        .bind(before).fetch_one(&pool).await.expect("migration timestamp approximation");
    assert!(approximation);
    assert_eq!(
        receiver(infra_postgres::test_support::pool_from_sqlx(pool.clone()))
            .receive(
                ENDPOINT,
                &unsigned_headers(&legacy),
                b"legacy",
                SystemTime::now(),
                admission_deadline(),
            )
            .await,
        Err(ReceiveError::Rejected(Rejection::new("invalid_message_id")))
    );
    assert_eq!(receipt_count(&pool).await, 3);
    assert_eq!(job_count(&pool).await, 1);
    pool.close().await;
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
    pool.close().await;
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
