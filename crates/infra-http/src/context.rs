//! Request opening and explicitly continuing response work have distinct lifetimes.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
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
        std::future::ready({
            parts
                .extensions
                .get::<Self>()
                .cloned()
                .ok_or_else(sanitized_internal_error)
        })
    }
}

impl<S: Send + Sync> FromRequestParts<S> for ResponseContext {
    type Rejection = Response;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready({
            parts
                .extensions
                .get::<Self>()
                .cloned()
                .ok_or_else(sanitized_internal_error)
        })
    }
}

pub(crate) async fn opening(
    State(budget): State<Duration>,
    mut request: Request,
    next: Next,
) -> Response {
    let deadline = Deadline::new(tokio::time::Instant::now(), budget);
    let response_context = OperationContext::unbounded();
    let opening = OperationContext::new(Some(deadline), response_context.cancellation().clone());
    let guard = response_context.cancellation().clone().drop_guard();
    request.extensions_mut().insert(opening.clone());
    request
        .extensions_mut()
        .insert(RequestContext(opening.clone()));
    request
        .extensions_mut()
        .insert(ResponseContext(response_context));
    // template:begin request-budget:http-request-deadline-projection
    if let Some(deadline) = crate::RequestDeadline::from_deadline(deadline) {
        request.extensions_mut().insert(deadline);
    }
    // template:end request-budget:http-request-deadline-projection
    let response = tokio::select! {
        biased;
        reason = opening.wait_stopped() => return stopped_response(reason),
        response = next.run(request) => response,
    };
    if let Some(reason) = opening.stopped() {
        return stopped_response(reason);
    }
    response.map(|body| {
        let guard = if body.is_end_stream() {
            drop(guard);
            None
        } else {
            Some(guard)
        };
        Body::new(CancelBody { body, guard })
    })
}

fn stopped_response(reason: Stopped) -> Response {
    match reason {
        Stopped::Deadline => timeout(),
        Stopped::Cancelled => Problem::new(Code::ServiceUnavailable).into_response(),
    }
}

pub(crate) fn timeout() -> Response {
    Problem::new(Code::GatewayTimeout)
        .detail("request budget expired before a response could be committed")
        .into_response()
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
#[allow(
    clippy::unwrap_used,
    reason = "mounted middleware fixtures assert transport context and body custody"
)]
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
        #[allow(
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
        #[allow(
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
        #[allow(
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
