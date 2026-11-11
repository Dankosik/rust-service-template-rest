//! Request opening and explicitly continuing response work have distinct lifetimes.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::CONNECTION;
use axum::http::request::Parts;
use axum::http::{HeaderValue, Version};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use http_body::{Body as HttpBody, Frame, SizeHint};
use operation_context::{Deadline, OperationContext, Stopped};
use tokio_util::sync::DropGuard;

use crate::problem::{Code, Problem, sanitized_internal_error};

/// Fixed request-opening budget installed by the hardened chain.
#[derive(Clone, Debug)]
pub struct RequestContext(OperationContext);

impl RequestContext {
    #[must_use]
    pub const fn operation(&self) -> &OperationContext {
        &self.0
    }
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
        .ok_or_else(sanitized_internal_error)
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
    let request_operation =
        OperationContext::new(Some(deadline), response_context.cancellation().clone());
    let guard = response_context.cancellation().clone().drop_guard();
    // The raw context is the extension outbound clients read for parent
    // deadline and cancellation; the wrapper below is the handler-facing
    // extractor.
    request.extensions_mut().insert(request_operation.clone());
    request
        .extensions_mut()
        .insert(RequestContext(request_operation.clone()));
    request
        .extensions_mut()
        .insert(ResponseContext(response_context));
    // template:begin request-budget:http-request-deadline-projection
    if let Some(deadline) = crate::RequestDeadline::from_deadline(deadline) {
        request.extensions_mut().insert(deadline);
    }
    // template:end request-budget:http-request-deadline-projection
    let upload = Upload::track(&mut request);
    let response = tokio::select! {
        biased;
        // The upload is read when the budget expires, before `select!` drops
        // the handler and with it the body.
        stop = async {
            let reason = request_operation.wait_stopped().await;
            (reason, upload.awaited())
        } => return stopped_response(stop.0, stop.1),
        response = next.run(request) => response,
    };
    if let Some(reason) = request_operation.stopped() {
        return stopped_response(reason, upload.awaited());
    }
    response.map(|body| {
        if body.is_end_stream() {
            drop(guard);
            body
        } else {
            Body::new(CancelBody {
                body,
                guard: Some(guard),
            })
        }
    })
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

/// Whether the handler is waiting on the caller for request body bytes: the
/// body's last poll was pending and the body is still held. A body that
/// arrived empty is not tracked.
struct Upload {
    awaiting: Option<Arc<AtomicBool>>,
    version: Version,
}

impl Upload {
    fn track(request: &mut Request) -> Self {
        let version = request.version();
        if request.body().is_end_stream() {
            return Self {
                awaiting: None,
                version,
            };
        }
        let awaiting = Arc::new(AtomicBool::new(false));
        let body = std::mem::take(request.body_mut());
        *request.body_mut() = Body::new(UploadBody {
            body,
            awaiting: Arc::clone(&awaiting),
        });
        Self {
            awaiting: Some(awaiting),
            version,
        }
    }

    /// The request version when the handler is blocked on the caller's body.
    fn awaited(&self) -> Option<Version> {
        self.awaiting
            .as_ref()
            .filter(|awaiting| awaiting.load(Ordering::Relaxed))
            .map(|_| self.version)
    }
}

struct UploadBody {
    body: Body,
    awaiting: Arc<AtomicBool>,
}

impl HttpBody for UploadBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.body).poll_frame(cx);
        this.awaiting.store(result.is_pending(), Ordering::Relaxed);
        result
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

impl Drop for UploadBody {
    fn drop(&mut self) {
        self.awaiting.store(false, Ordering::Relaxed);
    }
}

struct CancelBody {
    body: Body,
    guard: Option<DropGuard>,
}

impl HttpBody for CancelBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.body).poll_frame(cx);
        if matches!(result, Poll::Ready(None | Some(Err(_)))) || this.body.is_end_stream() {
            this.guard.take();
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
