use std::sync::atomic::AtomicUsize;

use axum::http::header::{ALLOW, CONNECTION, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{Method, Request as HttpRequest, Version};
use axum::response::IntoResponse as _;
use axum::routing::{get, post};
use axum_test::TestServer;
use http_body_util::BodyExt;
use observe_test_support::EventCapture;
use serde_json::Value;
use tower::ServiceExt;
use tracing::instrument::WithSubscriber;

use super::*;
use crate::request_id::REQUEST_ID_HEADER;
use crate::test_support::body_json;

fn options() -> HardenOptions {
    HardenOptions {
        max_body_bytes: 64,
        request_timeout: Duration::from_millis(200),
        max_in_flight: NonZeroU32::new(2),
        admission_wait: Duration::from_millis(200),
        log_health_probes: false,
        trust_trace_context: false,
        draining: None,
    }
}

fn app(options: &HardenOptions) -> Router {
    let ok = || async { "ok" };
    let slow = || async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        "late"
    };
    let panics = || async {
        let armed = std::hint::black_box(true);
        assert!(!armed, "boom {}", 42);
        "unreachable"
    };
    let echo = |body: Result<String, axum::extract::rejection::StringRejection>| async move {
        match body {
            Ok(body) => body.into_response(),
            Err(rejection) => ([(RETRY_AFTER, "60")], rejection).into_response(),
        }
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = Router::new()
        .route("/ok", get(ok))
        .route("/slow", get(slow))
        .route("/panic", get(panics))
        .route("/echo", post(echo))
        .route("/small-echo", post(echo).layer(DefaultBodyLimit::max(4)));
    harden(routes, options)
}

fn request(method: Method, uri: &str) -> HttpRequest<Body> {
    HttpRequest::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

// template:begin request-budget:http-request-deadline-test
#[tokio::test(start_paused = true)]
async fn request_deadline_is_observable_without_restarting_the_budget() {
    let options = HardenOptions {
        request_timeout: Duration::from_millis(100),
        ..options()
    };
    let started = tokio::time::Instant::now();
    let expected = started + options.request_timeout;
    let handler_1 = move |deadline: crate::RequestDeadline, context: crate::RequestContext| async move {
        assert_eq!(context.deadline(), Some(deadline));
        assert_eq!(deadline.at(), expected);
        assert_eq!(
            deadline.attempt_end(),
            Some(started + Duration::from_millis(50))
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(deadline.at(), expected);
        assert_eq!(deadline.attempt_end(), None);
        assert_eq!(
            deadline.at() - tokio::time::Instant::now(),
            Duration::from_millis(50)
        );
        "within budget"
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = Router::new().route("/deadline", get(handler_1));
    let response = harden(routes, &options)
        .oneshot(request(Method::GET, "/deadline"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "within budget"
    );
}
// template:end request-budget:http-request-deadline-test

#[tokio::test]
async fn success_carries_request_id_and_nosniff() {
    let server = TestServer::new(app(&options()));
    let response = server.get("/ok").await;
    response.assert_status_ok();
    response.assert_header(X_CONTENT_TYPE_OPTIONS, "nosniff");
    response.assert_text("ok");
    let id = response.header(REQUEST_ID_HEADER);
    let id = id.to_str().unwrap();
    let parsed = uuid::Uuid::parse_str(id).expect("generated request ID is a UUID");
    assert_eq!(parsed.get_version_num(), 4);
    assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122);
}

#[tokio::test]
async fn a_known_body_length_survives_the_chain() {
    let response = app(&options())
        .oneshot(request(Method::GET, "/ok"))
        .await
        .unwrap();
    assert_eq!(
        axum::body::HttpBody::size_hint(response.body()).exact(),
        Some(2)
    );
}

#[tokio::test]
async fn only_a_single_valid_inbound_request_id_is_echoed() {
    let server = TestServer::new(app(&options()));
    server
        .get("/ok")
        .add_header(REQUEST_ID_HEADER, "client-id_1")
        .await
        .assert_header(REQUEST_ID_HEADER, "client-id_1");

    let response = server
        .get("/ok")
        .add_header(REQUEST_ID_HEADER, "bad id with spaces")
        .await;
    let id = response.header(REQUEST_ID_HEADER);
    assert_ne!(id, "bad id with spaces");
    let parsed = uuid::Uuid::parse_str(id.to_str().unwrap()).unwrap();
    assert_eq!(parsed.get_version_num(), 4);
    assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122);

    let mut duplicate = request(Method::GET, "/missing");
    for value in ["client-id_1", "bad id with spaces"] {
        duplicate
            .headers_mut()
            .append(REQUEST_ID_HEADER, HeaderValue::from_static(value));
    }
    let response = app(&options()).oneshot(duplicate).await.unwrap();
    let id = response.headers()[&REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
    assert_eq!(body_json(response).await["request_id"], id);
}

#[tokio::test]
async fn request_id_stays_consistent_when_other_layers_supply_ids() {
    use tower_http::request_id::RequestId;

    let handler = || async {
        (
            [(REQUEST_ID_HEADER, "handler-id")],
            Problem::new(Code::BadRequest),
        )
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = Router::new().route("/problem", get(handler));
    let app = harden(routes, &options());
    for (path, existing_id, status, expected_code) in [
        ("/problem", None, StatusCode::BAD_REQUEST, "\"bad_request\""),
        (
            "/missing",
            Some("outer-id"),
            StatusCode::NOT_FOUND,
            "\"not_found\"",
        ),
    ] {
        let mut request = request(Method::GET, path);
        request
            .headers_mut()
            .insert(REQUEST_ID_HEADER, HeaderValue::from_static("client-id"));
        if let Some(id) = existing_id {
            request
                .extensions_mut()
                .insert(RequestId::new(HeaderValue::from_static(id)));
        }
        let events = EventCapture::new();
        let subscriber = events.dispatch_for(&[concat!(env!("CARGO_CRATE_NAME"), "::observe")]);
        let response = app
            .clone()
            .oneshot(request)
            .with_subscriber(subscriber)
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{path}");
        assert_eq!(response.headers()[REQUEST_ID_HEADER], "client-id", "{path}");
        assert_eq!(
            body_json(response).await["request_id"],
            "client-id",
            "{path}"
        );
        assert!(
            events.snapshot().iter().any(|event| {
                event.field("request_id") == Some("\"client-id\"")
                    && event.field("problem_code") == Some(expected_code)
            }),
            "{path}"
        );
    }
}

#[tokio::test]
async fn not_found_and_method_not_allowed_are_problems_with_allow() {
    let server = TestServer::new(app(&options()));
    let response = server.get("/missing").await;
    response.assert_status(StatusCode::NOT_FOUND);
    response.assert_header(CONTENT_TYPE, "application/problem+json");
    let json = response.json::<Value>();
    assert_eq!(json["code"], "not_found");
    let id = response.header(REQUEST_ID_HEADER);
    assert_eq!(json["request_id"].as_str(), Some(id.to_str().unwrap()));

    let response = server.delete("/ok").await;
    response.assert_status(StatusCode::METHOD_NOT_ALLOWED);
    response.assert_header(CONTENT_TYPE, "application/problem+json");
    let allow = response.header(ALLOW);
    assert!(allow.to_str().unwrap().contains("GET"), "{allow:?}");
    assert_eq!(response.json::<Value>()["code"], "method_not_allowed");

    let head = app(&options())
        .oneshot(request(Method::HEAD, "/missing"))
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::NOT_FOUND);
    assert_eq!(head.headers()[CONTENT_TYPE], "application/problem+json");
    assert!(
        head.headers()[CONTENT_LENGTH]
            .to_str()
            .unwrap()
            .parse::<usize>()
            .unwrap()
            > 0
    );
    assert!(
        head.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
}

#[tokio::test]
async fn timeout_is_a_504_problem() {
    let server = TestServer::new(app(&options()));
    let response = server.get("/slow").await;
    response.assert_status(StatusCode::GATEWAY_TIMEOUT);
    response.assert_header(CONTENT_TYPE, "application/problem+json");
    let json = response.json::<Value>();
    assert_eq!(json["code"], "gateway_timeout");
    let id = response.header(REQUEST_ID_HEADER);
    assert_eq!(json["request_id"].as_str(), Some(id.to_str().unwrap()));
}

#[tokio::test(start_paused = true)]
async fn a_body_the_caller_stops_sending_is_a_408_problem_that_closes_the_connection() {
    let first_chunk = futures_util::stream::once(async {
        Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"{"))
    });
    let stalled = futures_util::StreamExt::chain(first_chunk, futures_util::stream::pending());
    let request = HttpRequest::builder()
        .method(Method::POST)
        .uri("/json")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from_stream(stalled))
        .unwrap();

    let handler = |_: crate::extract::Json<serde_json::Value>| async { StatusCode::OK };
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = Router::new().route("/json", post(handler));
    let response = harden(routes, &options()).oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONNECTION)
            .unwrap(),
        "close"
    );
    assert_eq!(body_json(response).await["code"], "request_timeout");
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_body_read_does_not_turn_a_handler_timeout_into_408() {
    let handler = |context: crate::RequestContext, request: Request| async move {
        let mut body = request.into_body();
        let read = tokio::time::timeout(
            Duration::from_millis(10),
            context.read_body_frame(&mut body),
        );
        assert!(read.await.is_err());
        tokio::time::sleep(Duration::from_secs(1)).await;
        drop(body);
        StatusCode::OK
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = Router::new().route("/upload", post(handler));
    let request = HttpRequest::post("/upload")
        .body(Body::from_stream(futures_util::stream::pending::<
            Result<axum::body::Bytes, std::io::Error>,
        >()))
        .unwrap();

    let response = harden(routes, &options()).oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(
        !response
            .headers()
            .contains_key(axum::http::header::CONNECTION)
    );
    assert_eq!(body_json(response).await["code"], "gateway_timeout");
}

#[tokio::test(start_paused = true)]
async fn panic_is_a_sanitized_500_problem() {
    let events = EventCapture::new();
    let subscriber = events.dispatch_for(&["infra_http::harden"]);
    let response = app(&options())
        .oneshot(HttpRequest::get("/panic").body(Body::empty()).unwrap())
        .with_subscriber(subscriber)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
    assert!(response.headers().contains_key(&REQUEST_ID_HEADER));
    let id = response.headers()[&REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();
    let json = body_json(response).await;
    assert_eq!(json["request_id"].as_str(), Some(id.as_str()));
    assert_eq!(json["code"], "internal_error");
    assert_eq!(json["detail"], SANITIZED_DETAIL);
    assert!(!json.to_string().contains("boom"));
    let captured = events.snapshot();
    assert_eq!(captured.len(), 1, "{captured:?}");
    assert_eq!(captured[0].message(), Some("http_handler_panicked"));
    assert_eq!(captured[0].fields.len(), 1, "{captured:?}");
}

#[tokio::test]
async fn declared_and_streamed_oversize_bodies_are_413_problems() {
    let big = "x".repeat(65);
    let declared = HttpRequest::builder()
        .method(Method::POST)
        .uri("/echo")
        .header(CONTENT_LENGTH, big.len())
        .body(Body::from(big.clone()))
        .unwrap();
    let response = app(&options()).oneshot(declared).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/problem+json"
    );
    assert_eq!(
        body_json(response).await["code"],
        "request_entity_too_large"
    );

    let streamed = HttpRequest::builder()
        .method(Method::POST)
        .uri("/echo")
        .body(Body::from_stream(futures_stream(big)))
        .unwrap();
    let response = app(&options()).oneshot(streamed).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(response.headers()[RETRY_AFTER], "60");
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/problem+json"
    );
    assert_eq!(
        body_json(response).await["code"],
        "request_entity_too_large"
    );
}

#[tokio::test]
async fn extractors_accept_the_configured_ceiling_and_honor_a_lower_route_limit() {
    let options = HardenOptions {
        max_body_bytes: 2 * 1024 * 1024 + 1,
        ..options()
    };
    let app = app(&options);
    for (path, size, status) in [
        ("/echo", options.max_body_bytes, StatusCode::OK),
        ("/small-echo", 5, StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let request = HttpRequest::post(path)
            .body(Body::from("x".repeat(size)))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), status, "{path}");
    }
}

#[tokio::test]
async fn handler_413_preserves_its_body_and_headers() {
    let handler = || async {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            [
                ("retry-after", "60"),
                ("content-length", "4"),
                (
                    "content-digest",
                    "sha-256=:cW7Ku0WsaoigSTmP3i09UiXG3TEhrpu8WvRX60uvBWo=:",
                ),
            ],
            "wait",
        )
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = Router::new().route("/upload", post(handler));
    let response = harden(routes, &options())
        .oneshot(HttpRequest::post("/upload").body(Body::from("ok")).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(response.headers()[RETRY_AFTER], "60");
    assert_eq!(
        response.headers()[CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    assert_eq!(response.headers()[CONTENT_LENGTH], "4");
    assert_eq!(
        response.headers()["content-digest"],
        "sha-256=:cW7Ku0WsaoigSTmP3i09UiXG3TEhrpu8WvRX60uvBWo=:"
    );
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "wait"
    );
}

fn futures_stream(
    text: String,
) -> impl futures_util::Stream<Item = Result<axum::body::Bytes, std::io::Error>> {
    let chunks: Vec<_> = text
        .into_bytes()
        .chunks(16)
        .map(|c| Ok(axum::body::Bytes::copy_from_slice(c)))
        .collect();
    futures_util::stream::iter(chunks)
}

#[expect(
    clippy::disallowed_methods,
    reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
)]
fn holding_routes(started: Arc<AtomicUsize>, gate: Arc<tokio::sync::Notify>) -> Router {
    Router::new().route(
        "/hold",
        get(move || {
            let started = started.clone();
            let gate = gate.clone();
            async move {
                started.fetch_add(1, Ordering::SeqCst);
                gate.notified().await;
                "released"
            }
        }),
    )
}

#[tokio::test]
async fn a_request_over_the_limit_waits_for_a_permit_and_is_served() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let started = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(tokio::sync::Notify::new());
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = holding_routes(started.clone(), gate.clone()).route("/ok", get(|| async { "ok" }));
    let mut options = options();
    options.request_timeout = Duration::from_secs(5);
    let app = harden(routes, &options);

    let mut holders = Vec::new();
    for _ in 0..2 {
        let app = app.clone();
        holders.push(tokio::spawn(async move {
            app.oneshot(request(Method::GET, "/hold")).await.unwrap()
        }));
    }
    while started.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
    }

    let mut waiting = Box::pin(app.clone().oneshot(request(Method::GET, "/ok")));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut waiting)
            .await
            .is_err(),
        "the request waits instead of being shed"
    );
    gate.notify_waiters();
    for holder in holders {
        assert_eq!(holder.await.unwrap().status(), StatusCode::OK);
    }
    let served = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("served once a permit frees")
        .unwrap();
    assert_eq!(served.status(), StatusCode::OK);
    let wait_samples =
        snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .find_map(|(key, _, _, value)| {
                (key.key().name() == observe::ADMISSION_WAIT_METRIC).then_some(value)
            });
    std::assert_matches!(
        wait_samples,
        Some(DebugValue::Histogram(samples)) if samples.len() == 1 && samples[0].into_inner() > 0.0
    );
}

#[tokio::test(start_paused = true)]
async fn a_request_is_shed_when_a_permit_frees_after_its_wait_budget() {
    use std::task::{Context, Waker};

    let started = Arc::new(AtomicUsize::new(0));
    let routes = holding_routes(started.clone(), Arc::new(tokio::sync::Notify::new()));
    let mut options = options();
    options.max_in_flight = NonZeroU32::new(1);
    options.admission_wait = options.request_timeout * 3;
    let app = harden(routes, &options);
    let mut holder = std::pin::pin!(app.clone().oneshot(request(Method::GET, "/hold")));
    let mut waiting = std::pin::pin!(app.oneshot(request(Method::GET, "/hold")));
    let cx = &mut Context::from_waker(Waker::noop());
    assert!(holder.as_mut().poll(cx).is_pending());
    assert!(waiting.as_mut().poll(cx).is_pending());
    assert_eq!(started.load(Ordering::SeqCst), 1);

    tokio::time::advance(options.request_timeout + Duration::from_millis(1)).await;
    assert_eq!(holder.await.unwrap().status(), StatusCode::GATEWAY_TIMEOUT);

    let shed = waiting.await.unwrap();
    assert_eq!(shed.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(shed.headers()[RETRY_AFTER], "1");
    let request_id = shed.headers()[REQUEST_ID_HEADER].clone();
    let problem = body_json(shed).await;
    assert_eq!(problem["code"], "service_unavailable");
    assert_eq!(problem["request_id"], request_id.to_str().unwrap());
    assert_eq!(started.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn a_request_is_shed_after_the_admission_wait_while_its_budget_is_still_open() {
    use std::task::{Context, Waker};

    let started = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(tokio::sync::Notify::new());
    let routes = holding_routes(started.clone(), gate.clone());
    let options = HardenOptions {
        request_timeout: Duration::from_secs(8),
        admission_wait: Duration::from_millis(200),
        max_in_flight: NonZeroU32::new(1),
        ..options()
    };
    let app = harden(routes, &options);
    let mut holder = std::pin::pin!(app.clone().oneshot(request(Method::GET, "/hold")));
    let cx = &mut Context::from_waker(Waker::noop());
    assert!(holder.as_mut().poll(cx).is_pending());

    let arrived = tokio::time::Instant::now();
    let shed = app
        .clone()
        .oneshot(request(Method::GET, "/hold"))
        .await
        .unwrap();
    assert_eq!(shed.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(shed.headers()[RETRY_AFTER], "1");
    let waited = arrived.elapsed();
    assert!(waited >= options.admission_wait && waited < Duration::from_secs(1));
    assert_eq!(started.load(Ordering::SeqCst), 1);

    gate.notify_waiters();
    assert_eq!(holder.await.unwrap().status(), StatusCode::OK);
    let served = app.oneshot(request(Method::GET, "/hold"));
    gate.notify_one();
    assert_eq!(served.await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn admission_permit_releases_at_headers_while_response_context_lives_until_body_drop() {
    let (send, receive) = tokio::sync::oneshot::channel();
    let sender = Arc::new(std::sync::Mutex::new(Some(send)));
    let stream_handler = move |response: crate::ResponseContext| {
        let sender = Arc::clone(&sender);
        async move {
            sender
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(response)
                .unwrap();
            Body::from_stream(futures_util::stream::pending::<
                Result<axum::body::Bytes, std::io::Error>,
            >())
        }
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = Router::new()
        .route("/stream", get(stream_handler))
        .route("/ok", get(|| async { "ok" }));
    let app = harden(
        routes,
        &HardenOptions {
            max_in_flight: NonZeroU32::new(1),
            admission_wait: Duration::ZERO,
            ..options()
        },
    );

    let streaming = app
        .clone()
        .oneshot(request(Method::GET, "/stream"))
        .await
        .unwrap();
    assert_eq!(streaming.status(), StatusCode::OK);
    let response_context = receive.await.unwrap();
    assert!(!response_context.operation().cancellation().is_cancelled());

    let second = app.oneshot(request(Method::GET, "/ok")).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);

    drop(streaming);
    assert!(response_context.operation().cancellation().is_cancelled());
}

async fn ready_control() -> health::ReadinessControl {
    let readiness = health::Readiness::new(
        Vec::new(),
        health::RefreshPolicy::try_new(
            Duration::from_secs(1),
            Duration::from_secs(1),
            NonZeroU32::MIN,
        )
        .unwrap(),
    );
    let control = readiness.control();
    let _refresher = readiness.prepare().await.unwrap().activate().unwrap();
    control
}

#[tokio::test]
async fn http1_responses_ask_the_client_to_reconnect_only_while_draining() {
    let readiness = ready_control().await;
    let app = app(&HardenOptions {
        draining: Some(readiness.reader()),
        ..options()
    });

    let serving = app
        .clone()
        .oneshot(request(Method::GET, "/ok"))
        .await
        .unwrap();
    assert_eq!(serving.status(), StatusCode::OK);
    assert!(!serving.headers().contains_key(CONNECTION));

    readiness.start_drain();
    for uri in ["/ok", "/missing"] {
        let draining = app
            .clone()
            .oneshot(request(Method::GET, uri))
            .await
            .unwrap();
        assert_eq!(draining.headers()[CONNECTION], "close", "{uri}");
    }

    let mut multiplexed = request(Method::GET, "/ok");
    *multiplexed.version_mut() = Version::HTTP_2;
    let multiplexed = app.oneshot(multiplexed).await.unwrap();
    assert_eq!(multiplexed.status(), StatusCode::OK);
    assert!(!multiplexed.headers().contains_key(CONNECTION));
}

#[tokio::test(start_paused = true)]
async fn a_request_that_waits_out_its_patience_is_shed_and_logged() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let started = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let handler_1 = {
        let started = started.clone();
        let gate = gate.clone();
        move || {
            let started = started.clone();
            let gate = gate.clone();
            async move {
                started.notify_one();
                gate.notified().await;
                Problem::new(Code::ServiceUnavailable)
            }
        }
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = Router::new().route("/hold", get(handler_1));
    let patience = Duration::from_millis(100);
    let app = harden(
        routes,
        &HardenOptions {
            max_in_flight: NonZeroU32::new(1),
            admission_wait: patience,
            request_timeout: Duration::from_secs(10),
            ..options()
        },
    );
    let events = EventCapture::new();
    let subscriber = events.dispatch_for(&[concat!(env!("CARGO_CRATE_NAME"), "::observe")]);

    let holder = tokio::spawn({
        let app = app.clone();
        app.oneshot(request(Method::GET, "/hold"))
            .with_subscriber(subscriber.clone())
    });
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .unwrap();

    let waited = tokio::time::Instant::now();
    let shed = app
        .oneshot(request(Method::GET, "/hold"))
        .with_subscriber(subscriber)
        .await
        .unwrap();
    assert!(waited.elapsed() >= patience, "shed only after the wait");
    assert_eq!(shed.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(shed.headers().get(RETRY_AFTER).unwrap(), "1");
    let json = body_json(shed).await;
    assert_eq!(json["code"], "service_unavailable");
    assert_eq!(json["detail"], service_failure::AT_CAPACITY_DETAIL);

    let metrics = snapshotter.snapshot().into_vec();
    let wait_samples = metrics.iter().find_map(|(key, _, _, value)| {
        (key.key().name() == observe::ADMISSION_WAIT_METRIC).then_some(value)
    });
    std::assert_matches!(
        wait_samples,
        Some(DebugValue::Histogram(samples))
            if samples.len() == 1 && samples[0].into_inner() >= patience.as_secs_f64()
    );
    let shed_count = metrics.iter().find_map(|(key, _, _, value)| {
        (key.key().name() == observe::SHED_REQUESTS_METRIC).then_some(value)
    });
    assert_eq!(shed_count, Some(&DebugValue::Counter(1)));

    let shed_events = events.take();
    assert!(shed_events.iter().any(|event| {
        event.message() == Some("http_request")
            && event.field("termination") == Some("\"load_shed\"")
            && event.field("problem_code") == Some("\"service_unavailable\"")
    }));

    gate.notify_one();
    let unavailable = tokio::time::timeout(Duration::from_secs(1), holder)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_json(unavailable).await["code"], "service_unavailable");
    let holder_events = events.snapshot();
    assert!(holder_events.iter().any(|event| {
        event.message() == Some("http_request") && event.field("termination").is_none()
    }));
}

async fn probe_admission_app() -> (
    crate::Application,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
) {
    #[utoipa::path(get, path = "/hold", responses((status = 200)))]
    async fn hold(
        axum::extract::State(gates): axum::extract::State<(
            Arc<tokio::sync::Notify>,
            Arc<tokio::sync::Notify>,
        )>,
    ) -> &'static str {
        let (started, gate) = gates;
        started.notify_one();
        gate.notified().await;
        "released"
    }
    #[utoipa::path(get, path = "/business/health/live", responses((status = 204)))]
    async fn business() -> StatusCode {
        StatusCode::NO_CONTENT
    }
    #[utoipa::path(head, path = "/health/live", responses((status = 204)))]
    async fn explicit_head() -> StatusCode {
        StatusCode::NO_CONTENT
    }
    let readiness = ready_control().await;
    let started = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let routes = crate::http_route!(business)
        .unwrap()
        .merge(crate::http_route!(explicit_head).unwrap())
        .unwrap()
        .merge(
            crate::http_route!(hold)
                .unwrap()
                .with_state((Arc::clone(&started), Arc::clone(&gate))),
        )
        .unwrap();
    let app = routes
        .with_probes("/")
        .unwrap()
        .with_probes("/api")
        .unwrap()
        .build(
            readiness.reader(),
            crate::Authentication::Public,
            &HardenOptions {
                max_in_flight: NonZeroU32::new(1),
                admission_wait: Duration::ZERO,
                request_timeout: Duration::from_secs(30),
                ..options()
            },
        )
        .unwrap();
    (app, started, gate)
}

#[tokio::test]
async fn probes_are_answered_while_every_in_flight_permit_is_held() {
    let (app, started, gate) = probe_admission_app().await;
    let holder = tokio::spawn({
        let app = app.clone();
        async move { app.oneshot(request(Method::GET, "/hold")).await.unwrap() }
    });
    started.notified().await;

    for (method, path) in [
        (Method::GET, "/business/health/live"),
        (Method::HEAD, "/health/live"),
        (Method::POST, "/health/live"),
    ] {
        let refused = app.clone().oneshot(request(method, path)).await.unwrap();
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    for probe in [
        "/health/live",
        "/health/ready",
        "/api/health/live",
        "/api/health/ready",
    ] {
        let response = app
            .clone()
            .oneshot(request(Method::GET, probe))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{probe}");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "ok",
            "{probe}"
        );
    }

    let head = app
        .clone()
        .oneshot(request(Method::HEAD, "/api/health/live"))
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert!(
        head.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );

    gate.notify_one();
    assert_eq!(holder.await.unwrap().status(), StatusCode::OK);
    let head = app
        .clone()
        .oneshot(request(Method::HEAD, "/health/live"))
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::NO_CONTENT);
    let unsupported = app
        .oneshot(request(Method::POST, "/health/live"))
        .await
        .unwrap();
    assert_eq!(unsupported.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn unset_in_flight_limit_admits_more_than_the_default_limit() {
    let started = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(tokio::sync::Notify::new());
    let routes = holding_routes(started.clone(), gate.clone());
    let mut options = options();
    options.max_in_flight = None;
    let app = harden(routes, &options);

    let mut holders = Vec::new();
    for _ in 0..3 {
        let app = app.clone();
        holders.push(tokio::spawn(async move {
            app.oneshot(request(Method::GET, "/hold")).await.unwrap()
        }));
    }
    tokio::time::timeout(Duration::from_millis(100), async {
        while started.load(Ordering::SeqCst) < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all three requests reach the handler");

    gate.notify_waiters();
    for holder in holders {
        assert_eq!(holder.await.unwrap().status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn options_is_not_answered_by_a_cors_layer() {
    let server = TestServer::new(app(&options()));
    let response = server
        .method(Method::OPTIONS, "/ok")
        .add_header("origin", "https://evil.example")
        .add_header("access-control-request-method", "GET")
        .await;
    response.assert_status(StatusCode::METHOD_NOT_ALLOWED);
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
}
