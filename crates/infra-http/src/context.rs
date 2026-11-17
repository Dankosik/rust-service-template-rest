use std::mem::DropGuard;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::CONNECTION;
use axum::http::request::Parts;
use axum::http::{HeaderValue, Version};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use http_body::Body as _;
use operation_context::{Deadline, OperationContext, Stopped};
use tower_http::request_id::RequestId;

use crate::body::hold_guard_until_end;
use crate::problem::{Code, Problem, complete_problem, sanitized_internal_error};
use crate::request_id::REQUEST_ID_HEADER;

#[derive(Clone, Debug)]
pub struct RequestContext {
    operation: OperationContext,
    upload_reads: Option<Arc<AtomicUsize>>,
}

impl RequestContext {
    #[must_use]
    pub const fn operation(&self) -> &OperationContext {
        &self.operation
    }

    pub async fn read_body<F: Future>(&self, read: F) -> F::Output {
        read_body(Some(self), read).await
    }
}

pub(crate) async fn read_body<F: Future>(context: Option<&RequestContext>, read: F) -> F::Output {
    let _upload_read = context
        .and_then(|context| context.upload_reads.as_deref())
        .map(|reads| {
            reads.fetch_add(1, Ordering::Relaxed);
            DropGuard::new(reads, |reads| {
                reads.fetch_sub(1, Ordering::Relaxed);
            })
        });
    read.await
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

#[derive(Clone, Copy, Debug)]
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

pub(crate) fn missing_extension<T>() -> Response {
    tracing::error!(
        extractor = std::any::type_name::<T>(),
        "http_extractor_wiring_failed"
    );
    sanitized_internal_error()
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Arrival(pub(crate) tokio::time::Instant);

pub(crate) async fn finish_response(mut request: Request, next: Next) -> Response {
    let id = request.extensions().get::<RequestId>().cloned();
    let context = OperationContext::unbounded();
    let guard = context.cancellation().clone().drop_guard();
    request.extensions_mut().insert(ResponseContext(context));
    let mut response = next.run(request).await;
    if let Some(id) = id {
        if let Ok(text) = id.header_value().to_str() {
            complete_problem(&mut response, text);
        }
        response
            .headers_mut()
            .insert(REQUEST_ID_HEADER, id.header_value().clone());
        response.extensions_mut().insert(id);
    }
    response.map(|body| hold_guard_until_end(body, guard))
}

pub(crate) async fn opening(
    State(budget): State<Duration>,
    response_context: ResponseContext,
    mut request: Request,
    next: Next,
) -> Response {
    let origin = request
        .extensions()
        .get::<Arrival>()
        .map_or_else(tokio::time::Instant::now, |arrival| arrival.0);
    let deadline = Deadline::new(origin, budget);
    let context = RequestContext {
        operation: OperationContext::new(
            Some(deadline),
            response_context.operation().cancellation().clone(),
        ),
        upload_reads: (!request.body().is_end_stream()).then(|| Arc::new(AtomicUsize::new(0))),
    };
    let version = request.version();
    let upload_version = || {
        context
            .upload_reads
            .as_ref()
            .filter(|reads| reads.load(Ordering::Relaxed) != 0)
            .map(|_| version)
    };
    request.extensions_mut().insert(context.operation.clone());
    request.extensions_mut().insert(context.clone());
    // template:begin request-budget:http-request-deadline-projection
    if let Some(deadline) = RequestDeadline::from_deadline(deadline, budget) {
        request.extensions_mut().insert(deadline);
    }
    // template:end request-budget:http-request-deadline-projection
    let response = tokio::select! {
        biased;
        stopped = async {
            let reason = context.operation.wait_stopped().await;
            response_context.operation().cancel();
            stopped_response(reason, upload_version())
        } => return stopped,
        response = next.run(request) => response,
    };
    if let Some(reason) = context.operation.stopped() {
        response_context.operation().cancel();
        return stopped_response(reason, upload_version());
    }
    response
}

fn stopped_response(reason: Stopped, upload: Option<Version>) -> Response {
    match (reason, upload) {
        (Stopped::Deadline, Some(version)) => upload_timeout_response(version),
        (Stopped::Deadline, None) => budget_expired_response(),
        (Stopped::Cancelled, _) => cancelled_response(),
    }
}

fn upload_timeout_response(version: Version) -> Response {
    // RFC 9110 §15.5.9
    let mut response = Problem::new(Code::RequestTimeout)
        .detail("request body was not received within the request budget")
        .into_response();
    if version < Version::HTTP_2 {
        response
            .headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

pub(crate) fn budget_expired_response() -> Response {
    Problem::new(Code::GatewayTimeout)
        .detail("request budget expired before a response could be committed")
        .into_response()
}

pub(crate) fn cancelled_response() -> Response {
    Problem::new(Code::ServiceUnavailable).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, Bytes};
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
        let app = axum::Router::new()
            .route("/", get(handler))
            .layer(axum::middleware::from_fn_with_state(
                Duration::from_secs(1),
                opening,
            ))
            .layer(axum::middleware::from_fn(finish_response));
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
}
