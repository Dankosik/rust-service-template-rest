use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use http_body::{Body as HttpBody, Frame, SizeHint};

pub(crate) fn hold_guard_until_end<G: Send + Unpin + 'static>(body: Body, guard: G) -> Body {
    on_body_end(body, None, move |_| drop(guard))
}

#[derive(Clone, Copy)]
pub(crate) enum BodyEnd {
    Complete,
    Error,
}

pub(crate) fn on_body_end(
    body: Body,
    remaining: Option<u64>,
    finish: impl FnOnce(BodyEnd) + Send + Unpin + 'static,
) -> Body {
    if body.is_end_stream() || remaining == Some(0) {
        finish(if remaining.is_none_or(|length| length == 0) {
            BodyEnd::Complete
        } else {
            BodyEnd::Error
        });
        return body;
    }
    Body::new(GuardedBody {
        body,
        remaining,
        finish: Some(finish),
    })
}

struct GuardedBody<F> {
    body: Body,
    remaining: Option<u64>,
    finish: Option<F>,
}

impl<F: FnOnce(BodyEnd) + Unpin> HttpBody for GuardedBody<F> {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let frame = Pin::new(&mut this.body).poll_frame(cx);
        if let Poll::Ready(ref frame) = frame
            && this.finish.is_some()
            && let Some(end) = this.end_after(frame.as_ref())
            && let Some(finish) = this.finish.take()
        {
            finish(end);
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

impl<F> GuardedBody<F> {
    fn end_after(&mut self, frame: Option<&Result<Frame<Bytes>, axum::Error>>) -> Option<BodyEnd> {
        let end_stream = match frame {
            None => true,
            Some(Err(_)) => return Some(BodyEnd::Error),
            Some(Ok(frame)) => {
                if let (Some(remaining), Some(data)) = (&mut self.remaining, frame.data_ref()) {
                    let Some(next) = remaining.checked_sub(data.len() as u64) else {
                        return Some(BodyEnd::Error);
                    };
                    *remaining = next;
                }
                frame.is_trailers() || self.body.is_end_stream()
            }
        };
        match (end_stream, self.remaining) {
            (_, Some(0)) | (true, None) => Some(BodyEnd::Complete),
            (true, Some(_)) => Some(BodyEnd::Error),
            (false, _) => None,
        }
    }
}
