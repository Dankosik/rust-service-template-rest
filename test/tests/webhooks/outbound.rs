use std::{collections::BTreeMap, num::NonZeroU32, sync::Arc, time::Duration};

use base64::Engine as _;
use infra_jobs::{Engine, Kinds};
use infra_postgres::{PgPool, TxError, in_tx};
use infra_webhooks::{
    outbound::{Dispatcher, Endpoint, Outbound, OutboundError},
    protocol::KeyRing,
};
use sqlx::Row;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
    task::JoinHandle,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const FIXTURE_HOST: &str = "authn.fixture.test";
const CURRENT_KEY: &str = "whsec_Y3VycmVudA==";
const PREVIOUS_KEY: &str = "whsec_cHJldmlvdXM=";

#[derive(Debug)]
enum Step {
    Delivery(OutboundError),
    Transaction(TxError),
    Rejected,
}

impl From<TxError> for Step {
    fn from(error: TxError) -> Self {
        Self::Transaction(error)
    }
}

fn explain(error: &Step) -> String {
    match error {
        Step::Delivery(error) => format!("delivery: {error}"),
        Step::Transaction(error) => format!("transaction: {error}"),
        Step::Rejected => "caller rejected the transaction".to_owned(),
    }
}

fn endpoint(path_and_query: &str) -> Endpoint {
    Endpoint::new(format!("https://{FIXTURE_HOST}{path_and_query}"))
}

fn outbound(endpoints: &[(&str, &str)]) -> Outbound {
    Outbound::new(
        endpoints
            .iter()
            .map(|(id, path)| ((*id).to_owned(), endpoint(path)))
            .collect(),
    )
    .expect("fixture endpoint is admitted")
}

fn keys(endpoints: &[&str]) -> BTreeMap<String, KeyRing> {
    endpoints
        .iter()
        .map(|endpoint| {
            (
                (*endpoint).to_owned(),
                KeyRing::from_encoded(CURRENT_KEY, Some(PREVIOUS_KEY)).expect("fixture keys"),
            )
        })
        .collect()
}

fn dispatcher(
    outbound: &Outbound,
    endpoints: &[&str],
    address: std::net::SocketAddr,
) -> Dispatcher {
    outbound
        .dispatcher_for_test_http(keys(endpoints), address)
        .expect("fixture dispatcher")
}

async fn enqueue(pool: &PgPool, outbound: &Outbound, endpoint: &str, body: &[u8]) -> String {
    let prepared = outbound
        .prepare(
            endpoint,
            body.to_vec(),
            Some("application/webhook+json".to_owned()),
        )
        .expect("delivery is prepared");
    let id = in_tx(pool, async |tx| -> Result<_, Step> {
        prepared.enqueue(tx).await.map_err(Step::Delivery)
    })
    .await
    .unwrap_or_else(|error| panic!("delivery commits: {}", explain(&error)));
    id.to_string()
}

async fn job_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE kind = 'webhooks.deliver'")
        .fetch_one(pool)
        .await
        .expect("delivery count")
}

struct RunningDispatcher {
    cancel: CancellationToken,
    tracker: TaskTracker,
}

impl Drop for RunningDispatcher {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.tracker.close();
    }
}

impl RunningDispatcher {
    fn start(pool: &PgPool, dispatcher: Dispatcher, workers: u32) -> Self {
        let mut kinds = Kinds::new();
        dispatcher.register(&mut kinds);
        let engine = Engine::new(
            pool.clone(),
            kinds.validate().expect("delivery registry"),
            NonZeroU32::new(workers).expect("at least one worker"),
        );
        let cancel = CancellationToken::new();
        let tracker = TaskTracker::new();
        let _started = engine.start(&tracker, &cancel);
        Self { cancel, tracker }
    }

    async fn stop(self) {
        self.cancel.cancel();
        self.tracker.close();
        super::bounded("the delivery tasks join", self.tracker.wait()).await;
    }
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

async fn job_is(pool: &PgPool, id: &str, state: &str, attempts: i16) {
    until("the delivery reaches its persisted outcome", async || {
        let row = sqlx::query("SELECT state, attempts FROM background_jobs WHERE id::text = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("delivery row");
        (row.try_get::<String, _>("state").expect("state") == state
            && row.try_get::<i16, _>("attempts").expect("attempts") == attempts)
            .then_some(())
    })
    .await;
}

async fn job_is_snoozed(pool: &PgPool, id: &str) {
    until("the at-capacity delivery is durably snoozed", async || {
        let row = sqlx::query(
            "SELECT state, attempts, not_before > clock_timestamp() AS deferred \
             FROM background_jobs WHERE id::text = $1",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("delivery row");
        (row.try_get::<String, _>("state").expect("state") == "pending"
            && row.try_get::<i16, _>("attempts").expect("attempts") == 0
            && row.try_get::<bool, _>("deferred").expect("deferred"))
        .then_some(())
    })
    .await;
}

async fn read_headers<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> Vec<u8> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 256];
    let header_end = loop {
        if let Some(position) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream
            .read(&mut chunk)
            .await
            .expect("fixture reads request");
        assert!(read > 0, "fixture receives a complete request");
        request.extend_from_slice(&chunk[..read]);
        assert!(
            request.len() <= 4096,
            "fixture request headers remain bounded"
        );
    };
    let content_length = header(&request[..header_end], "content-length")
        .parse::<usize>()
        .expect("content length is an integer");
    while request.len() < header_end + content_length {
        let read = stream
            .read(&mut chunk)
            .await
            .expect("fixture reads request body");
        assert!(read > 0, "fixture receives the complete request body");
        request.extend_from_slice(&chunk[..read]);
    }
    request
}

async fn reply_peer(
    status: u16,
) -> (
    std::net::SocketAddr,
    tokio::sync::oneshot::Receiver<Vec<u8>>,
    JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let (sent, captured) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("fixture accepts connection");
        let headers = read_headers(&mut stream).await;
        sent.send(headers)
            .expect("request capture remains available");
        let response =
            format!("HTTP/1.1 {status} fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        stream
            .write_all(response.as_bytes())
            .await
            .expect("fixture writes response");
        stream.shutdown().await.expect("fixture closes response");
    });
    (address, captured, server)
}

fn header(headers: &[u8], name: &str) -> String {
    let header_end = headers
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .map_or(headers.len(), |position| position + 4);
    let text =
        std::str::from_utf8(&headers[..header_end]).expect("fixture request is ASCII headers");
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(field, _)| field.eq_ignore_ascii_case(name))
        .map_or_else(
            || panic!("request contains {name}"),
            |(_, value)| value.trim().to_owned(),
        )
}

fn request_body(request: &[u8]) -> &[u8] {
    let boundary = request
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .expect("request header boundary");
    &request[boundary + 4..]
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn producer_commits_only_v2_common_fields_and_preserves_raw_body(pool: PgPool) {
    let configured = outbound(&[("partner", "/events?source=producer")]);
    let body = b"\0{\"event\":\"created\"}\xff";
    let id = enqueue(&pool, &configured, "partner", body).await;
    let row = sqlx::query(
        "SELECT id::text AS id, payload->>'version' AS version, payload->>'endpoint_id' AS endpoint_id, \
         payload->>'content_type' AS content_type, payload ? 'destination' AS has_destination, \
         payload ? 'active_key' AS has_active_key, payload ? 'previous_key' AS has_previous_key, \
         payload->>'body' AS body FROM background_jobs WHERE id::text = $1",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .expect("delivery row");
    assert_eq!(row.try_get::<String, _>("id").expect("id"), id);
    assert_eq!(row.try_get::<String, _>("version").expect("version"), "2");
    assert_eq!(
        row.try_get::<String, _>("endpoint_id").expect("endpoint"),
        "partner"
    );
    assert_eq!(
        row.try_get::<String, _>("content_type")
            .expect("content type"),
        "application/webhook+json"
    );
    assert!(
        !row.try_get::<bool, _>("has_destination")
            .expect("destination")
    );
    assert!(
        !row.try_get::<bool, _>("has_active_key")
            .expect("active key")
    );
    assert!(
        !row.try_get::<bool, _>("has_previous_key")
            .expect("previous key")
    );
    assert_eq!(
        row.try_get::<String, _>("body").expect("body"),
        base64::engine::general_purpose::STANDARD.encode(body)
    );
    super::close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn caller_rollback_leaves_no_delivery(pool: PgPool) {
    let configured = outbound(&[("partner", "/events?source=rollback")]);
    let prepared = configured
        .prepare("partner", b"{\"event\":\"rolled-back\"}".to_vec(), None)
        .expect("prepared delivery");
    let result = in_tx(&pool, async |tx| -> Result<(), Step> {
        let _id = prepared.enqueue(tx).await.map_err(Step::Delivery)?;
        Err(Step::Rejected)
    })
    .await;
    assert!(matches!(&result, Err(Step::Rejected)), "{result:?}");
    assert_eq!(job_count(&pool).await, 0);
    super::close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn queued_statuses_retry_except_for_gone_and_complete_on_any_2xx(pool: PgPool) {
    for (status, expected_state) in [
        (401, "pending"),
        (404, "pending"),
        (410, "failed"),
        (204, "completed"),
    ] {
        let (address, captured, peer) = reply_peer(status).await;
        let configured = outbound(&[("partner", "/events?source=queue")]);
        let id = enqueue(&pool, &configured, "partner", b"{\"event\":\"status\"}").await;
        let running =
            RunningDispatcher::start(&pool, dispatcher(&configured, &["partner"], address), 1);

        job_is(&pool, &id, expected_state, 1).await;
        let row = sqlx::query(
            "SELECT failure_reason, error_summary FROM background_jobs WHERE id::text = $1",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .expect("delivery disposition");
        match status {
            401 | 404 => {
                assert!(
                    row.try_get::<Option<String>, _>("failure_reason")
                        .expect("reason")
                        .is_none()
                );
                assert_eq!(
                    row.try_get::<Option<String>, _>("error_summary")
                        .expect("summary")
                        .as_deref(),
                    Some("retryable_response")
                );
            }
            410 => {
                assert_eq!(
                    row.try_get::<Option<String>, _>("failure_reason")
                        .expect("reason")
                        .as_deref(),
                    Some("permanent")
                );
                assert_eq!(
                    row.try_get::<Option<String>, _>("error_summary")
                        .expect("summary")
                        .as_deref(),
                    Some("endpoint_gone")
                );
            }
            204 => assert!(
                row.try_get::<Option<String>, _>("error_summary")
                    .expect("summary")
                    .is_none()
            ),
            _ => unreachable!("fixed status table"),
        }
        let request = super::bounded("fixture request capture", captured)
            .await
            .expect("fixture captures request");
        assert!(
            request.starts_with(b"POST /events?source=queue HTTP/1.1\r\n"),
            "queued path and query reach the admitted receiver"
        );
        super::bounded("fixture peer joins", peer)
            .await
            .expect("fixture succeeds");
        running.stop().await;
        if expected_state == "pending" {
            sqlx::query(
                "UPDATE background_jobs SET not_before = statement_timestamp() + interval '1 day' \
                 WHERE id::text = $1",
            )
            .bind(&id)
            .execute(&pool)
            .await
            .expect("retry remains out of the next table row");
        }
    }
    super::close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn legacy_job_uses_current_path_and_rotated_keys_despite_invalid_saved_routing(pool: PgPool) {
    let body = b"\0{\"event\":\"legacy\"}\xff";
    let configured = outbound(&[("partner", "/current?binding=live")]);
    let id = enqueue(&pool, &configured, "partner", body).await;
    sqlx::query(
        "UPDATE background_jobs SET payload = jsonb_build_object(\
            'version', 1, 'endpoint_id', 'partner', 'content_type', 'application/webhook+json', \
            'body', payload->'body', 'destination', 'not a URL', 'active_key', 5, 'previous_key', false \
         ) WHERE id::text = $1",
    )
    .bind(&id)
    .execute(&pool)
    .await
    .expect("obsolete routing values do not constrain the legacy common payload");

    let (address, captured, peer) = reply_peer(200).await;
    let running =
        RunningDispatcher::start(&pool, dispatcher(&configured, &["partner"], address), 1);
    job_is(&pool, &id, "completed", 1).await;
    let request = super::bounded("fixture request capture", captured)
        .await
        .expect("fixture captures request");
    assert!(
        request.starts_with(b"POST /current?binding=live HTTP/1.1\r\n"),
        "current endpoint path replaces obsolete persisted routing"
    );
    let timestamp = header(&request, "webhook-timestamp")
        .parse::<i64>()
        .expect("timestamp is an integer");
    let expected = KeyRing::from_encoded(CURRENT_KEY, Some(PREVIOUS_KEY))
        .expect("fixture keys")
        .signatures(id.as_bytes(), timestamp, body)
        .expect("signature framing");
    assert_eq!(header(&request, "webhook-signature"), expected);
    assert_eq!(request_body(&request), body);
    super::bounded("fixture peer joins", peer)
        .await
        .expect("fixture succeeds");
    running.stop().await;
    super::close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn missing_current_endpoint_spends_the_final_attempt_and_exhausts(pool: PgPool) {
    let producer = outbound(&[("removed", "/removed")]);
    let id = enqueue(&pool, &producer, "removed", b"{\"event\":\"missing\"}").await;
    sqlx::query("UPDATE background_jobs SET attempts = 19 WHERE id::text = $1")
        .bind(&id)
        .execute(&pool)
        .await
        .expect("historical retry position");

    let current = outbound(&[]);
    let dispatcher = current
        .dispatcher(BTreeMap::new())
        .expect("empty current dispatcher");
    let running = RunningDispatcher::start(&pool, dispatcher, 1);
    job_is(&pool, &id, "failed", 20).await;
    let row = sqlx::query(
        "SELECT failure_reason, error_summary FROM background_jobs WHERE id::text = $1",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .expect("missing endpoint outcome");
    assert_eq!(
        row.try_get::<Option<String>, _>("failure_reason")
            .expect("failure reason")
            .as_deref(),
        Some("exhausted")
    );
    assert_eq!(
        row.try_get::<Option<String>, _>("error_summary")
            .expect("error summary")
            .as_deref(),
        Some("missing_endpoint")
    );
    running.stop().await;
    super::close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn invalid_or_unsupported_common_payloads_fail_permanently_before_transport(pool: PgPool) {
    let configured = outbound(&[("partner", "/never-called")]);
    for payload in [
        r#"{"version":3,"endpoint_id":"partner","content_type":"application/json","body":"AA=="}"#,
        r#"{"version":2,"endpoint_id":"partner","content_type":false,"body":"AA=="}"#,
        r#"{"version":2,"endpoint_id":"partner","content_type":"application/json","body":"%%%"}"#,
    ] {
        let id = enqueue(&pool, &configured, "partner", b"{}").await;
        sqlx::query("UPDATE background_jobs SET payload = $2::jsonb WHERE id::text = $1")
            .bind(&id)
            .bind(payload)
            .execute(&pool)
            .await
            .expect("invalid historical payload is stored");
        let running = RunningDispatcher::start(
            &pool,
            configured
                .dispatcher(keys(&["partner"]))
                .expect("current dispatcher"),
            1,
        );
        job_is(&pool, &id, "failed", 1).await;
        let row = sqlx::query(
            "SELECT failure_reason, error_summary FROM background_jobs WHERE id::text = $1",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .expect("invalid payload outcome");
        assert_eq!(
            row.try_get::<Option<String>, _>("failure_reason")
                .expect("failure reason")
                .as_deref(),
            Some("permanent")
        );
        assert_eq!(
            row.try_get::<Option<String>, _>("error_summary")
                .expect("error summary")
                .as_deref(),
            Some("invalid_payload")
        );
        running.stop().await;
    }
    super::close(&[&pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn one_held_endpoint_refunds_capacity_while_another_same_origin_endpoint_runs(pool: PgPool) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let held = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let other_received = Arc::new(Notify::new());
    let server = {
        let held = Arc::clone(&held);
        let release = Arc::clone(&release);
        let other_received = Arc::clone(&other_received);
        tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.expect("held exchange connects");
            read_headers(&mut first).await;
            held.notify_one();

            let (mut other, _) = listener.accept().await.expect("other exchange connects");
            read_headers(&mut other).await;
            other_received.notify_one();
            other
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("other response");
            other.shutdown().await.expect("other closes");

            release.notified().await;
            first
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("held response");
            first.shutdown().await.expect("held exchange closes");
        })
    };

    let configured = outbound(&[("held", "/held"), ("other", "/other")]);
    let running = RunningDispatcher::start(
        &pool,
        dispatcher(&configured, &["held", "other"], address),
        2,
    );
    let first = enqueue(&pool, &configured, "held", b"{\"event\":\"first\"}").await;
    super::bounded("the first exchange is held", held.notified()).await;

    let at_capacity = enqueue(&pool, &configured, "held", b"{\"event\":\"second\"}").await;
    job_is_snoozed(&pool, &at_capacity).await;
    let other = enqueue(&pool, &configured, "other", b"{\"event\":\"other\"}").await;
    super::bounded(
        "the other endpoint reaches the peer",
        other_received.notified(),
    )
    .await;
    job_is(&pool, &other, "completed", 1).await;

    release.notify_one();
    job_is(&pool, &first, "completed", 1).await;
    super::bounded("held fixture peer joins", server)
        .await
        .expect("fixture succeeds");
    running.stop().await;
    super::close(&[&pool]).await;
}
