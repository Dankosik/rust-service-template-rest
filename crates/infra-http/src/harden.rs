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
use axum::response::Response;
use health::ReadinessReader;
use http_body::Body as _;
use http_body_util::{BodyExt as _, LengthLimitError, Limited};
use opentelemetry::context::FutureExt as _;
use tokio::sync::Semaphore;
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tracing::Instrument as _;

use crate::context;
use crate::observe::{self, Observer};
use crate::probes::ProbeRoutes;
#[cfg(test)]
use crate::problem::SANITIZED_DETAIL;
use crate::problem::{Code, Problem, body_too_large, pending_problem, sanitized_problem};
use crate::request_id;

struct Admission {
    permits: Semaphore,
    max_wait: Duration,
}

struct Hardened {
    observer: Arc<Observer>,
    admission: Option<Admission>,
    request_timeout: Duration,
    max_body_bytes: usize,
    draining: Option<ReadinessReader>,
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

static ADMISSION_METRIC_METADATA: metrics::Metadata<'static> =
    metrics::Metadata::new(module_path!(), metrics::Level::INFO, Some(module_path!()));
static ADMISSION_WAIT_KEY: metrics::Key =
    metrics::Key::from_static_name(observe::ADMISSION_WAIT_METRIC);
static SHED_REQUESTS_KEY: metrics::Key =
    metrics::Key::from_static_name(observe::SHED_REQUESTS_METRIC);

fn admission_wait_histogram() -> metrics::Histogram {
    metrics::with_recorder(|recorder| {
        recorder.register_histogram(&ADMISSION_WAIT_KEY, &ADMISSION_METRIC_METADATA)
    })
}

fn shed_requests_counter() -> metrics::Counter {
    metrics::with_recorder(|recorder| {
        recorder.register_counter(&SHED_REQUESTS_KEY, &ADMISSION_METRIC_METADATA)
    })
}

pub(crate) fn assemble(routes: Router, options: &HardenOptions, probes: ProbeRoutes) -> Router {
    observe::describe();
    let admission = options.max_in_flight.map(|limit| {
        shed_requests_counter().increment(0);
        Admission {
            permits: Semaphore::new(limit.get() as usize),
            max_wait: options.admission_wait.min(options.request_timeout),
        }
    });

    let state = Arc::new(Hardened {
        observer: Observer::new(
            options.log_health_probes,
            options.trust_trace_context,
            probes,
        ),
        admission,
        request_timeout: options.request_timeout,
        max_body_bytes: options.max_body_bytes,
        draining: options.draining.clone(),
    });

    let chain = ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::overriding(
            X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(middleware::from_fn_with_state(state, handle))
        .layer(CatchPanicLayer::custom(panic_to_problem))
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

async fn handle(
    State(state): State<Arc<Hardened>>,
    matched: Option<MatchedPath>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = request_id::ensure(&mut request);
    let version = request.version();
    let (mut observation, is_probe) =
        state
            .observer
            .start(&mut request, matched, request_id, state.admission.is_some());
    let response_id = observation.response_request_id();
    let span = observation.span().clone();
    let fallback = observation.take_fallback_context();

    let traced = async {
        let (mut response, cancel_guard) = run_admitted(&state, is_probe, request, next).await;
        let problem_code = context::attach_request_id(&mut response, response_id);
        (response, cancel_guard, problem_code)
    }
    .instrument(span);

    let (mut response, cancel_guard, problem_code) = match fallback {
        None => traced.await,
        Some(context) => traced.with_context(context).await,
    };

    if version <= Version::HTTP_11
        && state
            .draining
            .as_ref()
            .is_some_and(ReadinessReader::is_draining)
    {
        response
            .headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("close"));
    }

    observation.finish(response, cancel_guard, problem_code)
}

async fn run_admitted(
    state: &Hardened,
    is_probe: bool,
    request: Request,
    next: Next,
) -> (Response, Option<context::CancelGuard>) {
    let (arrived, permit) = match (&state.admission, is_probe) {
        (Some(admission), false) => {
            let arrived = tokio::time::Instant::now();
            match infra_http_transport::acquire_borrowed_within(
                &admission.permits,
                admission.max_wait,
                || (),
            )
            .await
            {
                infra_http_transport::Admission::Immediate(permit) => (Some(arrived), Some(permit)),
                infra_http_transport::Admission::Waited { permit, waited } => {
                    admission_wait_histogram().record(waited);
                    (Some(arrived), Some(permit))
                }
                infra_http_transport::Admission::Shed { waited } => {
                    admission_wait_histogram().record(waited);
                    shed_requests_counter().increment(1);
                    return (shed_response(), None);
                }
            }
        }
        _ => (None, None),
    };
    context::enforce_request_budget(state.request_timeout, arrived, permit, request, |request| {
        limit_body(state.max_body_bytes, request, next)
    })
    .await
}

async fn limit_body(max_bytes: usize, request: Request, next: Next) -> Response {
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok()?.parse::<usize>().ok());
    let limit = declared.unwrap_or(max_bytes);
    if limit > max_bytes {
        return pending_problem(body_too_large());
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
    let (parts, body) = pending_problem(body_too_large()).into_parts();
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

#[derive(Clone, Copy)]
pub(super) struct AdmissionShed;

fn shed_response() -> Response {
    let mut response = pending_problem(
        Problem::new(Code::ServiceUnavailable)
            .detail_static(service_failure::AT_CAPACITY_DETAIL)
            .retry_after(service_failure::AT_CAPACITY_RETRY_AFTER),
    );
    response.extensions_mut().insert(AdmissionShed);
    response
}

fn panic_to_problem(_payload: Box<dyn Any + Send + 'static>) -> Response {
    tracing::error!("http_handler_panicked");
    pending_problem(sanitized_problem())
}

async fn not_found() -> Response {
    pending_problem(Problem::new(Code::NotFound).detail_static("no resource at this path"))
}

pub(crate) fn method_not_allowed_response() -> Response {
    pending_problem(
        Problem::new(Code::MethodNotAllowed)
            .detail_static("method is not allowed for this resource"),
    )
}

async fn method_not_allowed() -> Response {
    method_not_allowed_response()
}

#[cfg(test)]
mod tests;
