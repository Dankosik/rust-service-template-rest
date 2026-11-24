use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};
use axum::response::IntoResponse as _;
use axum::routing::post;
use axum_test::TestServer;
use infra_clickhouse::{ClickHouseAuth, ClickHouseOptions, QuerySettings, Row, TransportSecurity};
use operation_context::Stopped;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};
use tower::ServiceExt as _;

use super::*;
use crate::test_support::body_json;

const OPENING_BUDGET: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize, Serialize, Row)]
#[clickhouse(crate = "infra_clickhouse")]
struct EventRow {
    id: u64,
}

fn encode_ch_str(s: &str, out: &mut Vec<u8>) {
    out.push(u8::try_from(s.len()).unwrap());
    out.extend_from_slice(s.as_bytes());
}

fn rbwnat_describe_id_column() -> Bytes {
    let mut out = Vec::new();
    out.push(7);
    for col in [
        "name",
        "type",
        "default_type",
        "default_expression",
        "comment",
        "codec_expression",
        "ttl_expression",
    ] {
        encode_ch_str(col, &mut out);
    }
    for _ in 0..7 {
        encode_ch_str("String", &mut out);
    }
    encode_ch_str("id", &mut out);
    encode_ch_str("UInt64", &mut out);
    for _ in 0..5 {
        encode_ch_str("", &mut out);
    }
    Bytes::from(out)
}

async fn clickhouse_route(
    provider_route: axum::routing::MethodRouter,
    route: axum::routing::MethodRouter<ClickHouse>,
) -> (TestServer, Router, ClickHouse) {
    #[expect(
        clippy::disallowed_methods,
        reason = "the provider fixture receives ClickHouse HTTP requests"
    )]
    let provider = TestServer::builder()
        .http_transport()
        .build(Router::new().route("/", provider_route));
    let client = ClickHouse::start(ClickHouseOptions {
        url: provider
            .server_address()
            .unwrap()
            .origin()
            .ascii_serialization(),
        database: "app".to_owned(),
        auth: ClickHouseAuth::Unauthenticated { username: None },
        transport: TransportSecurity::Plaintext,
        query_settings: QuerySettings::Client {
            max_memory_usage: None,
        },
        query_timeout: Duration::from_mins(2),
        insert_timeout: Duration::from_mins(2),
        max_read_concurrency: NonZeroUsize::new(4).unwrap(),
        max_write_concurrency: NonZeroUsize::new(4).unwrap(),
        max_result_rows: NonZeroU64::new(1_000).unwrap(),
        max_result_bytes: NonZeroUsize::new(1024 * 1024).unwrap(),
        http_response_buffer_size: NonZeroUsize::new(256 * 1024).unwrap(),
        propagate_trace_context: false,
    })
    .await
    .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the fixture drives one hardened ClickHouse route"
    )]
    let routes = Router::new()
        .route("/events", route)
        .with_state(client.clone());
    let app = crate::harden(
        routes,
        &crate::HardenOptions {
            max_body_bytes: 1024,
            request_timeout: OPENING_BUDGET,
            max_in_flight: None,
            admission_wait: Duration::ZERO,
            log_health_probes: false,
            trust_trace_context: false,
            draining: None,
        },
    );
    (provider, app, client)
}

#[tokio::test]
async fn clickhouse_problems_preserve_route_policy_and_unconfirmed_inserts() {
    use InsertError::{NotApplied, OutcomeUnknown};
    use InsertFailure as F;
    use QueryError as Q;

    let read = |error| query_problem(error, || Problem::new(Code::Forbidden));
    for (problem, status, code) in [
        (read(Q::NotFound), 403, "forbidden"),
        (read(Q::Busy), 503, "service_unavailable"),
        (read(Q::Unavailable), 503, "service_unavailable"),
        (read(Q::ResourceLimit), 503, "service_unavailable"),
        (read(Q::Stopped(Stopped::Deadline)), 504, "gateway_timeout"),
        (read(Q::TooManyRows), 500, "internal_error"),
        (read(Q::TooLarge), 500, "internal_error"),
        (read(Q::Rejected), 500, "internal_error"),
        (read(Q::Decode), 500, "internal_error"),
        (
            insert_problem(NotApplied(F::TooLarge)),
            413,
            "request_entity_too_large",
        ),
        (
            insert_problem(NotApplied(F::Busy)),
            503,
            "service_unavailable",
        ),
        (
            insert_problem(NotApplied(F::TooManyParts)),
            503,
            "service_unavailable",
        ),
        (
            insert_problem(NotApplied(F::Stopped(Stopped::Deadline))),
            504,
            "gateway_timeout",
        ),
        (
            insert_problem(NotApplied(F::InvalidInput)),
            500,
            "internal_error",
        ),
        (insert_problem(NotApplied(F::Encode)), 500, "internal_error"),
        (
            insert_problem(NotApplied(F::Rejected)),
            500,
            "internal_error",
        ),
    ] {
        let response = problem.into_response();
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
        assert_eq!(body_json(response).await["code"], code);
    }

    let refused = insert_problem(NotApplied(F::Unavailable)).into_response();
    let unknown_unavail =
        insert_problem(OutcomeUnknown(UnknownOutcome::Unavailable)).into_response();
    let unknown_deadline =
        insert_problem(OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Deadline))).into_response();
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(unknown_unavail.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(unknown_deadline.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(!unknown_unavail.headers().contains_key(RETRY_AFTER));
    assert!(!unknown_deadline.headers().contains_key(RETRY_AFTER));
    assert!(body_json(refused).await.get("detail").is_none());
    for response in [unknown_unavail, unknown_deadline] {
        assert_eq!(
            body_json(response).await["detail"],
            "the analytics database could not confirm whether the insert took effect"
        );
    }
}

#[tokio::test]
async fn request_budget_timeout_distinguishes_pre_dispatch_describe_from_dispatched_insert() {
    let entered = Arc::new(Notify::new());
    let notify = entered.clone();
    let (release_tx, release_rx) = watch::channel(());

    let provider = move |request: Request| {
        let notify = notify.clone();
        let mut release = release_rx.clone();
        async move {
            let uri = request.uri().to_string();
            let body = to_bytes(request.into_body(), 4096).await.unwrap();
            let body_str = String::from_utf8_lossy(&body);
            if body_str.contains("DESCRIBE") || uri.contains("DESCRIBE") {
                if body_str.contains("stall_describe") || uri.contains("stall_describe") {
                    notify.notify_one();
                    let _ = release.changed().await;
                }
                return (StatusCode::OK, rbwnat_describe_id_column()).into_response();
            }
            if uri.contains("stall_insert") || body_str.contains("stall_insert") {
                notify.notify_one();
                let _ = release.changed().await;
            }
            StatusCode::OK.into_response()
        }
    };

    let handler = |State(client): State<ClickHouse>, context: RequestContext, request: Request| async move {
        let query = request.uri().query().unwrap_or("batch:events");
        let (mode, table) = query.split_once(':').unwrap_or(("batch", query));
        let rows = [EventRow { id: 1 }];
        let options = if mode == "async" {
            InsertOptions::async_with_payload_hash()
        } else {
            InsertOptions::batch()
        };
        context
            .clickhouse_insert(&client, "ingest", table, &rows, options)
            .await
            .map(|_| StatusCode::ACCEPTED)
            .map_err(insert_problem)
    };

    let (_provider, app, client) = clickhouse_route(post(provider), post(handler)).await;

    for (query, expected_detail) in [
        (
            "batch:stall_describe",
            "request budget expired before a response could be committed",
        ),
        (
            "async:stall_describe_async",
            "request budget expired before a response could be committed",
        ),
        (
            "batch:stall_insert",
            "the analytics database could not confirm whether the insert took effect",
        ),
        (
            "async:stall_insert_async",
            "the analytics database could not confirm whether the insert took effect",
        ),
    ] {
        let response = tokio::spawn(
            app.clone().oneshot(
                Request::post(format!("/events?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            ),
        );
        tokio::time::timeout(OPENING_BUDGET, entered.notified())
            .await
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(OPENING_BUDGET).await;
        let response = response.await.unwrap().unwrap();
        tokio::time::resume();

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT, "{query}");
        assert!(!response.headers().contains_key(RETRY_AFTER));
        let problem = body_json(response).await;
        assert_eq!(problem["code"], "gateway_timeout");
        assert_eq!(problem["detail"], expected_detail, "{query}");
    }

    drop(release_tx);
    let _ = client.close(Duration::from_secs(1)).await;
}

async fn run_insert_scenario(
    client: &ClickHouse,
    context: &RequestContext,
    scenario: Option<&str>,
) {
    let rows = [EventRow { id: 1 }];
    match scenario {
        Some("acknowledged") => {
            context
                .clickhouse_insert(
                    client,
                    "ingest",
                    "events_ok",
                    &rows,
                    InsertOptions::async_with_token("tok"),
                )
                .await
                .unwrap();
        }
        Some("not_applied") => {
            let err = context
                .clickhouse_insert(
                    client,
                    "ingest",
                    "not_applied",
                    &rows,
                    InsertOptions::batch(),
                )
                .await
                .unwrap_err();
            std::assert_matches!(err, InsertError::NotApplied(InsertFailure::TooManyParts));
        }
        Some("unconfirmed") => {
            let err = context
                .clickhouse_insert(
                    client,
                    "ingest",
                    "unconfirmed",
                    &rows,
                    InsertOptions::batch(),
                )
                .await
                .unwrap_err();
            std::assert_matches!(err, InsertError::OutcomeUnknown(_));
        }
        Some("unconfirmed_async") => {
            let err = context
                .clickhouse_insert(
                    client,
                    "ingest",
                    "unconfirmed",
                    &rows,
                    InsertOptions::async_with_payload_hash(),
                )
                .await
                .unwrap_err();
            std::assert_matches!(err, InsertError::OutcomeUnknown(_));
        }
        _ => {}
    }
}

#[tokio::test]
async fn a_later_timeout_distinguishes_settled_and_unconfirmed_clickhouse_inserts() {
    let waiting = Arc::new(Notify::new());
    let notify = waiting.clone();

    let provider = |request: Request| async move {
        let uri = request.uri().to_string();
        let body = to_bytes(request.into_body(), 4096).await.unwrap();
        let body_str = String::from_utf8_lossy(&body);
        if body_str.contains("DESCRIBE") || uri.contains("DESCRIBE") {
            return (StatusCode::OK, rbwnat_describe_id_column()).into_response();
        }
        if uri.contains("not_applied") {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Code: 252. DB::Exception: Too many parts",
            )
                .into_response();
        }
        if uri.contains("unconfirmed") {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Code: 319. DB::Exception: Unknown status of distributed insert",
            )
                .into_response();
        }
        StatusCode::OK.into_response()
    };

    let handler =
        move |State(client): State<ClickHouse>, context: RequestContext, request: Request| {
            let notify = notify.clone();
            async move {
                run_insert_scenario(&client, &context, request.uri().query()).await;
                notify.notify_one();
                std::future::pending::<StatusCode>().await
            }
        };

    let (_provider, app, client) = clickhouse_route(post(provider), post(handler)).await;

    for (uri, expected_detail) in [
        (
            "/events",
            "request budget expired before a response could be committed",
        ),
        (
            "/events?acknowledged",
            "request budget expired before a response could be committed",
        ),
        (
            "/events?not_applied",
            "request budget expired before a response could be committed",
        ),
        (
            "/events?unconfirmed",
            "the analytics database could not confirm whether the insert took effect",
        ),
        (
            "/events?unconfirmed_async",
            "the analytics database could not confirm whether the insert took effect",
        ),
    ] {
        let response = tokio::spawn(
            app.clone()
                .oneshot(Request::post(uri).body(Body::empty()).unwrap()),
        );
        tokio::time::timeout(OPENING_BUDGET, waiting.notified())
            .await
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(OPENING_BUDGET).await;
        let response = response.await.unwrap().unwrap();
        tokio::time::resume();

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT, "{uri}");
        assert_eq!(
            body_json(response).await["detail"],
            expected_detail,
            "{uri}"
        );
    }

    let _ = client.close(Duration::from_secs(1)).await;
}
