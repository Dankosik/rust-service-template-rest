//! The hardened middleware chain and fallback policy.
//!
//! Order is the contract, outermost first:
//!
//! request-id sanitize → set → nosniff → observation (OpenTelemetry server span,
//! HTTP metrics, request-id echo, problem completion, access log) → traceparent
//! response header → in-flight admission (waits for a permit, then sheds; probes
//! exempt) → opening context and deadline → panic recovery → body limit
//! → extractor body limit → routes / 404 / 405
//!
//! Every layer is applied with `Router::layer`, so the 404 and 405 fallbacks
//! travel through the same chain. Cross-origin requests are fail-closed by
//! having no CORS layer at all: an empty `CorsLayer` would answer every
//! `OPTIONS` with 200 and hide the router's 405.

use std::any::Any;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::header::{
    CONTENT_ENCODING, CONTENT_LENGTH, TRAILER, TRANSFER_ENCODING, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum_tracing_opentelemetry::middleware::OtelInResponseLayer;
use http_body::Body as _;
use http_body_util::{BodyExt as _, LengthLimitError, Limited};
use tokio::sync::Semaphore;
use tower::ServiceBuilder;
use tower::util::option_layer;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::request_id::SetRequestIdLayer;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::observe::{self, AccessLogOptions, Observer};
use crate::probes::is_health_probe_route;
#[cfg(test)]
use crate::problem::SANITIZED_DETAIL;
use crate::problem::{Code, Problem, body_too_large, sanitized_internal_error};
use crate::request_id;

/// The router-wide in-flight bound and how long a request waits for a permit.
struct Admission {
    permits: Semaphore,
    /// `http.request_timeout`: the request has no deadline until it is
    /// admitted, and the budget it then gets is what this wait leaves of it.
    patience: Duration,
}

#[derive(Clone, Debug)]
pub struct HardenOptions {
    /// A declared length above this ceiling answers 413 before the handler.
    /// Streaming bodies are checked as they are read. The ceiling applies
    /// both to the transport stream and to axum's extractor `DefaultBodyLimit`;
    /// axum's own default is independent, so omitting the extractor layer
    /// would keep a different ceiling than `http.max_body_bytes`.
    pub max_body_bytes: usize,
    /// Per-request budget; expiry answers 504 with code `gateway_timeout`, or
    /// 408 with code `request_timeout` while the handler waits for request
    /// body bytes the caller has not sent.
    /// Time a request spends waiting for an in-flight permit is spent from
    /// this same budget, so the handler gets only what admission leaves.
    pub request_timeout: Duration,
    /// Concurrent handler executions before 503; `None` disables shedding. A
    /// request over the bound waits for a permit for up to `request_timeout`
    /// and is shed only when none frees. The health probe routes are admitted
    /// without a permit, so a saturated service still answers its platform
    /// probes.
    pub max_in_flight: Option<NonZeroU32>,
    /// Re-enable access logging for matched health probe routes.
    pub log_health_probes: bool,
}

pub fn harden(routes: Router, options: &HardenOptions) -> Router {
    observe::describe();
    // One semaphore for the whole router: `Router::layer` applies the chain
    // to every route, so a per-layer limit would be a per-route limit.
    let in_flight = options.max_in_flight.map(|limit| {
        // Published at zero, so a scrape tells "never shed" from "not exported".
        metrics::counter!(observe::SHED_REQUESTS_METRIC).increment(0);
        middleware::from_fn_with_state(
            Arc::new(Admission {
                permits: Semaphore::new(limit.get() as usize),
                patience: options.request_timeout,
            }),
            admit,
        )
    });

    let chain = ServiceBuilder::new()
        .map_request(request_id::sanitize)
        .layer(SetRequestIdLayer::new(
            request_id::REQUEST_ID_HEADER,
            request_id::MakeRequestUuid,
        ))
        .layer(SetResponseHeaderLayer::overriding(
            X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        // Outside opening enforcement, so every Problem is completed before it is
        // counted and logged.
        .layer(middleware::from_fn_with_state(
            Observer::new(AccessLogOptions {
                log_health_probes: options.log_health_probes,
            }),
            observe::observe,
        ))
        .layer(OtelInResponseLayer)
        .layer(option_layer(in_flight))
        .layer(middleware::from_fn_with_state(
            options.request_timeout,
            crate::context::opening,
        ))
        .layer(CatchPanicLayer::custom(panic_to_problem))
        .layer(middleware::from_fn_with_state(
            options.max_body_bytes,
            limit_body,
        ))
        .layer(DefaultBodyLimit::max(options.max_body_bytes));

    #[expect(
        clippy::disallowed_methods,
        reason = "the hardened transport owns standardized 404 and 405 responses"
    )]
    let routes = routes
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed);
    routes.layer(chain)
}

async fn limit_body(State(max_bytes): State<usize>, request: Request, next: Next) -> Response {
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok()?.parse::<usize>().ok());
    let limit = declared.unwrap_or(max_bytes);
    if limit > max_bytes {
        return body_too_large().into_response();
    }
    if request.body().is_end_stream() {
        return next.run(request).await;
    }
    let overflowed = Arc::new(AtomicBool::new(false));
    let request = request.map(|body| {
        let overflowed = Arc::clone(&overflowed);
        Body::new(Limited::new(body, limit).map_err(move |error| {
            if error.is::<LengthLimitError>() {
                overflowed.store(true, Ordering::Relaxed);
            }
            error
        }))
    });
    let mut response = next.run(request).await;
    // Axum's raw extractors lose the cause when they render their 413.
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE
        && overflowed.load(Ordering::Relaxed)
        && response.extensions().get::<Problem>().is_none()
    {
        let (parts, body) = body_too_large().into_response().into_parts();
        let headers = response.headers_mut();
        for name in [CONTENT_ENCODING, CONTENT_LENGTH, TRAILER, TRANSFER_ENCODING] {
            headers.remove(name);
        }
        for name in ["content-digest", "repr-digest", "digest", "content-md5"] {
            headers.remove(name);
        }
        headers.extend(parts.headers);
        response.extensions_mut().extend(parts.extensions);
        *response.body_mut() = body;
    }
    response
}

/// Admit a request once an in-flight permit is free. A request over the bound
/// waits for one, in arrival order, for up to `request_timeout`: it is served
/// once capacity frees, and refusing it earlier would refuse work the process
/// could still do. Only a request that finds none in that time is shed, with
/// 503 and `Retry-After`. The permit is held until the response head is ready.
/// The health probe routes skip the limit: a saturated service is busy, not
/// dead, and a shed liveness probe would have the platform restart it under
/// load. They are matched by route template, also beneath an outer nest, so
/// only the probe handlers are exempt.
///
/// The wait sits outside the opening deadline, so the request's arrival is
/// handed on and the opening budget spends the wait from `request_timeout`
/// instead of starting over.
async fn admit(
    State(admission): State<Arc<Admission>>,
    matched: Option<MatchedPath>,
    mut request: Request,
    next: Next,
) -> Response {
    if matched.is_some_and(|route| is_health_probe_route(route.as_str())) {
        return next.run(request).await;
    }
    let arrived = tokio::time::Instant::now();
    let _permit = if let Ok(permit) = admission.permits.try_acquire() {
        permit
    } else {
        let waited = tokio::time::timeout(admission.patience, admission.permits.acquire()).await;
        let elapsed = arrived.elapsed();
        metrics::histogram!(observe::ADMISSION_WAIT_METRIC).record(elapsed.as_secs_f64());
        // Tokio's timeout can return a ready permit after the wait budget.
        match waited {
            Ok(Ok(permit)) if elapsed < admission.patience => permit,
            _ => return shed_response(),
        }
    };
    request
        .extensions_mut()
        .insert(crate::context::Arrival(arrived));
    next.run(request).await
}

#[derive(Clone, Copy)]
pub(super) struct AdmissionShed;

fn shed_response() -> Response {
    metrics::counter!(observe::SHED_REQUESTS_METRIC).increment(1);
    let problem = Problem::new(Code::ServiceUnavailable)
        .detail(service_failure::AT_CAPACITY_DETAIL)
        .retry_after(service_failure::AT_CAPACITY_RETRY_AFTER);
    (axum::Extension(AdmissionShed), problem).into_response()
}

/// A recovered panic becomes a sanitized 500 without recording its payload.
fn panic_to_problem(_payload: Box<dyn Any + Send + 'static>) -> Response {
    tracing::error!("http_handler_panicked");
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
mod tests;
