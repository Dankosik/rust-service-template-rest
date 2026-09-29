//! A streamed upload body held to its declared length.
//!
//! The request carries `Content-Length`, and hyper cuts a longer body at that
//! length without an error, so a provider that receives no checksum would
//! store a truncated object. This body fails the request instead and records
//! why, so the put reports a refused input rather than an unknown outcome.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, ready};

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_body_util::BodyExt;
use http_body_util::combinators::BoxBody;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The body yielded more or fewer bytes than declared.
#[derive(Debug, thiserror::Error)]
#[error("upload body length differs from the declared length")]
struct LengthMismatch;

pub(crate) struct ExactLength {
    inner: BoxBody<Bytes, BoxError>,
    remaining: u64,
    mismatch: Arc<AtomicBool>,
}

impl ExactLength {
    pub(crate) fn new<B, E>(len: u64, body: B) -> (Self, Arc<AtomicBool>)
    where
        B: Body<Data = Bytes, Error = E> + Send + Sync + 'static,
        E: Into<BoxError> + 'static,
    {
        let mismatch = Arc::new(AtomicBool::new(false));
        let body = Self {
            inner: BoxBody::new(body.map_err(Into::into)),
            remaining: len,
            mismatch: Arc::clone(&mismatch),
        };
        (body, mismatch)
    }

    fn refuse(&mut self) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        self.mismatch.store(true, Ordering::Release);
        Poll::Ready(Some(Err(Box::new(LengthMismatch))))
    }
}

impl Body for ExactLength {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        match ready!(Pin::new(&mut self.inner).poll_frame(context)) {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref() {
                    match self.remaining.checked_sub(data.len() as u64) {
                        Some(remaining) => self.remaining = remaining,
                        None => return self.refuse(),
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            }
            None if self.remaining != 0 => self.refuse(),
            other => Poll::Ready(other),
        }
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining)
    }
}
