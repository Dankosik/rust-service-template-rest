//! The hardened middleware chain and fallback policy.
//!
//! Order is the contract, outermost first:
//!
//! request-id sanitize → set → propagate → nosniff → observation (OpenTelemetry
//! server span, HTTP metrics, problem completion, access log) → traceparent
//! response header → in-flight admission (waits for a permit, then sheds; probes
//! exempt) → opening context and deadline → panic recovery → body limit
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

use axum::Router;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::HeaderValue;
use axum::http::header::X_CONTENT_TYPE_OPTIONS;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum_tracing_opentelemetry::middleware::OtelInResponseLayer;
use tokio::sync::Semaphore;
use tower::ServiceBuilder;
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
use crate::router::is_health_probe_route;

// template:begin request-budget:http-request-deadline
/// The conservative deadline shared with request-scoped dependencies.
///
/// The opening middleware projects its exact cutoff here. Consumers may observe the instant but
/// cannot introduce a second request budget.
#[derive(Clone, Copy, Debug)]
pub struct RequestDeadline(pub(crate) tokio::time::Instant);

impl RequestDeadline {
    pub(crate) fn from_deadline(deadline: operation_context::Deadline) -> Option<Self> {
        deadline.instant().map(Self)
    }

    /// The existing absolute request deadline; observing it never extends the budget.
    #[must_use]
    pub const fn at(&self) -> tokio::time::Instant {
        self.0
    }

    /// When a PostgreSQL attempt inside this budget must end: 100 ms before
    /// the deadline, leaving time for the bounded in-memory response after
    /// it. `None` once that point has passed.
    #[must_use]
    pub fn attempt_end(&self) -> Option<tokio::time::Instant> {
        self.0
            .checked_sub(Duration::from_millis(100))
            .filter(|end| *end > tokio::time::Instant::now())
    }
}
// template:end request-budget:http-request-deadline

/// Requests shed because no in-flight permit freed within the request timeout.
pub(crate) const SHED_REQUESTS_METRIC: &str = "http_server_shed_requests_total";

/// The router-wide in-flight bound and how long a request waits for a permit.
struct Admission {
    permits: Semaphore,
    /// `http.request_timeout`: the request has no deadline until it is
    /// admitted, and the budget it then gets is what this wait leaves of it.
    patience: Duration,
}

#[derive(Clone, Debug)]
pub struct HardenOptions {
    /// Request body ceiling; overflow answers 413. Applied both to tower-http's
    /// `RequestBodyLimitLayer` and to axum's extractor `DefaultBodyLimit`;
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
    metrics::describe_counter!(
        SHED_REQUESTS_METRIC,
        metrics::Unit::Count,
        "Requests rejected without running a handler because no in-flight permit freed within the request timeout."
    );
    observe::describe();
    // One semaphore for the whole router: `Router::layer` applies the chain
    // to every route, so a per-layer limit would be a per-route limit.
    let in_flight = options.max_in_flight.map(|limit| {
        // Published at zero, so a scrape tells "never shed" from "not exported".
        metrics::counter!(SHED_REQUESTS_METRIC).increment(0);
        middleware::from_fn_with_state(
            Arc::new(Admission {
                permits: Semaphore::new(limit.get() as usize),
                patience: options.request_timeout,
            }),
            admit,
        )
    });

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
        // Outside opening enforcement, so every Problem is completed before it is
        // counted and logged.
        .layer(middleware::from_fn_with_state(
            AccessLogOptions {
                log_health_probes: options.log_health_probes,
            },
            observe::observe,
        ))
        .layer(OtelInResponseLayer)
        .layer(option_layer(in_flight))
        .layer(middleware::from_fn_with_state(
            options.request_timeout,
            crate::context::opening,
        ))
        .layer(CatchPanicLayer::custom(panic_to_problem))
        .layer(RequestBodyLimitLayer::new(options.max_body_bytes))
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
        metrics::histogram!(observe::ADMISSION_WAIT_METRIC).record(arrived.elapsed().as_secs_f64());
        match waited {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) | Err(_) => return shed_response(),
        }
    };
    request
        .extensions_mut()
        .insert(crate::context::Arrival(arrived));
    next.run(request).await
}

fn shed_response() -> Response {
    metrics::counter!(SHED_REQUESTS_METRIC).increment(1);
    Problem::new(Code::ServiceUnavailable)
        .detail(AT_CAPACITY_DETAIL)
        .retry_after(service_failure::AT_CAPACITY_RETRY_AFTER)
        .into_response()
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
