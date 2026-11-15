//! Request opening and explicitly continuing response work have distinct lifetimes.

use std::future::Future;
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

use crate::body::hold_guard_until_end;
use crate::problem::{Code, Problem, sanitized_internal_error};

/// Fixed request-opening budget installed by the hardened chain.
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

    /// Track an awaited read of the incoming request body for timeout classification.
    ///
    /// Pass only the body-read future. If it is dropped while pending, a later
    /// opening deadline is classified independently of that read.
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

/// Cancellation lineage for work deliberately continuing after response headers.
/// No generic HTTP response deadline is imposed; dependencies retain their own caps.
///
/// The hardened chain hands the lineage's cancellation guard to the response
/// body, so it is cancelled when the body ends, fails, or is dropped. The
/// [`RequestContext`] shares this cancellation token, so cancelling the
/// response lineage also stops opening work.
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

/// The conservative deadline shared with request-scoped dependencies.
///
/// The opening middleware projects its exact cutoff here. Consumers may observe the instant but
/// cannot introduce a second request budget.
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

    /// The existing absolute request deadline; observing it never extends the budget.
    #[must_use]
    pub const fn at(&self) -> tokio::time::Instant {
        self.at
    }

    /// The cutoff for a PostgreSQL attempt. The reserve leaves time for the
    /// in-memory response without consuming the entire budget of a short request.
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

/// A clone of the request extension `T` the hardened chain installed, or the
/// sanitized 500 for a handler mounted outside that chain.
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

/// When the request reached admission. The opening budget counts from here, so
/// the time a request waited for an in-flight permit is spent from it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Arrival(pub(crate) tokio::time::Instant);

pub(crate) async fn opening(
    State(budget): State<Duration>,
    mut request: Request,
    next: Next,
) -> Response {
    let origin = request
        .extensions()
        .get::<Arrival>()
        .map_or_else(tokio::time::Instant::now, |arrival| arrival.0);
    let deadline = Deadline::new(origin, budget);
    let response_context = OperationContext::unbounded();
    let context = RequestContext {
        operation: OperationContext::new(Some(deadline), response_context.cancellation().clone()),
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
    let guard = response_context.cancellation().clone().drop_guard();
    // The raw context is the extension outbound clients read for parent
    // deadline and cancellation; the wrapper below is the handler-facing
    // extractor.
    request.extensions_mut().insert(context.operation.clone());
    request.extensions_mut().insert(context.clone());
    request
        .extensions_mut()
        .insert(ResponseContext(response_context));
    // template:begin request-budget:http-request-deadline-projection
    if let Some(deadline) = RequestDeadline::from_deadline(deadline, budget) {
        request.extensions_mut().insert(deadline);
    }
    // template:end request-budget:http-request-deadline-projection
    let response = tokio::select! {
        biased;
        // Check the live reads before `select!` drops the handler's future.
        stopped = async {
            let reason = context.operation.wait_stopped().await;
            stopped_response(reason, upload_version())
        } => return stopped,
        response = next.run(request) => response,
    };
    if let Some(reason) = context.operation.stopped() {
        return stopped_response(reason, upload_version());
    }
    response.map(|body| hold_guard_until_end(body, guard))
}

fn stopped_response(reason: Stopped, upload: Option<Version>) -> Response {
    match (reason, upload) {
        (Stopped::Deadline, Some(version)) => upload_timeout_response(version),
        (Stopped::Deadline, None) => budget_expired_response(),
        (Stopped::Cancelled, _) => Problem::new(Code::ServiceUnavailable).into_response(),
    }
}

/// The handler was waiting for body bytes the caller had not sent, so the
/// caller was slow: 408, not the server's own 504. RFC 9110 §15.5.9 asks for
/// `Connection: close`, since an HTTP/1.1 caller may still be sending; HTTP/2
/// forbids the field and resets the stream instead.
fn upload_timeout_response(version: Version) -> Response {
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use axum::routing::get;
    use http_body_util::BodyExt as _;
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
        let app = axum::Router::new().route("/", get(handler)).layer(
            axum::middleware::from_fn_with_state(Duration::from_secs(1), opening),
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
    async fn confirmed_response_eof_cancels_continuing_work() {
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
                "complete"
            }
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = axum::Router::new().route("/", get(handler)).layer(
            axum::middleware::from_fn_with_state(Duration::from_secs(1), opening),
        );
        let response = app.oneshot(Request::new(Body::empty())).await.unwrap();
        let context = receive.await.unwrap();
        assert!(!context.operation().cancellation().is_cancelled());
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "complete"
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
