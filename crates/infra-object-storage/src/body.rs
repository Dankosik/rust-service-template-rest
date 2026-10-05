//! Upload bodies: in-memory bytes, or a stream held to its declared length.
//!
//! The request carries `Content-Length`, and hyper stops polling a body once
//! that many bytes are through, without an error. A longer body would be
//! stored truncated on a provider that receives no checksum. A streamed body
//! holds back the frame that completes the declared length until the inner
//! body confirms its end, fails the request otherwise, and records why, so
//! the put reports a refused input rather than an unknown outcome.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use aws_sdk_s3::primitives::ByteStream;
use bytes::{Buf, Bytes};
use http_body::{Body, Frame, SizeHint};
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;
use sync_wrapper::SyncWrapper;
use tokio::time::{Instant, timeout_at};

use crate::ObjectStorageError;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// An upload body with its exact length.
#[derive(Debug)]
pub struct PutBody {
    pub(crate) len: u64,
    pub(crate) stream: ByteStream,
    pub(crate) source: UploadSource,
}

#[derive(Debug)]
pub(crate) enum UploadSource {
    InMemory,
    Streamed {
        /// Set when the body did not match its declared length.
        mismatch: Arc<AtomicBool>,
    },
}

impl PutBody {
    /// A streamed body that must yield exactly `len` bytes. A body that
    /// yields more or fewer fails the put with
    /// [`ObjectStorageError::Rejected`] instead of storing a truncated object.
    pub fn stream<B, E>(len: u64, body: B) -> Self
    where
        B: Body<Data = Bytes, Error = E> + Send + 'static,
        E: Into<BoxError> + 'static,
    {
        let (body, mismatch) = ExactLength::new(len, body);
        Self {
            len,
            stream: ByteStream::from_body_1_x(body),
            source: UploadSource::Streamed { mismatch },
        }
    }

    /// Declared length in bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the body is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// hyper never polls a body declared empty, so validate an empty stream
    /// before sending and normalize its EOF to an in-memory body.
    pub(crate) async fn prepare_empty_stream(
        &mut self,
        deadline: Instant,
    ) -> Result<(), (ObjectStorageError, &'static str)> {
        let UploadSource::Streamed { mismatch } = &self.source else {
            return Ok(());
        };
        if self.len != 0 {
            return Ok(());
        }
        let first = timeout_at(deadline, self.stream.next()).await;
        let length_mismatch = mismatch.load(Ordering::Acquire);
        match first {
            Err(_elapsed) => Err((ObjectStorageError::Unavailable, "timeout")),
            Ok(Some(_)) if length_mismatch => Err((ObjectStorageError::Rejected, "body_length")),
            Ok(Some(_)) => Err((ObjectStorageError::Rejected, "body")),
            Ok(None) => {
                *self = Self::from(Bytes::new());
                Ok(())
            }
        }
    }
}

/// In-memory bytes. The SDK can sign a checksum of them as a header.
impl From<Bytes> for PutBody {
    fn from(bytes: Bytes) -> Self {
        Self {
            len: bytes.len() as u64,
            stream: ByteStream::from(bytes),
            source: UploadSource::InMemory,
        }
    }
}

impl From<Vec<u8>> for PutBody {
    fn from(bytes: Vec<u8>) -> Self {
        Bytes::from(bytes).into()
    }
}

/// The body yielded more or fewer bytes than declared.
#[derive(Debug, thiserror::Error)]
#[error("upload body length differs from the declared length")]
struct LengthMismatch;

struct ExactLength {
    /// The SDK requires a `Sync` body, and a request body (axum's `Body`)
    /// is not one. The wrapper is sound here: a body is polled only through
    /// `&mut`.
    inner: SyncWrapper<UnsyncBoxBody<Bytes, BoxError>>,
    remaining: u64,
    /// The data frame that completed the declared length, held until the
    /// inner body shows it has nothing more.
    held: Option<Frame<Bytes>>,
    /// Trailers seen while confirming the end, returned after `held`.
    trailers: Option<Frame<Bytes>>,
    /// The inner body has ended.
    ended: bool,
    mismatch: Arc<AtomicBool>,
}

impl ExactLength {
    fn new<B, E>(len: u64, body: B) -> (Self, Arc<AtomicBool>)
    where
        B: Body<Data = Bytes, Error = E> + Send + 'static,
        E: Into<BoxError> + 'static,
    {
        let mismatch = Arc::new(AtomicBool::new(false));
        let body = Self {
            inner: SyncWrapper::new(UnsyncBoxBody::new(body.map_err(Into::into))),
            remaining: len,
            held: None,
            trailers: None,
            ended: false,
            mismatch: Arc::clone(&mismatch),
        };
        (body, mismatch)
    }

    fn refuse(&mut self) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        self.mismatch.store(true, Ordering::Release);
        self.ended = true;
        self.held = None;
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
        let mut polls_remaining = 64;
        loop {
            if self.ended {
                if let Some(frame) = self.held.take() {
                    return Poll::Ready(Some(Ok(frame)));
                }
                return Poll::Ready(self.trailers.take().map(Ok));
            }
            if polls_remaining == 0 {
                // Ready empty frames must not monopolize the caller, even
                // while the last data frame waits for end confirmation.
                context.waker().wake_by_ref();
                return Poll::Pending;
            }
            polls_remaining -= 1;
            let polled = match Pin::new(self.inner.get_mut()).poll_frame(context) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(polled) => polled,
            };
            match polled {
                Some(Ok(frame)) => {
                    let Some(data) = frame.data_ref() else {
                        // Trailers end the data; the held frame goes first.
                        if self.remaining != 0 {
                            return self.refuse();
                        }
                        self.ended = true;
                        if self.held.is_some() {
                            self.trailers = Some(frame);
                            continue;
                        }
                        return Poll::Ready(Some(Ok(frame)));
                    };
                    let len = data.remaining() as u64;
                    if len == 0 {
                        continue;
                    }
                    if self.held.is_some() {
                        return self.refuse();
                    }
                    let Some(remaining) = self.remaining.checked_sub(len) else {
                        return self.refuse();
                    };
                    self.remaining = remaining;
                    if remaining == 0 {
                        self.held = Some(frame);
                        continue;
                    }
                    return Poll::Ready(Some(Ok(frame)));
                }
                Some(Err(error)) => return Poll::Ready(Some(Err(error))),
                None if self.remaining != 0 => return self.refuse(),
                None => self.ended = true,
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.ended && self.held.is_none() && self.trailers.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        let held = self
            .held
            .as_ref()
            .and_then(Frame::data_ref)
            .map_or(0, |data| data.remaining() as u64);
        SizeHint::with_exact(self.remaining + held)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Wake, Waker};

    use super::*;

    /// A body that yields its frames one per poll, like a network stream.
    struct Frames(VecDeque<Frame<Bytes>>);

    impl Body for Frames {
        type Data = Bytes;
        type Error = std::convert::Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            Poll::Ready(self.0.pop_front().map(Ok))
        }
    }

    fn frames(chunks: &[&'static [u8]]) -> Frames {
        Frames(
            chunks
                .iter()
                .map(|chunk| Frame::data(Bytes::from_static(chunk)))
                .collect(),
        )
    }

    /// Poll the way hyper does: stop as soon as `len` bytes are through.
    fn send(len: u64, body: Frames) -> (Result<u64, ()>, bool) {
        let (mut body, mismatch) = ExactLength::new(len, body);
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut sent = 0;
        while sent < len {
            match Pin::new(&mut body).poll_frame(&mut context) {
                Poll::Ready(Some(Ok(frame))) => {
                    sent += frame.data_ref().map_or(0, |data| data.len() as u64);
                }
                Poll::Ready(Some(Err(_))) => return (Err(()), mismatch.load(Ordering::Acquire)),
                Poll::Ready(None) | Poll::Pending => break,
            }
        }
        (Ok(sent), mismatch.load(Ordering::Acquire))
    }

    #[test]
    fn an_exact_body_passes_in_any_framing() {
        assert_eq!(send(8, frames(&[b"abcdefgh"])), (Ok(8), false));
        assert_eq!(send(8, frames(&[b"abcd", b"", b"efgh"])), (Ok(8), false));
        assert_eq!(send(0, frames(&[])), (Ok(0), false));
    }

    #[test]
    fn a_frame_past_the_declared_length_fails_the_body() {
        // The first frame completes the length; hyper would stop there.
        assert_eq!(send(4, frames(&[b"abcd", b"efgh"])), (Err(()), true));
        assert_eq!(send(4, frames(&[b"abcdefgh"])), (Err(()), true));
    }

    #[test]
    fn a_short_body_fails_the_body() {
        assert_eq!(send(8, frames(&[b"abcd"])), (Err(()), true));
    }

    #[derive(Default)]
    struct WakeFlag(AtomicBool);

    impl Wake for WakeFlag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    struct PolledFrames {
        steps: VecDeque<Poll<Option<Result<Frame<Bytes>, std::io::Error>>>>,
        empty_tail: bool,
        polls: Arc<AtomicUsize>,
    }

    impl Body for PolledFrames {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            // Fail a broken wrapper deterministically instead of hanging on
            // the infinite empty source. The caller resets this each poll.
            assert!(self.polls.fetch_add(1, Ordering::Relaxed) < 64);
            self.steps.pop_front().unwrap_or_else(|| {
                Poll::Ready(self.empty_tail.then(|| Ok(Frame::data(Bytes::new()))))
            })
        }
    }

    #[test]
    fn endless_empty_frames_yield_and_wake_before_and_after_the_last_data() {
        for (len, first) in [(0, None), (4, None), (4, Some(b"abcd".as_slice()))] {
            let polls = Arc::new(AtomicUsize::new(0));
            let source = PolledFrames {
                steps: first
                    .map(|data| Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(data))))))
                    .into_iter()
                    .collect(),
                empty_tail: true,
                polls: Arc::clone(&polls),
            };
            let (mut body, mismatch) = ExactLength::new(len, source);
            let wake = Arc::new(WakeFlag::default());
            let waker = Waker::from(Arc::clone(&wake));
            let mut context = Context::from_waker(&waker);
            for _ in 0..3 {
                polls.store(0, Ordering::Relaxed);
                assert!(Pin::new(&mut body).poll_frame(&mut context).is_pending());
                assert_eq!(polls.load(Ordering::Relaxed), 64);
                assert!(wake.0.swap(false, Ordering::Relaxed));
                assert_eq!(body.size_hint().exact(), Some(len));
                assert!(!body.is_end_stream());
                assert!(!mismatch.load(Ordering::Acquire));
            }
        }
    }

    #[test]
    fn finite_stream_results_survive_budget_yields_and_source_pending() {
        for (len, payload, ending) in [
            (0, b"".as_slice(), "eof"),
            (4, b"abcd".as_slice(), "eof"),
            (5, b"abcd".as_slice(), "eof"),
            (4, b"abcd".as_slice(), "overflow"),
            (4, b"abcd".as_slice(), "trailers"),
            (4, b"abcd".as_slice(), "error"),
        ] {
            let mut steps = VecDeque::new();
            for _ in 0..65 {
                steps.push_back(Poll::Ready(Some(Ok(Frame::data(Bytes::new())))));
            }
            steps.push_back(Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(
                payload,
            ))))));
            for _ in 0..130 {
                steps.push_back(Poll::Ready(Some(Ok(Frame::data(Bytes::new())))));
            }
            steps.push_back(Poll::Pending);
            let mut trailers = axum::http::HeaderMap::new();
            trailers.insert("x-checksum", axum::http::HeaderValue::from_static("1"));
            match ending {
                "overflow" => {
                    steps.push_back(Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"!"))))))
                }
                "trailers" => {
                    steps.push_back(Poll::Ready(Some(Ok(Frame::trailers(trailers.clone())))))
                }
                "error" => steps.push_back(Poll::Ready(Some(Err(std::io::Error::other(
                    "source failed",
                ))))),
                _ => {}
            }
            let polls = Arc::new(AtomicUsize::new(0));
            let source = PolledFrames {
                steps,
                empty_tail: false,
                polls: Arc::clone(&polls),
            };
            let (mut body, mismatch) = ExactLength::new(len, source);
            let wake = Arc::new(WakeFlag::default());
            let waker = Waker::from(Arc::clone(&wake));
            let mut context = Context::from_waker(&waker);
            let mut received = Vec::new();
            let mut received_trailers = None;
            let mut source_pending = false;
            let mut completed = false;
            for _ in 0..10 {
                polls.store(0, Ordering::Relaxed);
                match Pin::new(&mut body).poll_frame(&mut context) {
                    Poll::Pending => {
                        if polls.load(Ordering::Relaxed) == 64 {
                            assert!(wake.0.swap(false, Ordering::Relaxed));
                        } else {
                            assert!(!wake.0.swap(false, Ordering::Relaxed));
                            source_pending = true;
                        }
                        assert_eq!(body.size_hint().exact(), Some(len - received.len() as u64));
                        assert!(!body.is_end_stream());
                        assert!(!mismatch.load(Ordering::Acquire));
                    }
                    Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                        Ok(data) => {
                            assert!(received_trailers.is_none());
                            received.extend_from_slice(&data);
                        }
                        Err(frame) => received_trailers = Some(frame.into_trailers().unwrap()),
                    },
                    Poll::Ready(Some(Err(error))) => {
                        assert_eq!(received, if len == 5 { payload } else { b"" });
                        if ending == "error" {
                            let source_error = error.downcast_ref::<std::io::Error>().unwrap();
                            assert_eq!(source_error.to_string(), "source failed");
                            assert!(!mismatch.load(Ordering::Acquire));
                        } else {
                            assert!(len == 5 || ending == "overflow");
                            assert!(error.is::<LengthMismatch>());
                            assert!(mismatch.load(Ordering::Acquire));
                            assert!(body.is_end_stream());
                        }
                        completed = true;
                        break;
                    }
                    Poll::Ready(None) => {
                        assert!(ending == "eof" || ending == "trailers");
                        assert_eq!(received, payload);
                        assert_eq!(received.len() as u64, len);
                        assert_eq!(
                            received_trailers,
                            (ending == "trailers").then_some(trailers)
                        );
                        assert_eq!(body.size_hint().exact(), Some(0));
                        assert!(body.is_end_stream());
                        assert!(!mismatch.load(Ordering::Acquire));
                        completed = true;
                        break;
                    }
                }
            }
            assert!(completed, "finite stream must finish after self-wakeup");
            assert!(source_pending, "source Pending must reach the caller");
        }
    }

    #[test]
    fn trailers_follow_the_held_frame() {
        let mut trailers = axum::http::HeaderMap::new();
        trailers.insert("x-checksum", axum::http::HeaderValue::from_static("1"));
        let body = Frames(VecDeque::from([
            Frame::data(Bytes::from_static(b"abcd")),
            Frame::trailers(trailers),
        ]));
        let (mut body, mismatch) = ExactLength::new(4, body);
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        let first = Pin::new(&mut body).poll_frame(&mut context);
        assert!(matches!(first, Poll::Ready(Some(Ok(ref frame))) if frame.is_data()));
        let second = Pin::new(&mut body).poll_frame(&mut context);
        assert!(matches!(second, Poll::Ready(Some(Ok(ref frame))) if frame.is_trailers()));
        assert!(matches!(
            Pin::new(&mut body).poll_frame(&mut context),
            Poll::Ready(None)
        ));
        assert!(body.is_end_stream());
        assert!(!mismatch.load(Ordering::Acquire));
    }
}
