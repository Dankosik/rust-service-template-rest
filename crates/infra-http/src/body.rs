use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use http_body::{Body as HttpBody, Frame, SizeHint};

pub(crate) fn hold_guard_until_end<G: Send + Unpin + 'static>(body: Body, guard: G) -> Body {
    if body.is_end_stream() {
        drop(guard);
        return body;
    }
    Body::new(GuardedBody {
        body,
        guard: Some(guard),
    })
}

struct GuardedBody<G> {
    body: Body,
    guard: Option<G>,
}

impl<G: Unpin> HttpBody for GuardedBody<G> {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let frame = Pin::new(&mut this.body).poll_frame(cx);
        if matches!(frame, Poll::Ready(None | Some(Err(_)))) || this.body.is_end_stream() {
            this.guard.take();
        }
        frame
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}
