//! An open download: the object's body, read to its confirmed end.

use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::task::{Context, Poll, Waker};

use aws_sdk_s3::primitives::{ByteStream, ByteStreamError};
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use operation_context::OperationContext;
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::JoinHandle;

use crate::observe::OperationGuard;
use crate::{ObjectMetadata, ObjectStorageError, error, stop_type};

/// An open download. Dropping it releases the admission slot and owned SDK
/// body; the operation is recorded as `cancelled` unless it already ended.
/// A terminal failure releases the body before the error is returned, even if
/// the failed `Download` is retained. Neither behavior claims socket closure
/// or completion of work owned by the SDK.
///
/// It is an [`http_body::Body`] of exactly [`ObjectMetadata::size`] bytes,
/// so it can be returned as a response body. The slot and the SDK body live
/// through confirmed EOF/integrity under the original operation context. Its
/// weak timer releases unpolled retained custody at the same cutoff. A
/// presigned URL remains the appropriate path for a remote slow reader.
/// Already delivered bytes are caller-owned. The final chunk is withheld until
/// the body confirms EOF and the SDK has completed any supported full-object
/// checksum validation.
#[derive(Debug)]
pub struct Download {
    metadata: ObjectMetadata,
    shared: Arc<Mutex<State>>,
    timer: Option<JoinHandle<()>>,
}

#[derive(Debug)]
struct State {
    waker: Option<Waker>,
    status: DownloadState,
}

#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "State already lives in one Arc allocation; inline custody avoids another allocation"
)]
enum DownloadState {
    Open(Resources),
    Succeeded(Option<Bytes>),
    Failed(ObjectStorageError),
}

/// The operation lifetime, body, final chunk, permit, and observation move as
/// one cell. Terminal code takes this entire cell while holding the short lock,
/// then destroys and observes it after the lock is released.
#[derive(Debug)]
struct Resources {
    context: OperationContext,
    body: ByteStream,
    remaining: u64,
    last: Option<Bytes>,
    guard: OperationGuard,
    _permit: OwnedSemaphorePermit,
}

#[derive(Clone, Copy)]
enum Outcome {
    Success,
    Failure(ObjectStorageError, &'static str),
    Cancelled,
}

struct Completion {
    resources: Resources,
    outcome: Outcome,
    waker: Option<Waker>,
    /// A ready SDK poll may have produced bytes or an error at the cutoff. It
    /// is discarded only after releasing the resource-cell lock.
    discarded: Option<Result<Bytes, ByteStreamError>>,
}

impl Completion {
    fn finish(mut self) {
        match self.outcome {
            Outcome::Success => self.resources.guard.succeed(),
            Outcome::Failure(error, class) => {
                self.resources.guard.fail(error, class);
            }
            Outcome::Cancelled => {}
        }
        drop(self.discarded);
        drop(self.resources);
        if let Some(waker) = self.waker {
            waker.wake();
        }
    }
}

fn lock(shared: &Mutex<State>) -> MutexGuard<'_, State> {
    shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A provider poll can advance custody without returning a caller payload.
/// Empty buffers cross the lock boundary before their destructor runs.
enum ReadStep {
    Buffered,
    Empty(Bytes),
    Chunk(Option<Bytes>),
}

type ReadPoll = Poll<Result<ReadStep, ObjectStorageError>>;

impl State {
    fn finish(&mut self, outcome: Outcome) -> Option<Completion> {
        if !matches!(self.status, DownloadState::Open(_)) {
            return None;
        }
        let terminal = match outcome {
            Outcome::Success => DownloadState::Succeeded(None),
            Outcome::Failure(error, _) => DownloadState::Failed(error),
            Outcome::Cancelled => DownloadState::Failed(ObjectStorageError::Unavailable),
        };
        let DownloadState::Open(mut resources) = std::mem::replace(&mut self.status, terminal)
        else {
            return None;
        };
        if let DownloadState::Succeeded(last) = &mut self.status {
            *last = resources.last.take();
        }
        Some(Completion {
            resources,
            outcome,
            waker: self.waker.take(),
            discarded: None,
        })
    }

    fn fail(
        &mut self,
        error: ObjectStorageError,
        class: &'static str,
    ) -> (ReadPoll, Option<Completion>) {
        (
            Poll::Ready(Err(error)),
            self.finish(Outcome::Failure(error, class)),
        )
    }

    fn fail_with_item(
        &mut self,
        error: ObjectStorageError,
        class: &'static str,
        item: Result<Bytes, ByteStreamError>,
    ) -> (ReadPoll, Option<Completion>) {
        let (result, mut completion) = self.fail(error, class);
        if let Some(completion) = &mut completion {
            completion.discarded = Some(item);
        }
        (result, completion)
    }

    fn poll(&mut self, context: &mut Context<'_>) -> (ReadPoll, Option<Completion>) {
        let resources = match &mut self.status {
            DownloadState::Open(resources) => resources,
            DownloadState::Succeeded(last) => {
                return (Poll::Ready(Ok(ReadStep::Chunk(last.take()))), None);
            }
            DownloadState::Failed(error) => return (Poll::Ready(Err(*error)), None),
        };
        if let Some(stopped) = resources.context.stopped() {
            return self.fail(ObjectStorageError::Unavailable, stop_type(stopped));
        }
        self.waker = Some(context.waker().clone());
        let Poll::Ready(coop) = tokio::task::coop::poll_proceed(context) else {
            return (Poll::Pending, None);
        };
        let polled = Pin::new(&mut resources.body).poll_next(context);
        if polled.is_ready() {
            coop.made_progress();
        }
        // Strict GET finality: a body poll that crossed the original cutoff
        // cannot yield a new chunk, error mapping, or successful EOF.
        if let Some(stopped) = resources.context.stopped() {
            let (result, mut completion) =
                self.fail(ObjectStorageError::Unavailable, stop_type(stopped));
            if let (Some(completion), Poll::Ready(item)) = (&mut completion, polled) {
                completion.discarded = item;
            }
            return (result, completion);
        }
        match polled {
            Poll::Pending => (Poll::Pending, None),
            Poll::Ready(Some(Ok(chunk))) if chunk.is_empty() => {
                (Poll::Ready(Ok(ReadStep::Empty(chunk))), None)
            }
            Poll::Ready(Some(Ok(chunk))) => match resources
                .remaining
                .checked_sub(chunk.len() as u64)
            {
                Some(0) => {
                    resources.remaining = 0;
                    resources.last = Some(chunk);
                    (Poll::Ready(Ok(ReadStep::Buffered)), None)
                }
                Some(remaining) => {
                    resources.remaining = remaining;
                    (Poll::Ready(Ok(ReadStep::Chunk(Some(chunk)))), None)
                }
                None => {
                    self.fail_with_item(ObjectStorageError::Integrity, "content_length", Ok(chunk))
                }
            },
            Poll::Ready(Some(Err(error))) if error::is_checksum_mismatch(&error) => {
                self.fail_with_item(ObjectStorageError::Integrity, "checksum", Err(error))
            }
            Poll::Ready(Some(Err(error))) => {
                self.fail_with_item(ObjectStorageError::Unavailable, "body", Err(error))
            }
            Poll::Ready(None) if resources.remaining == 0 => {
                let completion = self.finish(Outcome::Success);
                let chunk = match &mut self.status {
                    DownloadState::Succeeded(last) => last.take(),
                    _ => None,
                };
                (Poll::Ready(Ok(ReadStep::Chunk(chunk))), completion)
            }
            Poll::Ready(None) => self.fail(ObjectStorageError::Integrity, "content_length"),
        }
    }
}

/// Captured before the spawned timer can first poll. An unexpected timer exit
/// still frees retained body custody instead of depending on another read.
struct TimerExit(Weak<Mutex<State>>);

impl Drop for TimerExit {
    fn drop(&mut self) {
        if let Some(shared) = self.0.upgrade() {
            terminate(
                &shared,
                Outcome::Failure(ObjectStorageError::Unavailable, "body"),
            );
        }
    }
}

fn terminate(shared: &Mutex<State>, outcome: Outcome) {
    let completion = {
        let mut state = lock(shared);
        let outcome = match &state.status {
            DownloadState::Open(resources) => {
                resources.context.stopped().map_or(outcome, |stopped| {
                    Outcome::Failure(ObjectStorageError::Unavailable, stop_type(stopped))
                })
            }
            DownloadState::Succeeded(_) | DownloadState::Failed(_) => outcome,
        };
        state.finish(outcome)
    };
    if let Some(completion) = completion {
        completion.finish();
    }
}

impl Download {
    pub(crate) fn open(
        metadata: ObjectMetadata,
        body: ByteStream,
        guard: OperationGuard,
        permit: OwnedSemaphorePermit,
        context: OperationContext,
    ) -> Self {
        let shared = Arc::new(Mutex::new(State {
            waker: None,
            status: DownloadState::Open(Resources {
                context: context.clone(),
                body,
                remaining: metadata.size,
                last: None,
                guard,
                _permit: permit,
            }),
        }));
        let exit = TimerExit(Arc::downgrade(&shared));
        let timer_context = context;
        let timer = tokio::spawn(async move {
            let exit = exit;
            let stopped = timer_context.wait_stopped().await;
            if let Some(shared) = exit.0.upgrade() {
                terminate(
                    &shared,
                    Outcome::Failure(ObjectStorageError::Unavailable, stop_type(stopped)),
                );
            }
        });
        Self {
            metadata,
            shared,
            timer: Some(timer),
        }
    }

    /// Metadata from the response headers.
    #[must_use]
    pub fn metadata(&self) -> &ObjectMetadata {
        &self.metadata
    }

    /// The next chunk, or `None` at confirmed EOF. The SDK validates a
    /// supported full-object checksum there when available. A cancelled read
    /// preserves a withheld final chunk for the next reader while the context
    /// remains live.
    ///
    /// # Errors
    ///
    /// `Integrity` for a checksum mismatch or length disagreement;
    /// `Unavailable` for transport, cancellation, or original-cutoff expiry.
    /// Every later call preserves the same terminal result.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, ObjectStorageError> {
        std::future::poll_fn(|context| self.poll_chunk(context)).await
    }

    fn poll_chunk(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Option<Bytes>, ObjectStorageError>> {
        if let Some(timer) = &mut self.timer
            && let Poll::Ready(result) = Pin::new(timer).poll(context)
        {
            self.timer = None;
            if result.is_err() {
                terminate(
                    &self.shared,
                    Outcome::Failure(ObjectStorageError::Unavailable, "body"),
                );
            }
        }
        loop {
            let polled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                lock(&self.shared).poll(context)
            }));
            let (result, completion) = match polled {
                Ok(result) => result,
                Err(panic) => {
                    terminate(&self.shared, Outcome::Cancelled);
                    self.abort_timer();
                    std::panic::resume_unwind(panic);
                }
            };
            if let Some(completion) = completion {
                completion.finish();
            }
            if !matches!(lock(&self.shared).status, DownloadState::Open(_)) {
                self.abort_timer();
            }
            match result {
                Poll::Ready(Ok(ReadStep::Buffered)) => {}
                Poll::Ready(Ok(ReadStep::Empty(chunk))) => drop(chunk),
                Poll::Ready(Ok(ReadStep::Chunk(chunk))) => return Poll::Ready(Ok(chunk)),
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => return Poll::Pending,
            }
        }
    }

    fn abort_timer(&self) {
        if let Some(timer) = &self.timer {
            timer.abort();
        }
    }

    /// Consume and collect unread bytes under the original context. Returned
    /// bytes outlive admission and need a budget at their consumer.
    ///
    /// # Errors
    ///
    /// As [`Self::next_chunk`].
    pub async fn bytes(mut self) -> Result<Bytes, ObjectStorageError> {
        let mut buffer = Vec::with_capacity(usize::try_from(self.metadata.size).unwrap_or(0));
        while let Some(chunk) = self.next_chunk().await? {
            buffer.extend_from_slice(&chunk);
        }
        Ok(Bytes::from(buffer))
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        terminate(&self.shared, Outcome::Cancelled);
        self.abort_timer();
    }
}

impl http_body::Body for Download {
    type Data = Bytes;
    type Error = ObjectStorageError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, ObjectStorageError>>> {
        self.poll_chunk(context)
            .map(|chunk| chunk.map(|chunk| chunk.map(Frame::data)).transpose())
    }

    fn is_end_stream(&self) -> bool {
        matches!(lock(&self.shared).status, DownloadState::Succeeded(None))
    }

    fn size_hint(&self) -> SizeHint {
        let state = lock(&self.shared);
        let remaining = match &state.status {
            DownloadState::Open(resources) => {
                resources.remaining
                    + resources
                        .last
                        .as_ref()
                        .map_or(0, |chunk| chunk.len() as u64)
            }
            DownloadState::Succeeded(last) => last.as_ref().map_or(0, |chunk| chunk.len() as u64),
            // Hyper may skip an exact-zero body. Keep an error visible.
            DownloadState::Failed(_) => return SizeHint::default(),
        };
        SizeHint::with_exact(remaining)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::collections::VecDeque;
    use std::future::Future as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Waker;
    use std::time::Duration;

    use tokio::sync::{Semaphore, oneshot};

    use super::*;
    use crate::observe::{Histograms, Operation};

    struct GatedBody {
        chunks: VecDeque<Bytes>,
        eof: oneshot::Receiver<()>,
    }

    impl http_body::Body for GatedBody {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            if let Some(chunk) = self.chunks.pop_front() {
                return Poll::Ready(Some(Ok(Frame::data(chunk))));
            }
            std::task::ready!(Pin::new(&mut self.eof).poll(context)).unwrap();
            Poll::Ready(None)
        }
    }

    fn download() -> (Download, Arc<Semaphore>, oneshot::Sender<()>) {
        let admission = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&admission).try_acquire_owned().unwrap();
        let (send_eof, eof) = oneshot::channel();
        let body = GatedBody {
            chunks: [Bytes::from_static(b"abc"), Bytes::from_static(b"de")].into(),
            eof,
        };
        let metadata = ObjectMetadata {
            size: 5,
            content_type: None,
            last_modified: None,
            e_tag: None,
        };
        let guard = OperationGuard::start(Arc::new(Histograms::default()), Operation::Get, None);
        (
            Download::open(
                metadata,
                ByteStream::from_body_1_x(body),
                guard,
                permit,
                OperationContext::with_timeout(Duration::from_secs(60)),
            ),
            admission,
            send_eof,
        )
    }

    #[tokio::test]
    async fn collection_reserves_only_the_unread_tail() {
        for (read_chunks, expected) in [(0, &b"abcde"[..]), (1, &b"de"[..]), (2, &b""[..])] {
            let (mut download, admission, eof) = download();
            eof.send(()).unwrap();
            for _ in 0..read_chunks {
                download.next_chunk().await.unwrap().unwrap();
            }
            assert_eq!(download.metadata().size, 5);
            let bytes = tokio::time::timeout(Duration::from_secs(1), download.bytes())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(bytes.as_ref(), expected);
            let buffer = Vec::from(bytes);
            assert_eq!(buffer.capacity(), expected.len());
            assert_eq!(admission.available_permits(), 1);
        }
    }

    #[tokio::test]
    async fn cancelled_final_chunk_poll_is_collected_only_after_eof() {
        let (mut download, admission, eof) = download();
        assert_eq!(download.next_chunk().await.unwrap().unwrap(), "abc");
        {
            let mut next = std::pin::pin!(download.next_chunk());
            let mut context = Context::from_waker(Waker::noop());
            assert!(next.as_mut().poll(&mut context).is_pending());
        }
        assert_eq!(admission.available_permits(), 0);
        let mut collect = std::pin::pin!(download.bytes());
        let mut context = Context::from_waker(Waker::noop());
        assert!(collect.as_mut().poll(&mut context).is_pending());
        assert_eq!(admission.available_permits(), 0);
        eof.send(()).unwrap();
        let bytes = tokio::time::timeout(Duration::from_secs(1), collect)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bytes, "de");
        assert_eq!(Vec::from(bytes).capacity(), 2);
        assert_eq!(admission.available_permits(), 1);
    }

    struct FailingBody {
        error: Option<Box<dyn std::error::Error + Send + Sync>>,
        drops: Arc<AtomicUsize>,
    }

    impl http_body::Body for FailingBody {
        type Data = Bytes;
        type Error = Box<dyn std::error::Error + Send + Sync>;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            Poll::Ready(self.error.take().map(Err))
        }
    }

    impl Drop for FailingBody {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn retained_failed_download_releases_its_body_before_returning_the_error() {
        let drops = Arc::new(AtomicUsize::new(0));
        let admission = Arc::new(Semaphore::new(1));
        let guard = OperationGuard::start(Arc::new(Histograms::default()), Operation::Get, None);
        let metadata = ObjectMetadata {
            size: 1,
            content_type: None,
            last_modified: None,
            e_tag: None,
        };
        let mut download = Download::open(
            metadata,
            ByteStream::from_body_1_x(FailingBody {
                error: Some(std::io::Error::other("lost body").into()),
                drops: Arc::clone(&drops),
            }),
            guard,
            Arc::clone(&admission).try_acquire_owned().unwrap(),
            OperationContext::with_timeout(Duration::from_secs(60)),
        );
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(
            download.poll_chunk(&mut context),
            Poll::Ready(Err(ObjectStorageError::Unavailable))
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(admission.available_permits(), 1);
        assert_eq!(
            download.poll_chunk(&mut context),
            Poll::Ready(Err(ObjectStorageError::Unavailable))
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    struct PendingBody {
        dropped: Option<oneshot::Sender<()>>,
    }

    impl http_body::Body for PendingBody {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            Poll::Pending
        }
    }

    impl Drop for PendingBody {
        fn drop(&mut self) {
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(());
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn original_deadline_releases_an_unpolled_retained_body() {
        let admission = Arc::new(Semaphore::new(1));
        let guard = OperationGuard::start(Arc::new(Histograms::default()), Operation::Get, None);
        let (dropped, body_dropped) = oneshot::channel();
        let context = OperationContext::with_timeout(Duration::from_secs(2));
        let mut download = Download::open(
            ObjectMetadata {
                size: 1,
                content_type: None,
                last_modified: None,
                e_tag: None,
            },
            ByteStream::from_body_1_x(PendingBody {
                dropped: Some(dropped),
            }),
            guard,
            Arc::clone(&admission).try_acquire_owned().unwrap(),
            context,
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        body_dropped.await.unwrap();
        assert_eq!(admission.available_permits(), 1);
        assert_eq!(
            download.next_chunk().await,
            Err(ObjectStorageError::Unavailable)
        );
    }

    struct LateChunk {
        cutoff: tokio::time::Instant,
        chunk: Option<Bytes>,
    }

    impl http_body::Body for LateChunk {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            while tokio::time::Instant::now() < self.cutoff {
                std::hint::spin_loop();
            }
            Poll::Ready(self.chunk.take().map(|chunk| Ok(Frame::data(chunk))))
        }
    }

    #[tokio::test]
    async fn a_provider_chunk_at_the_original_cutoff_is_not_delivered() {
        let context = OperationContext::with_timeout(Duration::from_millis(10));
        let cutoff = context
            .deadline()
            .and_then(|deadline| deadline.instant())
            .unwrap();
        let admission = Arc::new(Semaphore::new(1));
        let mut download = Download::open(
            ObjectMetadata {
                size: 1,
                content_type: None,
                last_modified: None,
                e_tag: None,
            },
            ByteStream::from_body_1_x(LateChunk {
                cutoff,
                chunk: Some(Bytes::from_static(b"x")),
            }),
            OperationGuard::start(Arc::new(Histograms::default()), Operation::Get, None),
            Arc::clone(&admission).try_acquire_owned().unwrap(),
            context,
        );
        assert_eq!(
            download.next_chunk().await,
            Err(ObjectStorageError::Unavailable)
        );
        assert_eq!(admission.available_permits(), 1);
    }
}
