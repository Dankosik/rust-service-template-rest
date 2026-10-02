//! The hardened middleware chain and fallback policy.
//!
//! Order is the contract, outermost first:
//!
//! request-id sanitize → set → propagate → nosniff → observation (OpenTelemetry
//! server span, HTTP metrics, problem completion, access log) → traceparent
//! response header → in-flight admission (shed, probes exempt) → error mapping
//! → request deadline → request timeout → panic recovery → body limit
//! (tower-http) → extractor body limit → routes / 404 / 405
//!
//! Every layer is applied with `Router::layer`, so the 404 and 405 fallbacks
//! travel through the same chain. Cross-origin requests are fail-closed by
//! having no CORS layer at all: an empty `CorsLayer` would answer every
//! `OPTIONS` with 200 and hide the router's 405.

use std::any::Any;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::error_handling::HandleErrorLayer;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::header::X_CONTENT_TYPE_OPTIONS;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{BoxError, Router};
use axum_tracing_opentelemetry::middleware::OtelInResponseLayer;
use tokio::sync::Semaphore;
use tower::ServiceBuilder;
use tower::timeout::error::Elapsed;
use tower::util::option_layer;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::observe::{self, AccessLogOptions};
#[cfg(test)]
use crate::problem::SANITIZED_DETAIL;
use crate::problem::{AT_CAPACITY_DETAIL, Code, Problem, sanitized_internal_error};
use crate::request_id;
use crate::router::HEALTH_PROBE_ROUTES;

// template:begin request-budget:http-request-deadline
/// The conservative deadline shared with request-scoped dependencies.
///
/// Only this module constructs it, immediately before the tower timeout that
/// remains the final 504 authority. Consumers may observe the instant but
/// cannot introduce a second request budget.
#[derive(Clone, Copy, Debug)]
pub struct RequestDeadline(tokio::time::Instant);

impl RequestDeadline {
    fn from_timeout(timeout: Duration) -> Self {
        Self(tokio::time::Instant::now() + timeout)
    }

    /// The existing absolute request deadline; observing it never extends the budget.
    #[must_use]
    pub const fn at(&self) -> tokio::time::Instant {
        self.0
    }
}
// template:end request-budget:http-request-deadline

/// Retry hint on a shed request. Short on purpose: shedding means the server
/// is momentarily past capacity, not down.
const SHED_RETRY_AFTER: Duration = Duration::from_secs(1);

/// HTTP request-duration histogram name. The composition root passes it with
/// [`HTTP_REQUESTS_DURATION_BUCKETS`] into the Prometheus recorder.
pub const HTTP_REQUESTS_DURATION_SECONDS: &str = "http_server_request_duration_seconds";

/// Buckets in seconds for [`HTTP_REQUESTS_DURATION_SECONDS`], shaped for an
/// HTTP API.
pub const HTTP_REQUESTS_DURATION_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

pub(crate) const HTTP_ACTIVE_REQUESTS: &str = "http_server_active_requests";

/// HTTP server metrics, under the OpenTelemetry HTTP semantic-convention
/// names as Prometheus renders them: `http_server_request_duration_seconds`,
/// labelled by `http_request_method`, `http_route` (the matched route template
/// or `<unmatched>`), and `http_response_status_code`; and
/// `http_server_active_requests`, labelled by `http_request_method`. The
/// request count is the histogram's `_count` series.
pub const HTTP_METRICS_NAMES: &[&str] = &[HTTP_REQUESTS_DURATION_SECONDS, HTTP_ACTIVE_REQUESTS];

/// Counter of requests rejected without running a handler because the
/// in-flight limit was reached.
pub const SHED_REQUESTS_METRIC: &str = "http_server_shed_requests_total";

/// Request-level policy for [`harden`].
#[derive(Clone, Debug)]
pub struct HardenOptions {
    /// Request body ceiling; overflow answers 413. Applied both to tower-http's
    /// `RequestBodyLimitLayer` and to axum's extractor `DefaultBodyLimit`;
    /// axum's own default is independent, so omitting the extractor layer
    /// would keep a different ceiling than `http.max_body_bytes`.
    pub max_body_bytes: usize,
    /// Per-request handler budget; expiry answers 504 with code
    /// `gateway_timeout`.
    pub request_timeout: Duration,
    /// Concurrent handler executions before 503; `None` disables shedding.
    /// The health probe routes are admitted without a permit, so a saturated
    /// service still answers its platform probes.
    pub max_in_flight: Option<NonZeroU32>,
    /// Re-enable access logging for matched health probe routes.
    pub log_health_probes: bool,
}

/// Wrap `routes` in the hardened chain and fallback policy.
pub fn harden(routes: Router, options: &HardenOptions) -> Router {
    metrics::describe_counter!(
        SHED_REQUESTS_METRIC,
        metrics::Unit::Count,
        "Requests rejected without running a handler because the in-flight limit was reached."
    );
    metrics::describe_gauge!(
        HTTP_ACTIVE_REQUESTS,
        metrics::Unit::Count,
        "Number of active HTTP server requests."
    );
    metrics::describe_histogram!(
        HTTP_REQUESTS_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Duration of HTTP server requests."
    );
    // One semaphore for the whole router: `Router::layer` applies the chain
    // to every route, so a per-layer limit would be a per-route limit.
    let in_flight = options.max_in_flight.map(|limit| {
        middleware::from_fn_with_state(Arc::new(Semaphore::new(limit.get() as usize)), admit)
    });
    // template:begin request-budget:http-request-deadline-mapper-state
    let request_timeout = options.request_timeout;
    // template:end request-budget:http-request-deadline-mapper-state

    // The chain sits inside `ServiceBuilder` on `Router::layer` so 404 and
    // 405 take it too.
    let chain = ServiceBuilder::new()
        .map_request(request_id::strip_invalid)
        .layer(SetRequestIdLayer::new(
            request_id::REQUEST_ID_HEADER,
            request_id::MakeRequestUuid,
        ))
        .layer(PropagateRequestIdLayer::new(request_id::REQUEST_ID_HEADER))
        .layer(SetResponseHeaderLayer::overriding(
            X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        // Outside error mapping, so every Problem is completed before it is
        // counted and logged.
        .layer(middleware::from_fn_with_state(
            AccessLogOptions {
                log_health_probes: options.log_health_probes,
            },
            observe::observe,
        ))
        .layer(OtelInResponseLayer)
        .layer(option_layer(in_flight))
        .layer(HandleErrorLayer::new(middleware_error))
        // template:begin request-budget:http-request-deadline-mapper
        .map_request(move |mut request: Request| {
            request
                .extensions_mut()
                .insert(RequestDeadline::from_timeout(request_timeout));
            request
        })
        // template:end request-budget:http-request-deadline-mapper
        .timeout(options.request_timeout)
        .layer(CatchPanicLayer::custom(panic_to_problem))
        .layer(RequestBodyLimitLayer::new(options.max_body_bytes))
        .layer(DefaultBodyLimit::max(options.max_body_bytes));

    #[allow(
        clippy::disallowed_methods,
        reason = "the hardened transport owns standardized 404 and 405 responses"
    )]
    let routes = routes
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed);
    routes.layer(chain)
}

/// Admit a request while an in-flight permit is free, otherwise shed it with
/// 503 instead of queueing. The permit is held until the response head is
/// ready. The health probe routes skip the limit: a saturated service is
/// busy, not dead, and a shed liveness probe would have the platform restart
/// it under load. They are matched by route template, so only the probe
/// handlers are exempt.
async fn admit(
    State(permits): State<Arc<Semaphore>>,
    matched: Option<MatchedPath>,
    request: Request,
    next: Next,
) -> Response {
    if matched.is_some_and(|route| HEALTH_PROBE_ROUTES.contains(&route.as_str())) {
        return next.run(request).await;
    }
    let Ok(_permit) = permits.try_acquire() else {
        metrics::counter!(SHED_REQUESTS_METRIC).increment(1);
        return Problem::new(Code::ServiceUnavailable)
            .detail(AT_CAPACITY_DETAIL)
            .retry_after(SHED_RETRY_AFTER)
            .into_response();
    };
    next.run(request).await
}

/// Map the timeout error to a problem response.
async fn middleware_error(err: BoxError) -> Response {
    if err.is::<Elapsed>() {
        return Problem::new(Code::GatewayTimeout)
            .detail("request budget expired before a response could be committed")
            .into_response();
    }
    tracing::error!(error = %err, "unclassified middleware error");
    sanitized_internal_error()
}

/// Complete every Problem the chain or a handler returns: a bare 413 from
/// tower-http's body limit or an axum extractor takes the Problem envelope,
/// and every Problem body gains the request id.
pub(crate) fn complete_problem(mut response: Response, request_id: Option<&str>) -> Response {
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE
        && response.extensions().get::<Problem>().is_none()
    {
        response = Problem::new(Code::RequestEntityTooLarge)
            .detail("request body exceeds the configured limit")
            .into_response();
    }
    let Some(id) = request_id else {
        return response;
    };
    let Some(problem) = response.extensions_mut().remove::<Problem>() else {
        return response;
    };
    let problem = problem.with_request_id(id.to_owned());
    // Only `Problem::into_response` attaches the extension, so status and
    // headers (Allow, WWW-Authenticate, Retry-After) are already right.
    *response.body_mut() = axum::Json(&problem).into_response().into_body();
    response.extensions_mut().insert(problem);
    response
}

/// A recovered panic becomes a sanitized 500. The payload is logged, never
/// echoed. Problem completion adds the request id to the body.
#[allow(clippy::needless_pass_by_value)] // `ResponseForPanic` hands over the box.
fn panic_to_problem(payload: Box<dyn Any + Send + 'static>) -> Response {
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic payload");
    tracing::error!(panic = message, "handler panicked");
    sanitized_internal_error()
}

async fn not_found() -> Response {
    Problem::new(Code::NotFound)
        .detail("no resource at this path")
        .into_response()
}

async fn method_not_allowed() -> Response {
    // axum appends the computed `Allow` header to this response.
    Problem::new(Code::MethodNotAllowed)
        .detail("method is not allowed for this resource")
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::header::{ALLOW, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
    use axum::http::{Method, Request as HttpRequest};
    use axum::routing::{get, post};
    use axum_test::TestServer;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

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

    #[allow(
        clippy::disallowed_methods,
        reason = "transport fixture exercises the middleware independently of contract finalization"
    )]
    fn app(options: &HardenOptions) -> Router {
        let routes = Router::new()
            .route("/ok", get(|| async { "ok" }))
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    "late"
                }),
            )
            .route(
                "/panic",
                get(|| async {
                    let armed = std::hint::black_box(true);
                    assert!(!armed, "boom {}", 42);
                    "unreachable"
                }),
            )
            .route("/echo", post(|body: String| async move { body }));
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
    #[allow(
        clippy::disallowed_methods,
        reason = "transport fixture exercises the middleware independently of contract finalization"
    )]
    async fn request_deadline_is_observable_without_restarting_the_budget() {
        let options = options();
        let started = tokio::time::Instant::now();
        let expected = started + options.request_timeout;
        let routes = Router::new().route(
            "/deadline",
            get(move |axum::Extension(deadline): axum::Extension<crate::RequestDeadline>| async move {
                assert_eq!(deadline.at(), expected);
                tokio::time::sleep(Duration::from_millis(50)).await;
                assert_eq!(deadline.at(), expected);
                assert_eq!(
                    deadline.at() - tokio::time::Instant::now(),
                    Duration::from_millis(150)
                );
                "within budget"
            }),
        );
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
    async fn panic_is_a_sanitized_500_problem() {
        let server = TestServer::new(app(&options()));
        let response = server.get("/panic").await;
        response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
        response.assert_header(CONTENT_TYPE, "application/problem+json");
        assert!(response.headers().contains_key(&REQUEST_ID_HEADER));
        let json = response.json::<Value>();
        assert_eq!(json["code"], "internal_error");
        assert_eq!(json["detail"], SANITIZED_DETAIL);
        assert!(!json.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn panic_problem_body_request_id_matches_the_header() {
        let server = TestServer::new(app(&options()));
        let response = server.get("/panic").await;
        response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
        let id = response.header(REQUEST_ID_HEADER);
        assert_eq!(
            response.json::<Value>()["request_id"].as_str(),
            Some(id.to_str().unwrap())
        );
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

    #[tokio::test]
    #[allow(
        clippy::disallowed_methods,
        reason = "transport fixture exercises the middleware independently of contract finalization"
    )]
    async fn shedding_answers_503_with_retry_after_without_queueing() {
        let started = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Notify::new());
        let routes = Router::new().route(
            "/hold",
            get({
                let started = started.clone();
                let gate = gate.clone();
                move || {
                    let started = started.clone();
                    let gate = gate.clone();
                    async move {
                        started.fetch_add(1, Ordering::SeqCst);
                        gate.notified().await;
                        "released"
                    }
                }
            }),
        );
        let app = harden(routes, &options());

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

        let shed = tokio::time::timeout(
            Duration::from_millis(100),
            app.clone().oneshot(request(Method::GET, "/hold")),
        )
        .await
        .expect("shedding must not queue")
        .unwrap();
        assert_eq!(shed.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(shed.headers().get(RETRY_AFTER).unwrap(), "1");
        let json = body_json(shed).await;
        assert_eq!(json["code"], "service_unavailable");
        assert_eq!(json["detail"], AT_CAPACITY_DETAIL);

        gate.notify_waiters();
        for holder in holders {
            assert_eq!(holder.await.unwrap().status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    #[allow(
        clippy::disallowed_methods,
        reason = "transport fixture exercises the middleware independently of contract finalization"
    )]
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
        let routes = crate::finalize_public(crate::router())
            .expect("the probe contract is public")
            .with_state(readiness.reader())
            .route(
                "/hold",
                get({
                    let started = started.clone();
                    let gate = gate.clone();
                    move || async move {
                        started.notify_one();
                        gate.notified().await;
                        "released"
                    }
                }),
            );
        let mut options = options();
        options.max_in_flight = NonZeroU32::new(1);
        // The holder must still own its permit when the next request arrives.
        options.request_timeout = Duration::from_secs(30);
        let app = harden(routes, &options);

        let holder = tokio::spawn({
            let app = app.clone();
            async move { app.oneshot(request(Method::GET, "/hold")).await.unwrap() }
        });
        started.notified().await;

        let shed = app
            .clone()
            .oneshot(request(Method::GET, "/hold"))
            .await
            .unwrap();
        assert_eq!(shed.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(shed).await["code"], "service_unavailable");
        for probe in ["/health/live", "/health/ready"] {
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
    }

    #[tokio::test]
    async fn zero_in_flight_disables_shedding() {
        let mut options = options();
        options.max_in_flight = None;
        let server = TestServer::new(app(&options));
        server.get("/ok").await.assert_status_ok();
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
}
