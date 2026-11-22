use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{FromRequestParts, Request};
use axum::http::header::CONNECTION;
use axum::http::request::Parts;
use axum::http::{HeaderValue, Version};
use axum::response::{IntoResponse as _, Response};
use http_body::{Body as _, Frame};
use http_body_util::BodyExt as _;
// template:begin http-idempotency:http-idempotency-collected-import
use http_body_util::Collected;
// template:end http-idempotency:http-idempotency-collected-import
use operation_context::{Deadline, OperationContext, Stopped};
use tower_http::request_id::RequestId;

use crate::problem::{Code, Problem, complete_problem, pending_problem, sanitized_problem};
use crate::request_id::REQUEST_ID_HEADER;

#[derive(Clone, Debug)]
pub struct RequestContext {
    operation: OperationContext,
    tracking: Option<Arc<RequestTracking>>,
    // template:begin request-budget:http-request-context-deadline-field
    deadline: Option<RequestDeadline>,
    // template:end request-budget:http-request-context-deadline-field
}

#[derive(Debug, Default)]
pub(super) struct RequestTracking {
    pending_reads: AtomicUsize,
    pending_at_deadline: AtomicBool,
    // template:begin object-storage:http-request-put-outcomes
    put_outcomes: std::sync::OnceLock<infra_object_storage::PutOutcomes>,
    // template:end object-storage:http-request-put-outcomes
}

impl RequestContext {
    #[must_use]
    pub const fn operation(&self) -> &OperationContext {
        &self.operation
    }

    // template:begin request-budget:http-request-context-deadline
    #[must_use]
    pub const fn deadline(&self) -> Option<RequestDeadline> {
        self.deadline
    }
    // template:end request-budget:http-request-context-deadline

    pub async fn read_body_frame(
        &self,
        body: &mut Body,
    ) -> Option<Result<Frame<Bytes>, axum::Error>> {
        track_body_read(Some(self), body.frame()).await
    }

    pub async fn read_body_bytes(&self, body: Body, limit: usize) -> Result<Bytes, axum::Error> {
        track_body_read(Some(self), to_bytes(body, limit)).await
    }

    // template:begin object-storage:http-observe-put-outcomes
    pub(super) fn observe_put(
        &self,
        body: infra_object_storage::PutBody,
    ) -> infra_object_storage::PutBody {
        match &self.tracking {
            Some(tracking) => tracking
                .put_outcomes
                .get_or_init(Default::default)
                .observe(body),
            None => body,
        }
    }

    pub(super) fn body_read_tracker(&self) -> Option<BodyReadTracker<Arc<RequestTracking>>> {
        let (tracking, deadline) = self.tracking.as_ref().zip(self.operation.deadline())?;
        Some(BodyReadTracker::new(Arc::clone(tracking), deadline))
    }
    // template:end object-storage:http-observe-put-outcomes
}

fn stopped_response(
    tracking: Option<&RequestTracking>,
    reason: Stopped,
    version: Version,
) -> Response {
    if reason == Stopped::Deadline
        && tracking.is_some_and(|tracking| {
            tracking.pending_reads.load(Ordering::Acquire) != 0
                || tracking.pending_at_deadline.load(Ordering::Relaxed)
        })
    {
        return upload_timeout_response(version);
    }
    let problem = Problem::from(reason);
    // template:begin object-storage:http-stopped-put-outcome
    if tracking
        .and_then(|tracking| tracking.put_outcomes.get())
        .is_some_and(infra_object_storage::PutOutcomes::has_unconfirmed_write)
    {
        return pending_problem(crate::object_storage::unconfirmed_write(problem));
    }
    // template:end object-storage:http-stopped-put-outcome
    pending_problem(problem)
}

pub(crate) async fn read_request_bytes(
    context: Option<&RequestContext>,
    body: Body,
) -> Result<Bytes, axum::Error> {
    track_body_read(context, body.collect())
        .await
        .map(http_body_util::Collected::to_bytes)
}

// template:begin http-idempotency:http-idempotency-collect-body
pub(crate) async fn collect_body(
    context: Option<&RequestContext>,
    body: Body,
) -> Result<Collected<Bytes>, axum::Error> {
    track_body_read(context, body.collect()).await
}
// template:end http-idempotency:http-idempotency-collect-body

pub(crate) struct BodyReadTracker<T: Deref<Target = RequestTracking>> {
    tracking: T,
    deadline: Deadline,
    counted: bool,
}

impl<T: Deref<Target = RequestTracking>> BodyReadTracker<T> {
    fn new(tracking: T, deadline: Deadline) -> Self {
        Self {
            tracking,
            deadline,
            counted: false,
        }
    }

    fn release_count(&mut self) {
        if self.counted {
            self.counted = false;
            if self.deadline.expired() {
                self.tracking
                    .pending_at_deadline
                    .store(true, Ordering::Relaxed);
            }
            self.tracking.pending_reads.fetch_sub(1, Ordering::Release);
        }
    }

    pub(crate) fn poll_with<R>(&mut self, poll: impl FnOnce() -> Poll<R>) -> Poll<R> {
        let polled = poll();
        if polled.is_pending() {
            if !self.counted && !self.deadline.expired() {
                self.tracking.pending_reads.fetch_add(1, Ordering::Relaxed);
                self.counted = true;
            }
        } else {
            self.release_count();
        }
        polled
    }
}

impl<T: Deref<Target = RequestTracking>> Drop for BodyReadTracker<T> {
    fn drop(&mut self) {
        self.release_count();
    }
}

async fn track_body_read<F: Future>(context: Option<&RequestContext>, read: F) -> F::Output {
    let Some((tracking, deadline)) = context.and_then(|context| {
        context
            .tracking
            .as_deref()
            .zip(context.operation.deadline())
    }) else {
        return read.await;
    };
    let mut tracker = BodyReadTracker::new(tracking, deadline);
    let mut read = std::pin::pin!(read);
    std::future::poll_fn(|cx| tracker.poll_with(|| read.as_mut().poll(cx))).await
}

#[derive(Clone, Debug)]
pub struct ResponseContext(OperationContext);

impl ResponseContext {
    #[must_use]
    pub const fn operation(&self) -> &OperationContext {
        &self.0
    }
}

// template:begin request-budget:http-request-deadline
const MAX_RESPONSE_RESERVE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestDeadline {
    at: tokio::time::Instant,
    response_reserve: Duration,
}

impl RequestDeadline {
    pub(crate) fn from_deadline(deadline: Deadline, budget: Duration) -> Option<Self> {
        Some(Self {
            at: deadline.instant()?,
            response_reserve: MAX_RESPONSE_RESERVE.min(budget / 2),
        })
    }

    #[must_use]
    pub const fn at(&self) -> tokio::time::Instant {
        self.at
    }

    #[must_use]
    pub fn attempt_end(&self) -> Option<tokio::time::Instant> {
        self.at
            .checked_sub(self.response_reserve)
            .filter(|end| *end > tokio::time::Instant::now())
    }
}

impl<S: Send + Sync> FromRequestParts<S> for RequestDeadline {
    type Rejection = Response;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            parts
                .extensions
                .get::<RequestContext>()
                .and_then(RequestContext::deadline)
                .ok_or_else(missing_extension::<Self>),
        )
    }
}
// template:end request-budget:http-request-deadline

impl<S: Send + Sync> FromRequestParts<S> for RequestContext {
    type Rejection = Response;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(extension::<Self>(parts))
    }
}

impl<S: Send + Sync> FromRequestParts<S> for ResponseContext {
    type Rejection = Response;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(extension::<Self>(parts))
    }
}

#[expect(
    clippy::result_large_err,
    reason = "the error is the extractor's axum rejection, a Response"
)]
pub(crate) fn extension<T: Clone + Send + Sync + 'static>(parts: &Parts) -> Result<T, Response> {
    parts
        .extensions
        .get::<T>()
        .cloned()
        .ok_or_else(missing_extension::<T>)
}

pub(crate) fn wiring_failure(extractor: &'static str) -> Problem {
    tracing::error!(extractor, "http_extractor_wiring_failed");
    sanitized_problem()
}

pub(crate) fn missing_extension_problem<T>() -> Problem {
    wiring_failure(std::any::type_name::<T>())
}

pub(crate) fn missing_extension<T>() -> Response {
    missing_extension_problem::<T>().into_response()
}

pub(crate) type CancelGuard = std::mem::DropGuard<OperationContext, fn(OperationContext)>;

pub(crate) fn attach_request_id(response: &mut Response, id: Option<RequestId>) -> Option<Code> {
    let problem_code = complete_problem(response, id.as_ref().map(RequestId::header_value));
    if let Some(id) = id {
        response
            .headers_mut()
            .insert(REQUEST_ID_HEADER, id.header_value().clone());
        response.extensions_mut().insert(id);
    }
    problem_code
}

pub(crate) async fn enforce_request_budget<P, F, Fut>(
    budget: Duration,
    admitted: Option<(tokio::time::Instant, P)>,
    mut request: Request,
    run: F,
) -> (Response, Option<CancelGuard>)
where
    F: FnOnce(Request) -> Fut,
    Fut: Future<Output = Response>,
{
    let origin = admitted
        .as_ref()
        .map_or_else(tokio::time::Instant::now, |(arrived, _)| *arrived);
    let deadline = Deadline::new(origin, budget);
    let cancel_guard: CancelGuard =
        std::mem::DropGuard::new(OperationContext::from_deadline(deadline), |context| {
            context.cancel();
        });
    let track_request = !request.body().is_end_stream();
    // template:begin object-storage:http-opening-put-outcomes
    let track_request = track_request || !request.method().is_safe();
    // template:end object-storage:http-opening-put-outcomes
    let tracking: Option<Arc<RequestTracking>> = track_request.then(Arc::default);
    let version = request.version();
    let extensions = request.extensions_mut();
    extensions.insert(ResponseContext(OperationContext::new(
        None,
        cancel_guard.cancellation().clone(),
    )));
    extensions.insert(RequestContext {
        operation: OperationContext::clone(&cancel_guard),
        tracking: tracking.clone(),
        // template:begin request-budget:http-request-deadline-field-init
        deadline: RequestDeadline::from_deadline(deadline, budget),
        // template:end request-budget:http-request-deadline-field-init
    });
    let outcome = {
        let _admitted = admitted;
        cancel_guard.run(run(request)).await
    };
    match outcome {
        Ok(response) => (response, Some(cancel_guard)),
        Err(reason) => {
            drop(cancel_guard);
            (stopped_response(tracking.as_deref(), reason, version), None)
        }
    }
}

fn upload_timeout_response(version: Version) -> Response {
    // RFC 9110 §15.5.9
    let mut response = pending_problem(
        Problem::new(Code::RequestTimeout)
            .detail_static("request body was not received within the request budget"),
    );
    if version < Version::HTTP_2 {
        response
            .headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

impl From<Stopped> for Problem {
    fn from(reason: Stopped) -> Self {
        match reason {
            Stopped::Deadline => Self::new(Code::GatewayTimeout)
                .detail_static("request budget expired before a response could be committed"),
            Stopped::Cancelled => Self::new(Code::ServiceUnavailable),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use tower::ServiceExt as _;

    #[tokio::test(start_paused = true)]
    async fn opening_expiry_does_not_cancel_response_work_but_body_drop_does() {
        let (send, receive) = tokio::sync::oneshot::channel();
        let sender = std::sync::Arc::new(std::sync::Mutex::new(Some(send)));
        let handler = move |request: RequestContext, response: ResponseContext| {
            let sender = sender.clone();
            async move {
                sender
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send((request, response))
                    .unwrap();
                Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::io::Error>>())
            }
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let routes = axum::Router::new().route("/", get(handler));
        let app = crate::harden::harden(
            routes,
            &crate::HardenOptions {
                max_body_bytes: 1024,
                request_timeout: Duration::from_secs(1),
                max_in_flight: None,
                admission_wait: Duration::ZERO,
                log_health_probes: false,
                trust_trace_context: false,
                draining: None,
            },
        );
        let response = app.oneshot(Request::new(Body::empty())).await.unwrap();
        let (opening, continuing) = receive.await.unwrap();
        assert_eq!(
            opening.operation().remaining(),
            Some(Duration::from_secs(1))
        );
        assert!(continuing.operation().deadline().is_none());
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(
            opening.operation().stopped(),
            Some(operation_context::Stopped::Deadline)
        );
        assert_eq!(continuing.operation().check(), Ok(()));
        drop(response);
        assert!(continuing.operation().cancellation().is_cancelled());
    }

    #[tokio::test]
    async fn completed_problem_keeps_response_work_alive_until_its_final_body_ends() {
        let (send, receive) = tokio::sync::oneshot::channel();
        let sender = std::sync::Arc::new(std::sync::Mutex::new(Some(send)));
        let handler = move |response: ResponseContext| {
            let sender = sender.clone();
            async move {
                sender
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(response)
                    .unwrap();
                Problem::new(Code::BadRequest)
            }
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let routes = axum::Router::new().route("/", get(handler));
        let app = crate::harden::harden(
            routes,
            &crate::HardenOptions {
                max_body_bytes: 1024,
                request_timeout: Duration::from_secs(1),
                max_in_flight: None,
                admission_wait: Duration::ZERO,
                log_health_probes: false,
                trust_trace_context: false,
                draining: None,
            },
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(REQUEST_ID_HEADER, "response-life")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let context = receive.await.unwrap();
        assert!(!context.operation().cancellation().is_cancelled());
        assert_eq!(response.headers()[REQUEST_ID_HEADER], "response-life");
        assert_eq!(
            crate::test_support::body_json(response).await["request_id"],
            "response-life"
        );
        assert!(context.operation().cancellation().is_cancelled());
    }

    #[tokio::test]
    async fn missing_hardened_context_is_a_sanitized_internal_failure() {
        let handler = |_: RequestContext| async { "unreachable" };
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = axum::Router::new().route("/", get(handler));
        let response = app.oneshot(Request::new(Body::empty())).await.unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
    }

    // template:begin request-budget:http-missing-request-deadline-test
    #[tokio::test]
    async fn missing_request_deadline_is_a_sanitized_internal_failure() {
        let handler = |_: RequestDeadline| async { "unreachable" };
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = axum::Router::new().route("/", get(handler));
        let response = app.oneshot(Request::new(Body::empty())).await.unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
    }
    // template:end request-budget:http-missing-request-deadline-test
}
