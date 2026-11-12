use axum::http::StatusCode;

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::header::{ALLOW, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{Method, Request as HttpRequest};
use axum::routing::{get, post};
use axum_test::TestServer;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::SubscriberExt;

use super::*;
use crate::request_id::REQUEST_ID_HEADER;

fn options() -> HardenOptions {
    HardenOptions {
        max_body_bytes: 64,
        request_timeout: Duration::from_millis(200),
        max_in_flight: NonZeroU32::new(2),
        log_health_probes: false,
    }
}

fn app(options: &HardenOptions) -> Router {
    let handler_1 = || async { "ok" };
    let handler_2 = || async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        "late"
    };
    let handler_3 = || async {
        let armed = std::hint::black_box(true);
        assert!(!armed, "boom {}", 42);
        "unreachable"
    };
    let handler_4 = |body: String| async move { body };
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = Router::new()
        .route("/ok", get(handler_1))
        .route("/slow", get(handler_2))
        .route("/panic", get(handler_3))
        .route("/echo", post(handler_4));
    harden(routes, options)
}

async fn body_json(response: Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
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
    let handler_1 = move |axum::Extension(deadline): axum::Extension<crate::RequestDeadline>| async move {
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
    // hyper writes `Content-Length` from the exact size hint; a layer
    // that wraps the body without forwarding it turns every response
    // into a chunked one.
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
async fn valid_inbound_request_id_is_echoed_and_invalid_replaced() {
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

#[tokio::test]
async fn a_body_the_caller_stops_sending_is_a_408_problem_that_closes_the_connection() {
    let first_chunk = futures_util::stream::once(async {
        Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"{"))
    });
    let stalled = futures_util::StreamExt::chain(first_chunk, futures_util::stream::pending());
    let request = HttpRequest::builder()
        .method(Method::POST)
        .uri("/echo")
        .body(Body::from_stream(stalled))
        .unwrap();

    let response = app(&options()).oneshot(request).await.unwrap();

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

struct EventFields {
    target: &'static str,
    fields: Arc<Mutex<Vec<String>>>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for EventFields {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() == self.target {
            event.record(
                &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                    self.fields
                        .lock()
                        .unwrap()
                        .push(format!("{}={value:?}", field.name()));
                },
            );
        }
    }
}

// Keep panic assertions under one capturing subscriber: an uninstrumented
// sibling can otherwise register the shared recovery callsite as disabled.
#[tokio::test]
async fn panic_is_a_sanitized_500_problem() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(EventFields {
        target: module_path!().trim_end_matches("::tests"),
        fields: Arc::clone(&events),
    });
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
    assert_eq!(*events.lock().unwrap(), ["message=http_handler_panicked"]);
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
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/problem+json"
    );
    assert_eq!(
        body_json(response).await["code"],
        "request_entity_too_large"
    );

    let small = HttpRequest::builder()
        .method(Method::POST)
        .uri("/echo")
        .body(Body::from("hello"))
        .unwrap();
    let response = app(&options()).oneshot(small).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn handler_413_keeps_retry_after_when_converted_to_problem() {
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
        .oneshot(request(Method::POST, "/upload"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(response.headers()[RETRY_AFTER], "60");
    assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
    assert!(!response.headers().contains_key("content-digest"));
    let length = response.headers().get(CONTENT_LENGTH).cloned();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    if let Some(length) = length {
        assert_eq!(
            length.to_str().unwrap().parse::<usize>().unwrap(),
            body.len()
        );
    }
    let problem: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(problem["code"], "request_entity_too_large");
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
    let started = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(tokio::sync::Notify::new());
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = holding_routes(started.clone(), gate.clone()).route("/ok", get(|| async { "ok" }));
    let mut options = options();
    // The holders must outlast the wait below.
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
}

/// Omit the opening deadline so the holder can outlive the waiter's patience.
#[tokio::test(start_paused = true)]
async fn a_request_that_waits_out_its_patience_is_shed_and_logged() {
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
    let app = routes
        .layer(middleware::from_fn_with_state(
            Arc::new(Admission {
                permits: Semaphore::new(1),
                patience,
            }),
            admit,
        ))
        .layer(middleware::from_fn_with_state(
            Observer::new(AccessLogOptions {
                log_health_probes: false,
            }),
            observe::observe,
        ));
    let fields = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing::Dispatch::new(tracing_subscriber::registry().with(EventFields {
        target: concat!(env!("CARGO_CRATE_NAME"), "::observe"),
        fields: Arc::clone(&fields),
    }));

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

    let shed_fields = std::mem::take(&mut *fields.lock().unwrap());
    assert!(
        shed_fields
            .iter()
            .any(|field| field == "message=http_request")
    );
    assert!(
        shed_fields
            .iter()
            .any(|field| field == "termination=\"load_shed\"")
    );

    gate.notify_one();
    let unavailable = tokio::time::timeout(Duration::from_secs(1), holder)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_json(unavailable).await["code"], "service_unavailable");
    let fields = fields.lock().unwrap();
    assert!(fields.iter().any(|field| field == "message=http_request"));
    assert!(!fields.iter().any(|field| field.starts_with("termination=")));
}

#[tokio::test]
async fn probes_are_answered_while_every_in_flight_permit_is_held() {
    let readiness = health::Readiness::new(
        Vec::new(),
        health::RefreshPolicy {
            interval: Duration::from_secs(1),
            probe_budget: Duration::from_secs(1),
            failure_threshold: 1,
        },
    );
    readiness.refresh().await;
    let started = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let handler_1 = {
        let started = started.clone();
        let gate = gate.clone();
        move || async move {
            started.notify_one();
            gate.notified().await;
            "released"
        }
    };
    let probes = crate::finalize_public(crate::router())
        .expect("the probe contract is public")
        .with_state(readiness.reader());
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let routes = probes
        .clone()
        .nest("/api", probes)
        .route("/hold", get(handler_1));
    let mut options = options();
    options.max_in_flight = NonZeroU32::new(1);
    // The holder keeps its permit, and the next request its wait, while the
    // probes run.
    options.request_timeout = Duration::from_secs(30);
    let app = harden(routes, &options);

    let holder = tokio::spawn({
        let app = app.clone();
        async move { app.oneshot(request(Method::GET, "/hold")).await.unwrap() }
    });
    started.notified().await;

    // The next request waits for the permit, so the instance is saturated
    // with a request queued behind it.
    let waiting = tokio::spawn({
        let app = app.clone();
        async move { app.oneshot(request(Method::GET, "/hold")).await.unwrap() }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiting.is_finished());
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

    gate.notify_one();
    assert_eq!(holder.await.unwrap().status(), StatusCode::OK);
    started.notified().await;
    gate.notify_one();
    assert_eq!(waiting.await.unwrap().status(), StatusCode::OK);
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
