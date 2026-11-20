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
use axum::http::{HeaderName, HeaderValue, StatusCode, Version};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use health::ReadinessReader;
use http_body::Body as _;
use http_body_util::{BodyExt as _, LengthLimitError, Limited};
use tokio::sync::Semaphore;
use tower::ServiceBuilder;
use tower::util::option_layer;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::request_id::{RequestId, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::context::ResponseContext;
use crate::observe::{self, Observer};
use crate::probes::ProbeRoutes;
#[cfg(test)]
use crate::problem::SANITIZED_DETAIL;
use crate::problem::{Code, Problem, body_too_large, sanitized_internal_error};
use crate::request_id;

struct Admission {
    permits: Semaphore,
    max_wait: Duration,
    probes: ProbeRoutes,
}

#[derive(Clone, Debug)]
pub struct HardenOptions {
    pub max_body_bytes: usize,
    pub request_timeout: Duration,
    pub max_in_flight: Option<NonZeroU32>,
    pub admission_wait: Duration,
    pub log_health_probes: bool,
    pub trust_trace_context: bool,
    pub draining: Option<ReadinessReader>,
}

#[cfg(test)]
pub(crate) fn harden(routes: Router, options: &HardenOptions) -> Router {
    assemble(routes, options, ProbeRoutes::default())
}

pub(crate) fn assemble(routes: Router, options: &HardenOptions, probes: ProbeRoutes) -> Router {
    observe::describe();
    let in_flight = options.max_in_flight.map(|limit| {
        metrics::counter!(observe::SHED_REQUESTS_METRIC).increment(0);
        middleware::from_fn_with_state(
            Arc::new(Admission {
                permits: Semaphore::new(limit.get() as usize),
                max_wait: options.admission_wait.min(options.request_timeout),
                probes: probes.clone(),
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
            Observer::new(
                options.log_health_probes,
                options.trust_trace_context,
                probes,
            ),
            observe::observe,
        ))
        .layer(option_layer(in_flight))
        .layer(middleware::from_fn_with_state(
            options.request_timeout,
            crate::context::enforce_request_budget,
        ))
        .layer(CatchPanicLayer::custom(panic_to_problem))
        .layer(middleware::from_fn_with_state(
            options.max_body_bytes,
            limit_body,
        ))
        .layer(DefaultBodyLimit::disable());

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
    if http1 && readiness.is_draining() {
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

const DIGEST_HEADERS: [HeaderName; 4] = [
    HeaderName::from_static("content-digest"),
    HeaderName::from_static("repr-digest"),
    HeaderName::from_static("digest"),
    HeaderName::from_static("content-md5"),
];

fn normalize_overflow_response(response: &mut Response) {
    let (parts, body) = body_too_large().into_response().into_parts();
    let headers = response.headers_mut();
    for name in [CONTENT_ENCODING, CONTENT_LENGTH, TRAILER, TRANSFER_ENCODING] {
        headers.remove(name);
    }
    for name in &DIGEST_HEADERS {
        headers.remove(name);
    }
    headers.extend(parts.headers);
    response.extensions_mut().extend(parts.extensions);
    *response.body_mut() = body;
}

async fn admit(
    State(admission): State<Arc<Admission>>,
    mut request: Request,
    next: Next,
) -> Response {
    if request
        .extensions()
        .get::<MatchedPath>()
        .is_some_and(|route| admission.probes.contains(request.method(), route.as_str()))
    {
        return next.run(request).await;
    }
    let arrived = tokio::time::Instant::now();
    let _permit =
        match infra_http_transport::acquire_borrowed_within(&admission.permits, admission.max_wait)
            .await
        {
            infra_http_transport::Admission::Immediate(permit) => permit,
            infra_http_transport::Admission::Waited { permit, waited } => {
                metrics::histogram!(observe::ADMISSION_WAIT_METRIC).record(waited);
                permit
            }
            infra_http_transport::Admission::Shed { waited } => {
                metrics::histogram!(observe::ADMISSION_WAIT_METRIC).record(waited);
                metrics::counter!(observe::SHED_REQUESTS_METRIC).increment(1);
                return shed_response(&request);
            }
        };
    request
        .extensions_mut()
        .insert(crate::context::Arrival(arrived));
    next.run(request).await
}

#[derive(Clone, Copy)]
pub(super) struct AdmissionShed;

fn shed_response(request: &Request) -> Response {
    let problem = Problem::new(Code::ServiceUnavailable)
        .detail_static(service_failure::AT_CAPACITY_DETAIL)
        .retry_after(service_failure::AT_CAPACITY_RETRY_AFTER);
    let extensions = request.extensions();
    let mut response = if extensions.get::<ResponseContext>().is_some()
        && extensions
            .get::<RequestId>()
            .is_some_and(|id| id.header_value().to_str().is_ok())
    {
        crate::problem::pending_problem(problem)
    } else {
        problem.into_response()
    };
    response.extensions_mut().insert(AdmissionShed);
    response
}

fn panic_to_problem(_payload: Box<dyn Any + Send + 'static>) -> Response {
    tracing::error!("http_handler_panicked");
    sanitized_internal_error()
}

async fn not_found() -> Response {
    Problem::new(Code::NotFound)
        .detail_static("no resource at this path")
        .into_response()
}

pub(crate) fn method_not_allowed_response() -> Response {
    Problem::new(Code::MethodNotAllowed)
        .detail_static("method is not allowed for this resource")
        .into_response()
}

async fn method_not_allowed() -> Response {
    method_not_allowed_response()
}

#[cfg(test)]
mod tests;
