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

fn signed_headers(keys: &KeyRing, message_id: &str, body: &[u8]) -> HeaderMap {
    let timestamp: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
        .try_into()
        .expect("timestamp fits i64");
    let mut headers = HeaderMap::new();
    headers.insert(
        "webhook-id",
        HeaderValue::from_str(message_id).expect("message id"),
    );
    headers.insert(
        "webhook-timestamp",
        HeaderValue::from_str(&timestamp.to_string()).expect("timestamp"),
    );
    headers.insert(
        "webhook-signature",
        HeaderValue::from_str(
            &keys
                .signatures(message_id.as_bytes(), timestamp, body)
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
async fn receiver_atomically_accepts_duplicates_and_conflicts(pool: PgPool) {
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
        Ok(ReceiptOutcome::Conflict)
    );
    assert_eq!(receipt_count(&pool).await, 1);
    assert_eq!(job_count(&pool).await, 1);
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
    let (proxy, proxied) = proxied_pool(&pool).await;
    let body = b"{\"lost_ack\":true}";
    let headers = signed_headers(&keys, "message-unknown", body);
    proxy.arm((Fault::ForwardThenDrop, "INSERT INTO webhook_receipts"));
    assert_eq!(
        receiver(proxied.clone())
            .receive(ENDPOINT, &headers, body, SystemTime::now())
            .await,
        Err(ReceiveError::Unavailable)
    );
    assert_eq!(proxy.fired(), Some(Fault::ForwardThenDrop));
    assert_eq!(receipt_count(&pool).await, 1);
    assert_eq!(job_count(&pool).await, 1);
    assert_eq!(
        receiver(pool.clone())
            .receive(ENDPOINT, &headers, body, SystemTime::now())
            .await,
        Ok(ReceiptOutcome::Duplicate)
    );
    super::close(&[&pool, &proxied]).await;
    proxy.shutdown().await;
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
    accept_for_processing(&pool, "message-processor-unknown").await;
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
    let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM webhook_effects")
        .fetch_one(&pool)
        .await
        .expect("effects count");
    assert_eq!(effects, 1);
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
async fn missing_consumer_snoozes_without_spending_an_attempt(pool: PgPool) {
    accept_for_processing(&pool, "message-missing-consumer").await;
    let running = RunningProcessor::start(&pool, Consumers::new());
    until("the missing binding snoozes the job", async || {
        let row = sqlx::query(
            "SELECT state, attempts FROM background_jobs WHERE kind = 'webhooks.process'",
        )
        .fetch_one(&pool)
        .await
        .expect("job row");
        (row.try_get::<String, _>("state").expect("state") == "pending"
            && row.try_get::<i16, _>("attempts").expect("attempts") == 0)
            .then_some(())
    })
    .await;
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
