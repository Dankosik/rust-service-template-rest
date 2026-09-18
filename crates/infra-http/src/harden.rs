//! The hardened middleware chain and fallback policy.
//!
//! Order is the contract, outermost first:
//!
//! request-id sanitize → set → propagate → nosniff → OpenTelemetry server span →
//! traceparent response header → HTTP metrics → access log → error mapping →
//! load shed → in-flight limit → request timeout → panic recovery →
//! body limit → extractor body limit → routes / 404 / 405
//!
//! Every layer is applied with `Router::layer`, so the 404 and 405 fallbacks
//! travel through the same chain. Cross-origin requests are fail-closed by
//! having no CORS layer at all: an empty `CorsLayer` would answer every
//! `OPTIONS` with 200 and hide the router's 405.

use std::any::Any;
use std::num::NonZeroU32;
use std::time::Duration;

use axum::body::Body;
use axum::error_handling::HandleErrorLayer;
use axum::extract::{DefaultBodyLimit, Extension, Request, State};
use axum::http::header::{CONTENT_LENGTH, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{BoxError, Router};
use axum_prometheus::{EndpointLabel, PrometheusMetricLayerBuilder};
use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
use http_body_util::Limited;
use tower::ServiceBuilder;
use tower::limit::GlobalConcurrencyLimitLayer;
use tower::load_shed::error::Overloaded;
use tower::timeout::error::Elapsed;
use tower::util::option_layer;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::request_id::{
    MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::access_log::{self, AccessLogOptions, UNMATCHED_ROUTE};
use crate::problem::{AT_CAPACITY_DETAIL, Code, Problem, SANITIZED_DETAIL};
use crate::request_id;

/// Retry hint on a shed request. Short on purpose: shedding means the server
/// is momentarily past capacity, not down.
const SHED_RETRY_AFTER: Duration = Duration::from_secs(1);

/// HTTP request-duration histogram name emitted by `axum-prometheus`.
/// The composition root passes this into the Prometheus recorder so buckets
/// match the adapter without naming `axum-prometheus` itself.
pub const HTTP_REQUESTS_DURATION_SECONDS: &str =
    axum_prometheus::AXUM_HTTP_REQUESTS_DURATION_SECONDS;

/// HTTP server metrics are emitted by `axum-prometheus` under its default
/// names: `axum_http_requests_total`, `axum_http_requests_duration_seconds`,
/// and `axum_http_requests_pending`, labelled by `method`, `endpoint` (the
/// matched route template or [`UNMATCHED_ROUTE`]), and `status`.
pub const HTTP_METRICS_NAMES: &[&str] = &[
    axum_prometheus::AXUM_HTTP_REQUESTS_TOTAL,
    HTTP_REQUESTS_DURATION_SECONDS,
    axum_prometheus::AXUM_HTTP_REQUESTS_PENDING,
];

/// Counter of requests rejected without running a handler because the
/// in-flight limit was reached.
pub const SHED_REQUESTS_METRIC: &str = "http_server_shed_requests_total";

/// Request-level policy for [`harden`].
#[derive(Clone, Debug)]
pub struct HardenOptions {
    /// Request body ceiling; overflow answers 413.
    pub max_body_bytes: usize,
    /// Per-request handler budget; expiry answers 504.
    pub request_timeout: Duration,
    /// Concurrent handler executions before 503; `None` disables shedding.
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
    let http_metrics = PrometheusMetricLayerBuilder::new()
        .with_endpoint_label_type(EndpointLabel::MatchedPathWithFallbackFn(|_| {
            UNMATCHED_ROUTE.to_owned()
        }))
        .build();
    let in_flight = options
        .max_in_flight
        .map(|limit| GlobalConcurrencyLimitLayer::new(limit.get() as usize));

    // `load_shed` plus `GlobalConcurrencyLimitLayer` reject with 503
    // instead of queueing. They sit inside `ServiceBuilder` on
    // `Router::layer` so 404 and 405 take the same chain.
    let chain = ServiceBuilder::new()
        .map_request(request_id::strip_invalid)
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetResponseHeaderLayer::overriding(
            X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(OtelAxumLayer::default())
        .layer(OtelInResponseLayer)
        .layer(http_metrics)
        .layer(middleware::from_fn_with_state(
            AccessLogOptions {
                log_health_probes: options.log_health_probes,
            },
            access_log::record,
        ))
        .layer(HandleErrorLayer::new(middleware_error))
        .load_shed()
        .layer(option_layer(in_flight))
        .timeout(options.request_timeout)
        .layer(CatchPanicLayer::custom(panic_to_problem))
        .layer(middleware::from_fn_with_state(
            BodyLimit(options.max_body_bytes),
            enforce_body_limit,
        ))
        .layer(DefaultBodyLimit::max(options.max_body_bytes));

    routes
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(chain)
}

/// Map the shedder and timeout errors to problem responses.
async fn middleware_error(id: Option<Extension<RequestId>>, err: BoxError) -> Response {
    let request_id = id
        .as_ref()
        .and_then(|Extension(id)| request_id::from_request_id(id));
    if err.is::<Overloaded>() {
        metrics::counter!(SHED_REQUESTS_METRIC).increment(1);
        return Problem::new(Code::ServiceUnavailable)
            .detail(AT_CAPACITY_DETAIL)
            .retry_after(SHED_RETRY_AFTER)
            .request_id(request_id)
            .into_response();
    }
    if err.is::<Elapsed>() {
        return Problem::new(Code::GatewayTimeout)
            .detail("request budget expired before a response could be committed")
            .request_id(request_id)
            .into_response();
    }
    tracing::error!(error = %err, "unclassified middleware error");
    Problem::new(Code::InternalServerError)
        .detail(SANITIZED_DETAIL)
        .request_id(request_id)
        .into_response()
}

/// A recovered panic becomes a sanitized 500. The payload is logged, never
/// echoed. The request id still reaches the caller through the propagated
/// header.
#[allow(clippy::needless_pass_by_value)] // `ResponseForPanic` hands over the box.
fn panic_to_problem(payload: Box<dyn Any + Send + 'static>) -> Response {
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic payload");
    tracing::error!(panic = message, "handler panicked");
    Problem::new(Code::InternalServerError)
        .detail(SANITIZED_DETAIL)
        .into_response()
}

#[derive(Clone, Copy, Debug)]
struct BodyLimit(usize);

/// Refuse a declared body above the limit before the handler runs, and cap
/// every byte read from an undeclared one. tower-http's `RequestBodyLimitLayer`
/// does the same with a `text/plain` body; this keeps the problem envelope.
async fn enforce_body_limit(
    State(BodyLimit(limit)): State<BodyLimit>,
    request: Request,
    next: Next,
) -> Response {
    let request_id = request_id::request_id(request.extensions());
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > limit) {
        return payload_too_large(request_id);
    }
    let request = request.map(|body| Body::new(Limited::new(body, limit)));
    let response = next.run(request).await;
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE
        && response.extensions().get::<Code>().is_none()
    {
        // An extractor hit the limit while streaming and answered with
        // axum's plain-text rejection; keep the envelope uniform.
        return payload_too_large(request_id);
    }
    response
}

fn payload_too_large(request_id: Option<String>) -> Response {
    Problem::new(Code::RequestEntityTooLarge)
        .detail("request body exceeds the configured limit")
        .request_id(request_id)
        .into_response()
}

async fn not_found(request: Request) -> Response {
    Problem::new(Code::NotFound)
        .detail("no resource at this path")
        .request_id(request_id::request_id(request.extensions()))
        .into_response()
}

async fn method_not_allowed(request: Request) -> Response {
    // axum appends the computed `Allow` header to this response.
    Problem::new(Code::MethodNotAllowed)
        .detail("method is not allowed for this resource")
        .request_id(request_id::request_id(request.extensions()))
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::http::header::{ALLOW, CONTENT_TYPE, RETRY_AFTER};
    use axum::http::{Method, Request as HttpRequest};
    use axum::routing::{get, post};
    use http_body_util::BodyExt;
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

    #[tokio::test]
    async fn success_carries_request_id_and_nosniff() {
        let response = app(&options())
            .oneshot(request(Method::GET, "/ok"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(X_CONTENT_TYPE_OPTIONS).unwrap(),
            "nosniff"
        );
        let id = response
            .headers()
            .get(&REQUEST_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(id.len(), 36, "generated UUIDv4: {id}");
    }

    #[tokio::test]
    async fn valid_inbound_request_id_is_echoed_and_invalid_replaced() {
        let mut req = request(Method::GET, "/ok");
        req.headers_mut()
            .insert(&REQUEST_ID_HEADER, HeaderValue::from_static("client-id_1"));
        let response = app(&options()).oneshot(req).await.unwrap();
        assert_eq!(
            response.headers().get(&REQUEST_ID_HEADER).unwrap(),
            "client-id_1"
        );

        let mut req = request(Method::GET, "/ok");
        req.headers_mut().insert(
            &REQUEST_ID_HEADER,
            HeaderValue::from_static("bad id with spaces"),
        );
        let response = app(&options()).oneshot(req).await.unwrap();
        assert_ne!(
            response.headers().get(&REQUEST_ID_HEADER).unwrap(),
            "bad id with spaces"
        );
    }

    #[tokio::test]
    async fn not_found_and_method_not_allowed_are_problems_with_allow() {
        let response = app(&options())
            .oneshot(request(Method::GET, "/missing"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
        let json = body_json(response).await;
        assert_eq!(json["code"], "not_found");
        assert!(json["request_id"].is_string());

        let response = app(&options())
            .oneshot(request(Method::DELETE, "/ok"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        let allow = response
            .headers()
            .get(ALLOW)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(allow.contains("GET"), "{allow}");
        assert_eq!(body_json(response).await["code"], "method_not_allowed");
    }

    #[tokio::test]
    async fn timeout_is_a_504_problem() {
        let response = app(&options())
            .oneshot(request(Method::GET, "/slow"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let json = body_json(response).await;
        assert_eq!(json["code"], "gateway_timeout");
        assert!(json["request_id"].is_string());
    }

    #[tokio::test]
    async fn panic_is_a_sanitized_500_problem() {
        let response = app(&options())
            .oneshot(request(Method::GET, "/panic"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(response.headers().contains_key(&REQUEST_ID_HEADER));
        let json = body_json(response).await;
        assert_eq!(json["code"], "internal_error");
        assert_eq!(json["detail"], SANITIZED_DETAIL);
        assert!(!json.to_string().contains("boom"));
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
    async fn zero_in_flight_disables_shedding() {
        let mut options = options();
        options.max_in_flight = None;
        let response = app(&options)
            .oneshot(request(Method::GET, "/ok"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn options_is_not_answered_by_a_cors_layer() {
        let mut req = request(Method::OPTIONS, "/ok");
        req.headers_mut()
            .insert("origin", HeaderValue::from_static("https://evil.example"));
        req.headers_mut().insert(
            "access-control-request-method",
            HeaderValue::from_static("GET"),
        );
        let response = app(&options()).oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
    }
}
