use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};

use aws_sdk_s3::primitives::ByteStream;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use sync_wrapper::SyncWrapper;

use crate::byte_progress::{BodyEndedEarly, ByteProgress};
use crate::error::UploadFailure;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const SOURCE_POLLS_PER_YIELD: usize = 64;

#[derive(derive_more::Debug)]
#[debug("PutBody {{ len: {}, .. }}", payload.len())]
pub struct PutBody {
    pub(crate) payload: Payload,
    pub(crate) outcomes: Option<PutOutcomes>,
}

#[derive(Clone, Debug, Default)]
pub struct PutOutcomes(Arc<AtomicUsize>);

impl PutOutcomes {
    #[must_use]
    pub fn observe(&self, mut body: PutBody) -> PutBody {
        body.outcomes = Some(self.clone());
        body
    }

    #[must_use]
    pub fn has_unconfirmed_write(&self) -> bool {
        self.0.load(Ordering::Acquire) != 0
    }

    #[must_use]
    pub(super) fn begin_dispatch(&self) -> DispatchedPut<'_> {
        self.0.fetch_add(1, Ordering::Release);
        DispatchedPut(self)
    }
}

pub(super) struct DispatchedPut<'a>(&'a PutOutcomes);

impl DispatchedPut<'_> {
    pub(super) fn settle(self) {
        self.0.0.fetch_sub(1, Ordering::Release);
    }
}

pub(crate) enum Payload {
    InMemory(Bytes),
    Streamed {
        len: u64,
        stream: ByteStream,
        mismatch: Arc<AtomicBool>,
    },
}

impl Payload {
    pub(crate) fn len(&self) -> u64 {
        match self {
            Self::InMemory(bytes) => bytes.len() as u64,
            Self::Streamed { len, .. } => *len,
        }
    }

    pub(crate) async fn verify_empty_stream(
        mut stream: ByteStream,
        mismatch: Arc<AtomicBool>,
    ) -> Result<Self, UploadFailure> {
        match stream.next().await {
            Some(_) if mismatch.load(Ordering::Acquire) => Err(UploadFailure::Length),
            Some(_) => Err(UploadFailure::Body),
            None => Ok(Self::InMemory(Bytes::new())),
        }
    }

    pub(crate) fn into_stream(self) -> (u64, ByteStream, Option<Arc<AtomicBool>>) {
        match self {
            Self::InMemory(bytes) => (bytes.len() as u64, ByteStream::from(bytes), None),
            Self::Streamed {
                len,
                stream,
                mismatch,
            } => (len, stream, Some(mismatch)),
        }
    }
}

impl PutBody {
    pub fn stream<B>(len: u64, body: B) -> Self
    where
        B: Body<Data = Bytes> + Send + 'static,
        B::Error: Into<BoxError> + 'static,
    {
        let (body, mismatch) = ExactLength::wrap_with_mismatch_flag(len, body);
        Self {
            payload: Payload::Streamed {
                len,
                stream: ByteStream::from_body_1_x(body),
                mismatch,
            },
            outcomes: None,
        }
    }

    #[must_use]
    pub fn len(&self) -> u64 {
        self.payload.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl From<Bytes> for PutBody {
    fn from(bytes: Bytes) -> Self {
        Self {
            payload: Payload::InMemory(bytes),
            outcomes: None,
        }
    }
}

impl From<Vec<u8>> for PutBody {
    fn from(bytes: Vec<u8>) -> Self {
        Bytes::from(bytes).into()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("upload body length differs from the declared length")]
struct LengthMismatch;

#[derive(Default)]
struct Drained {
    held: Option<Bytes>,
    trailers: Option<Frame<Bytes>>,
}

enum ExactLengthPhase {
    Streaming(ByteProgress),
    Draining(Drained),
}

enum StreamStep {
    Continue,
    Yield(Result<Frame<Bytes>, BoxError>),
    Drain(Drained),
    Mismatch,
}

fn drain(progress: &mut ByteProgress, trailers: Option<Frame<Bytes>>) -> StreamStep {
    match progress.finish() {
        Ok(held) => StreamStep::Drain(Drained { held, trailers }),
        Err(BodyEndedEarly) => StreamStep::Mismatch,
    }
}

fn advance_stream<E: Into<BoxError>>(
    progress: &mut ByteProgress,
    polled: Option<Result<Frame<Bytes>, E>>,
) -> StreamStep {
    match polled {
        Some(Err(error)) => StreamStep::Yield(Err(error.into())),
        Some(Ok(frame)) => match frame.into_data() {
            Ok(data) if data.is_empty() => StreamStep::Continue,
            Ok(data) => match progress.accept(data) {
                Ok(Some(data)) => StreamStep::Yield(Ok(Frame::data(data))),
                Ok(None) => StreamStep::Continue,
                Err(_rejected) => StreamStep::Mismatch,
            },
            Err(trailers) => drain(progress, Some(trailers)),
        },
        None => drain(progress, None),
    }
}

pin_project_lite::pin_project! {
    struct ExactLength<B> {
        #[pin]
        inner: SyncWrapper<B>,
        phase: ExactLengthPhase,
        mismatch: Arc<AtomicBool>,
    }
}

impl<B> ExactLength<B> {
    fn wrap_with_mismatch_flag(len: u64, body: B) -> (Self, Arc<AtomicBool>) {
        let mismatch = Arc::new(AtomicBool::new(false));
        let phase = ExactLengthPhase::Streaming(ByteProgress::new(len));
        let body = Self {
            inner: SyncWrapper::new(body),
            phase,
            mismatch: Arc::clone(&mismatch),
        };
        (body, mismatch)
    }
}

impl<B> Body for ExactLength<B>
where
    B: Body<Data = Bytes>,
    B::Error: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let mut this = self.project();
        let mut polls_remaining = SOURCE_POLLS_PER_YIELD;
        loop {
            let progress = match this.phase {
                ExactLengthPhase::Draining(Drained { held, trailers }) => {
                    if let Some(data) = held.take() {
                        return Poll::Ready(Some(Ok(Frame::data(data))));
                    }
                    return Poll::Ready(trailers.take().map(Ok));
                }
                ExactLengthPhase::Streaming(progress) => progress,
            };
            if polls_remaining == 0 {
                context.waker().wake_by_ref();
                return Poll::Pending;
            }
            polls_remaining -= 1;
            let polled = std::task::ready!(this.inner.as_mut().get_pin_mut().poll_frame(context));
            match advance_stream(progress, polled) {
                StreamStep::Continue => {}
                StreamStep::Yield(item) => return Poll::Ready(Some(item)),
                StreamStep::Drain(drained) => {
                    *this.phase = ExactLengthPhase::Draining(drained);
                }
                StreamStep::Mismatch => {
                    this.mismatch.store(true, Ordering::Release);
                    *this.phase = ExactLengthPhase::Draining(Drained::default());
                    return Poll::Ready(Some(Err(Box::new(LengthMismatch))));
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        matches!(
            self.phase,
            ExactLengthPhase::Draining(Drained {
                held: None,
                trailers: None,
            })
        )
    }

    fn size_hint(&self) -> SizeHint {
        let exact = match &self.phase {
            ExactLengthPhase::Streaming(progress) => progress.remaining(),
            ExactLengthPhase::Draining(Drained { held, .. }) => {
                held.as_ref().map_or(0, |data| data.len() as u64)
            }
        };
        SizeHint::with_exact(exact)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::task::{Wake, Waker};

    use futures_util::stream;
    use http_body_util::StreamBody;

    use super::*;

    #[test]
    fn put_body_debug_hides_payload() {
        let body = PutBody::from(Bytes::from_static(b"private-upload-data"));
        let rendered = format!("{body:?}");
        assert!(!rendered.contains("private-upload-data"), "{rendered}");
    }

    fn send<const N: usize>(len: u64, chunks: [&'static [u8]; N]) -> (Result<u64, ()>, bool) {
        let frames =
            chunks.map(|chunk| Ok::<_, Infallible>(Frame::data(Bytes::from_static(chunk))));
        let body = StreamBody::new(stream::iter(frames));
        let (body, mismatch) = ExactLength::wrap_with_mismatch_flag(len, body);
        let mut body = std::pin::pin!(body);
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut sent = 0;
        while sent < len {
            match body.as_mut().poll_frame(&mut context) {
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
        assert_eq!(send(8, [b"abcdefgh"]), (Ok(8), false));
        assert_eq!(send(8, [b"abcd", b"", b"efgh"]), (Ok(8), false));
    }

    #[test]
    fn a_frame_past_the_declared_length_fails_the_body() {
        assert_eq!(send(4, [b"abcd", b"efgh"]), (Err(()), true));
        assert_eq!(send(4, [b"abcdefgh"]), (Err(()), true));
    }

    #[test]
    fn a_short_body_fails_the_body() {
        assert_eq!(send(8, [b"abcd"]), (Err(()), true));
    }

    #[derive(Default)]
    struct WakeFlag(AtomicBool);

    impl Wake for WakeFlag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    type FramePoll = Poll<Option<Result<Frame<Bytes>, std::io::Error>>>;

    fn data_step(data: &'static [u8]) -> FramePoll {
        Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(data)))))
    }

    struct PolledFrames {
        steps: VecDeque<FramePoll>,
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
            assert!(self.polls.fetch_add(1, Ordering::Relaxed) < SOURCE_POLLS_PER_YIELD);
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
                steps: first.map(data_step).into_iter().collect(),
                empty_tail: true,
                polls: Arc::clone(&polls),
            };
            let (body, mismatch) = ExactLength::wrap_with_mismatch_flag(len, source);
            let mut body = std::pin::pin!(body);
            let wake = Arc::new(WakeFlag::default());
            let waker = Waker::from(Arc::clone(&wake));
            let mut context = Context::from_waker(&waker);
            for _ in 0..3 {
                polls.store(0, Ordering::Relaxed);
                assert!(body.as_mut().poll_frame(&mut context).is_pending());
                assert_eq!(polls.load(Ordering::Relaxed), SOURCE_POLLS_PER_YIELD);
                assert!(wake.0.swap(false, Ordering::Relaxed));
                assert_eq!(body.size_hint().exact(), Some(len));
                assert!(!body.is_end_stream());
                assert!(!mismatch.load(Ordering::Acquire));
            }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Ending {
        Eof,
        Overflow,
        Trailers,
        SourceError,
    }

    #[test]
    fn finite_stream_results_survive_budget_yields_and_source_pending() {
        for (len, payload, ending) in [
            (0, b"".as_slice(), Ending::Eof),
            (4, b"abcd".as_slice(), Ending::Eof),
            (5, b"abcd".as_slice(), Ending::Eof),
            (4, b"abcd".as_slice(), Ending::Overflow),
            (4, b"abcd".as_slice(), Ending::Trailers),
            (4, b"abcd".as_slice(), Ending::SourceError),
        ] {
            let mut steps = VecDeque::new();
            for _ in 0..65 {
                steps.push_back(data_step(b""));
            }
            steps.push_back(data_step(payload));
            for _ in 0..130 {
                steps.push_back(data_step(b""));
            }
            steps.push_back(Poll::Pending);
            let mut trailers = axum::http::HeaderMap::new();
            trailers.insert("x-checksum", axum::http::HeaderValue::from_static("1"));
            match ending {
                Ending::Overflow => steps.push_back(data_step(b"!")),
                Ending::Trailers => {
                    steps.push_back(Poll::Ready(Some(Ok(Frame::trailers(trailers.clone())))));
                }
                Ending::SourceError => steps.push_back(Poll::Ready(Some(Err(
                    std::io::Error::other("source failed"),
                )))),
                Ending::Eof => {}
            }
            let polls = Arc::new(AtomicUsize::new(0));
            let source = PolledFrames {
                steps,
                empty_tail: false,
                polls: Arc::clone(&polls),
            };
            let (body, mismatch) = ExactLength::wrap_with_mismatch_flag(len, source);
            let mut body = std::pin::pin!(body);
            let wake = Arc::new(WakeFlag::default());
            let waker = Waker::from(Arc::clone(&wake));
            let mut context = Context::from_waker(&waker);
            let mut received = Vec::new();
            let mut received_trailers = None;
            let mut source_pending = false;
            let mut completed = false;
            for _ in 0..10 {
                polls.store(0, Ordering::Relaxed);
                match body.as_mut().poll_frame(&mut context) {
                    Poll::Pending => {
                        if polls.load(Ordering::Relaxed) == SOURCE_POLLS_PER_YIELD {
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
                        if ending == Ending::SourceError {
                            let source_error = error.downcast_ref::<std::io::Error>().unwrap();
                            assert_eq!(source_error.to_string(), "source failed");
                            assert!(!mismatch.load(Ordering::Acquire));
                        } else {
                            assert!(len == 5 || ending == Ending::Overflow);
                            assert!(error.is::<LengthMismatch>());
                            assert!(mismatch.load(Ordering::Acquire));
                            assert!(body.is_end_stream());
                        }
                        completed = true;
                        break;
                    }
                    Poll::Ready(None) => {
                        assert!(matches!(ending, Ending::Eof | Ending::Trailers));
                        assert_eq!(received, payload);
                        assert_eq!(received.len() as u64, len);
                        assert_eq!(
                            received_trailers,
                            (ending == Ending::Trailers).then_some(trailers)
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
}
