use std::any::Any;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::header::{
    CONNECTION, CONTENT_ENCODING, CONTENT_LENGTH, TRAILER, TRANSFER_ENCODING,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, StatusCode, Version};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum_tracing_opentelemetry::middleware::OtelInResponseLayer;
use health::{NotReady, ReadinessReader};
use http_body::Body as _;
use http_body_util::{BodyExt as _, LengthLimitError, Limited};
use tokio::sync::Semaphore;
use tower::ServiceBuilder;
use tower::util::option_layer;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::request_id::SetRequestIdLayer;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::observe::{self, Observer};
use crate::probes::is_health_probe_route;
#[cfg(test)]
use crate::problem::SANITIZED_DETAIL;
use crate::problem::{Code, Problem, body_too_large, sanitized_internal_error};
use crate::request_id;

struct Admission {
    permits: Semaphore,
    patience: Duration,
}

#[derive(Clone, Debug)]
pub struct HardenOptions {
    pub max_body_bytes: usize,
    pub request_timeout: Duration,
    pub max_in_flight: Option<NonZeroU32>,
    pub admission_wait: Duration,
    pub log_health_probes: bool,
    /// While this reader reports draining, HTTP/1 responses carry
    /// `Connection: close`, so keep-alive clients reconnect elsewhere before
    /// the listener stops instead of losing a request sent to a closed socket.
    pub draining: Option<ReadinessReader>,
}

pub fn harden(routes: Router, options: &HardenOptions) -> Router {
    observe::describe();
    let in_flight = options.max_in_flight.map(|limit| {
        metrics::counter!(observe::SHED_REQUESTS_METRIC).increment(0);
        middleware::from_fn_with_state(
            Arc::new(Admission {
                permits: Semaphore::new(limit.get() as usize),
                patience: options.admission_wait,
            }),
            admit,
        )
    });

    let closing = options
        .draining
        .clone()
        .map(|draining| middleware::from_fn_with_state(draining, close_while_draining));

    let chain = ServiceBuilder::new()
        .layer(option_layer(closing))
        .map_request(request_id::sanitize)
        .layer(SetRequestIdLayer::new(
            request_id::REQUEST_ID_HEADER,
            request_id::MakeRequestUuid,
        ))
        .layer(SetResponseHeaderLayer::overriding(
            X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(middleware::from_fn_with_state(
            Observer::new(options.log_health_probes),
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

async fn close_while_draining(
    State(readiness): State<ReadinessReader>,
    request: Request,
    next: Next,
) -> Response {
    let http1 = request.version() <= Version::HTTP_11;
    let mut response = next.run(request).await;
    if http1 && matches!(readiness.verdict(), Err(NotReady::Draining)) {
        response
            .headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("close"));
    }
    response
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
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE
        && overflowed.load(Ordering::Relaxed)
        && response.extensions().get::<Problem>().is_none()
    {
        normalize_overflow_response(&mut response);
    }
    response
}

fn normalize_overflow_response(response: &mut Response) {
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
    Problem::new(Code::MethodNotAllowed)
        .detail("method is not allowed for this resource")
        .into_response()
}

#[cfg(test)]
mod tests;
