use std::{
    collections::VecDeque,
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::Engine as _;
use infra_jobs::{Engine, KindsBuilder};
use infra_postgres::{Pool, TxError};
use infra_webhooks::{
    outbound::{Dispatcher, JSON_CONTENT_TYPE, OutboundError, Producer},
    protocol::KeyRing,
};
use integration_tests::jobs::EngineRun;
use sqlx::{PgPool, Row};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

const CURRENT_KEY: &str = "whsec_QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE=";
const PREVIOUS_KEY: &str = "whsec_QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI=";
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, derive_more::From, derive_more::Display)]
enum Step {
    #[from(skip)]
    #[display("delivery: {_0}")]
    Delivery(OutboundError),
    #[display("transaction: {_0}")]
    Transaction(TxError),
    #[from(skip)]
    #[display("caller rejected the transaction")]
    Rejected,
}

fn outbound(endpoint_ids: &[&str]) -> Producer {
    Producer::new(endpoint_ids.iter().map(|id| (*id).to_owned()))
}

fn limits() -> infra_outbound_http::Limits {
    infra_outbound_http::Limits::new(
        infra_webhooks::outbound::DELIVERY_POLICY.timeout,
        64,
        64 * 1024,
    )
}

fn dispatcher(endpoints: &[(&str, &str)], address: std::net::SocketAddr) -> Dispatcher {
    let limits = limits();
    Dispatcher::new(endpoints.iter().map(|(id, path)| {
        let destination =
            url::Url::parse(&format!("http://{address}{path}")).expect("fixture destination");
        let client = infra_outbound_http::test_support::client_http(
            &destination.origin().ascii_serialization(),
            limits,
        )
        .expect("fixture client");
        let keys = KeyRing::from_encoded(CURRENT_KEY, Some(PREVIOUS_KEY)).expect("fixture keys");
        (
            (*id).to_owned(),
            infra_webhooks::test_support::endpoint_with_client(destination, client, keys)
                .expect("fixture endpoint"),
        )
    }))
}

async fn enqueue(pool: &Pool, outbound: &Producer, endpoint: &str, body: &[u8]) -> String {
    let id = pool
        .in_tx(async |tx| -> Result<_, Step> {
            outbound
                .enqueue(tx, endpoint, body.to_vec(), "application/webhook+json")
                .await
                .map_err(Step::Delivery)
        })
        .await
        .unwrap_or_else(|error| panic!("delivery commits: {error}"));
    id.to_string()
}

async fn job_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM background_jobs WHERE kind = 'webhooks.deliver'")
        .fetch_one(pool)
        .await
        .expect("delivery count")
}

fn start_dispatcher(pool: &Pool, dispatcher: Dispatcher, workers: u32) -> EngineRun {
    let mut kinds = KindsBuilder::new();
    dispatcher.register(&mut kinds, None);
    let engine = Engine::new(
        pool.clone(),
        kinds.validate().expect("delivery registry"),
        NonZeroU32::new(workers).expect("at least one worker"),
    );
    EngineRun::start(engine)
}

async fn job_is(pool: &PgPool, id: &str, state: &str, attempts: i16) {
    integration_tests::poll_until(
        "the delivery reaches its persisted outcome",
        super::WAIT,
        POLL,
        async || {
            let row =
                sqlx::query("SELECT state, attempts FROM background_jobs WHERE id::text = $1")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .expect("delivery row");
            (row.try_get::<String, _>("state").expect("state") == state
                && row.try_get::<i16, _>("attempts").expect("attempts") == attempts)
                .then_some(())
        },
    )
    .await;
}

async fn status_peer(
    response_head: &'static str,
) -> (
    std::net::SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let (release, released) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("fixture accepts connection");
        let mut received = Vec::new();
        let mut chunk = [0_u8; 256];
        while !received.ends_with(b"{\"event\":\"status\"}") {
            let read = stream
                .read(&mut chunk)
                .await
                .expect("fixture reads request");
            assert!(read > 0, "fixture receives complete status request");
            received.extend_from_slice(&chunk[..read]);
        }
        assert!(
            received.starts_with(b"POST /events?source=queue HTTP/1.1\r\n"),
            "queued path and query reach the admitted receiver"
        );
        let response = format!("HTTP/1.1 {response_head}\r\nConnection: close\r\n\r\n");
        stream
            .write_all(response.as_bytes())
            .await
            .expect("fixture writes response");
        let _ = released.await;
    });
    (address, release, peer)
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn producer_commits_only_common_fields_and_preserves_raw_body(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let configured = outbound(&["partner"]);
    let body = b"\0{\"event\":\"created\"}\xff";
    let id = app_pool
        .in_tx(async |tx| -> Result<_, Step> {
            std::assert_matches!(
                configured
                    .enqueue(tx, "partners", body.to_vec(), "application/webhook+json")
                    .await,
                Err(OutboundError::UnknownEndpoint)
            );
            configured
                .enqueue(tx, "partner", body.to_vec(), "application/webhook+json")
                .await
                .map_err(Step::Delivery)
        })
        .await
        .unwrap()
        .to_string();
    assert_eq!(job_count(&pool).await, 1);
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload_text::jsonb FROM background_jobs WHERE id::text = $1")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .expect("delivery payload");
    assert_eq!(payload["version"], 2);
    assert_eq!(payload["endpoint_id"], "partner");
    assert_eq!(payload["content_type"], "application/webhook+json");
    for private in ["destination", "active_key", "previous_key"] {
        assert!(payload.get(private).is_none());
    }
    assert_eq!(
        payload["body"],
        base64::engine::general_purpose::STANDARD.encode(body)
    );
    super::close_pools(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn caller_rollback_leaves_no_delivery(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let configured = outbound(&["partner"]);
    let result = app_pool
        .in_tx(async |tx| -> Result<(), Step> {
            let _id = configured
                .enqueue(
                    tx,
                    "partner",
                    b"{\"event\":\"rolled-back\"}".as_slice(),
                    JSON_CONTENT_TYPE,
                )
                .await
                .map_err(Step::Delivery)?;
            Err(Step::Rejected)
        })
        .await;
    std::assert_matches!(&result, Err(Step::Rejected), "{result:?}");
    assert_eq!(job_count(&pool).await, 0);
    super::close_pools(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn queued_delivery_finishes_on_headers_and_respects_the_receiver_retry_floor(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    for (head, expected_state, summary, floor_seconds) in [
        (
            "401 Unauthorized\r\nContent-Length: 0",
            "pending",
            Some("response_status"),
            None,
        ),
        (
            "404 Not Found\r\nContent-Length: 0",
            "pending",
            Some("response_status"),
            None,
        ),
        (
            "410 Gone\r\nContent-Length: 1",
            "failed",
            Some("endpoint_gone"),
            None,
        ),
        ("200 OK\r\nContent-Length: 65537", "completed", None, None),
        ("204 No Content", "completed", None, None),
        (
            "503 Service Unavailable\r\nRetry-After: 172800\r\nContent-Length: 1",
            "pending",
            Some("response_status"),
            Some(172_800_i64),
        ),
        (
            "429 Too Many Requests\r\nRetry-After: 3153600000\r\nContent-Length: 0",
            "pending",
            Some("response_status"),
            Some(3_153_600_000),
        ),
        (
            "429 Too Many Requests\r\nRetry-After: 18446744073709551616\r\nContent-Length: 0",
            "failed",
            Some("retry_after_exceeds_max_delay"),
            None,
        ),
    ] {
        let (address, release, peer) = status_peer(head).await;
        let configured = outbound(&["partner"]);
        let id = enqueue(&app_pool, &configured, "partner", b"{\"event\":\"status\"}").await;
        let running = start_dispatcher(
            &app_pool,
            dispatcher(&[("partner", "/events?source=queue")], address),
            1,
        );

        job_is(&pool, &id, expected_state, 1).await;
        let row = sqlx::query(
            "SELECT failure_reason, error_summary, not_before >= (errors -> -1 ->> 'at')::timestamptz \
                 + $2::bigint * interval '1 second' AS floor_met \
             FROM background_jobs WHERE id::text = $1",
        )
        .bind(&id)
        .bind(floor_seconds)
        .fetch_one(&pool)
        .await
        .expect("delivery disposition");
        assert_eq!(
            row.get::<Option<&str>, _>("failure_reason"),
            (expected_state == "failed").then_some("permanent"),
            "{head}"
        );
        assert_eq!(
            row.get::<Option<&str>, _>("error_summary"),
            summary,
            "{head}"
        );
        assert_eq!(
            row.get::<Option<bool>, _>("floor_met"),
            floor_seconds.map(|_| true),
            "{head}"
        );
        release
            .send(())
            .expect("release peer after durable outcome");
        super::bounded("fixture peer joins", peer)
            .await
            .expect("fixture succeeds");
        running.cancel_and_join(super::WAIT).await;
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
    super::close_pools(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn reclaim_repeats_the_external_delivery_and_fences_the_late_completion(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let (first_sent, first_received) = tokio::sync::oneshot::channel();
    let (second_sent, second_received) = tokio::sync::oneshot::channel();
    let (release_first, first_released) = tokio::sync::oneshot::channel();
    let (release_second, second_released) = tokio::sync::oneshot::channel();
    let slots = Arc::new(Mutex::new(VecDeque::from([
        (first_sent, first_released),
        (second_sent, second_released),
    ])));
    #[expect(
        clippy::disallowed_methods,
        reason = "test webhook receiver fixture has no OpenAPI document"
    )]
    let app = axum::Router::new().route(
        "/events",
        axum::routing::post({
            let slots = Arc::clone(&slots);
            move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                let (sent, released) = slots
                    .lock()
                    .expect("delivery slot lock")
                    .pop_front()
                    .expect("expected at most two deliveries");
                async move {
                    let webhook_id = headers
                        .get("webhook-id")
                        .and_then(|value| value.to_str().ok())
                        .expect("webhook-id header")
                        .to_owned();
                    sent.send((webhook_id, body.to_vec()))
                        .expect("delivery capture");
                    released.await.expect("delivery release");
                    axum::http::StatusCode::NO_CONTENT
                }
            }
        }),
    );
    let (stop_peer, peer_stopped) = tokio::sync::oneshot::channel::<()>();
    let peer = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = peer_stopped.await;
            })
            .await
            .expect("receiver serves both deliveries");
    });
    let body = b"{\"event\":\"late-completion\"}";
    let id = enqueue(&app_pool, &outbound(&["partner"]), "partner", body).await;
    let original = start_dispatcher(&app_pool, dispatcher(&[("partner", "/events")], address), 1);
    let (first_id, first_body) =
        super::bounded("first request reached the receiver", first_received)
            .await
            .expect("first request");
    original.started.stop_claiming();
    let generation: i64 =
        sqlx::query_scalar("SELECT claim_generation FROM background_jobs WHERE id::text = $1")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .expect("original generation");
    sqlx::query(
        "UPDATE background_jobs SET claim_expires_at = statement_timestamp() - interval '1 second' \
         WHERE id::text = $1 AND state = 'running'",
    )
    .bind(&id)
    .execute(&pool)
    .await
    .expect("stage expiry while the first response is held");
    let replacement =
        start_dispatcher(&app_pool, dispatcher(&[("partner", "/events")], address), 1);
    let (second_id, second_body) =
        super::bounded("reclaim delivers before the old response", second_received)
            .await
            .expect("replacement request");
    assert_eq!(first_id, id);
    assert_eq!(second_id, id);
    assert_eq!(first_body, body);
    assert_eq!(second_body, body);
    job_is(&pool, &id, "running", 2).await;
    let reclaimed: i64 =
        sqlx::query_scalar("SELECT claim_generation FROM background_jobs WHERE id::text = $1")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .expect("replacement generation");
    assert!(reclaimed > generation);

    release_first.send(()).expect("release the late response");
    super::bounded(
        "the old executor records its fenced outcome",
        original.started.drained(),
    )
    .await;
    job_is(&pool, &id, "running", 2).await;
    let after: i64 =
        sqlx::query_scalar("SELECT claim_generation FROM background_jobs WHERE id::text = $1")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .expect("generation after the late completion");
    assert_eq!(after, reclaimed);
    release_second
        .send(())
        .expect("release the replacement response");
    job_is(&pool, &id, "completed", 2).await;
    stop_peer.send(()).expect("stop receiver after completion");
    super::bounded("both receiver connections join", peer)
        .await
        .expect("receiver succeeds");
    original.cancel_and_join(super::WAIT).await;
    replacement.cancel_and_join(super::WAIT).await;
    super::close_pools(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn legacy_job_uses_current_path_and_rotated_keys_despite_invalid_saved_routing(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let body = b"\0{\"event\":\"legacy\"}\xff";
    let configured = outbound(&["partner", "oversized"]);
    let refused = enqueue(&app_pool, &configured, "oversized", body).await;
    let id = enqueue(&app_pool, &configured, "partner", body).await;
    sqlx::query(
        "UPDATE background_jobs SET payload_text = NULL, payload = jsonb_build_object(\
            'version', 1, 'endpoint_id', 'partner', 'content_type', 'application/webhook+json', \
            'body', payload_text::jsonb->'body', 'destination', 'not a URL', 'active_key', 5, 'previous_key', false \
         ) WHERE id::text = $1",
    )
    .bind(&id)
    .execute(&pool)
    .await
    .expect("obsolete routing values do not constrain the legacy common payload");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let oversized = format!("/{}", "a".repeat(65_534));
    let running = start_dispatcher(
        &app_pool,
        dispatcher(
            &[
                ("oversized", &oversized),
                ("partner", "/current%2Fpath?binding=live%26well"),
            ],
            *server.address(),
        ),
        1,
    );
    job_is(&pool, &refused, "failed", 1).await;
    let (reason, summary): (String, String) = sqlx::query_as(
        "SELECT failure_reason, error_summary FROM background_jobs WHERE id::text = $1",
    )
    .bind(&refused)
    .fetch_one(&pool)
    .await
    .expect("permanent refusal is recorded on the oversized endpoint's delivery");
    assert_eq!(reason, "permanent");
    assert_eq!(summary, "invalid_payload");
    job_is(&pool, &id, "completed", 1).await;
    let requests = server
        .received_requests()
        .await
        .expect("wiremock records requests");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request.url.path(),
        "/current%2Fpath",
        "escaped path from the current endpoint replaces obsolete routing"
    );
    assert_eq!(
        request.url.query(),
        Some("binding=live%26well"),
        "escaped query from the current endpoint replaces obsolete routing"
    );
    let timestamp = request
        .headers
        .get("webhook-timestamp")
        .and_then(|value| value.to_str().ok())
        .expect("webhook-timestamp header")
        .parse::<i64>()
        .expect("timestamp is an integer");
    let expected = KeyRing::from_encoded(CURRENT_KEY, Some(PREVIOUS_KEY))
        .expect("fixture keys")
        .signatures(id.as_bytes(), timestamp, body)
        .expect("signature framing");
    assert_eq!(
        request
            .headers
            .get("webhook-signature")
            .and_then(|value| value.to_str().ok()),
        Some(expected.as_str())
    );
    assert_eq!(request.body, body);
    server.verify().await;
    running.cancel_and_join(super::WAIT).await;
    super::close_pools(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn missing_current_endpoint_spends_the_final_attempt_and_exhausts(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let producer = outbound(&["removed"]);
    let id = enqueue(&app_pool, &producer, "removed", b"{\"event\":\"missing\"}").await;
    sqlx::query("UPDATE background_jobs SET attempts = 19 WHERE id::text = $1")
        .bind(&id)
        .execute(&pool)
        .await
        .expect("historical retry position");

    let running = start_dispatcher(&app_pool, Dispatcher::new([]), 1);
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
    running.cancel_and_join(super::WAIT).await;
    super::close_pools(&[&app_pool]).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn an_undecodable_payload_is_retried_not_failed_permanently(pool: PgPool) {
    let app_pool = infra_postgres::test_support::pool_from_sqlx(pool.clone());
    let configured = outbound(&["partner"]);
    let id = enqueue(&app_pool, &configured, "partner", b"{}").await;
    sqlx::query("UPDATE background_jobs SET payload_text = $2 WHERE id::text = $1")
        .bind(&id)
        .bind(r#"{"endpoint_id":"partner","content_type":"application/json","body":"%%%"}"#)
        .execute(&pool)
        .await
        .expect("undecodable payload is stored");
    let running = start_dispatcher(
        &app_pool,
        dispatcher(
            &[("partner", "/never-called")],
            "127.0.0.1:1".parse().unwrap(),
        ),
        1,
    );
    job_is(&pool, &id, "pending", 1).await;
    let row = sqlx::query(
        "SELECT failure_reason, error_summary FROM background_jobs WHERE id::text = $1",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .expect("decode outcome");
    assert!(
        row.try_get::<Option<String>, _>("failure_reason")
            .expect("failure reason")
            .is_none()
    );
    assert!(
        row.try_get::<Option<String>, _>("error_summary")
            .expect("error summary")
            .is_some_and(|summary| summary.contains("does not decode"))
    );
    running.cancel_and_join(super::WAIT).await;
    super::close_pools(&[&app_pool]).await;
}
