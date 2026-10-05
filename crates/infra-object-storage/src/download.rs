//! An open download: the object's body, read to its confirmed end.

use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::task::{Context, Poll, Waker};

use aws_sdk_s3::primitives::{ByteStream, ByteStreamError};
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::observe::OperationGuard;
use crate::{ObjectMetadata, ObjectStorageError, error};

/// An open download. Dropping it releases the admission slot and the
/// connection; the operation is recorded as `cancelled` unless the body
/// already ended.
///
/// It is an [`http_body::Body`] of exactly [`ObjectMetadata::size`] bytes,
/// so it can be returned as a response body. The slot is then held for as
/// long as the original operation timeout allows, through confirmed EOF.
/// Expiry releases the provider body and slot even without another read. Use
/// a presigned URL for remote slow readers, or an adequate existing timeout
/// within the parent budget for an in-process reader. The
/// download of an empty object has already ended. The chunk that completes the
/// object is released only after the provider's body has ended and any
/// SDK-supported full-object checksum has been validated: a reader that stops
/// at the declared length never holds a complete object that failed that check.
/// The SDK permits absent checksums and skips composite (`-N`) or invalid-base64
/// checksums. Success does not attest that a checksum was validated; a feature
/// requiring end-to-end integrity checks its own authoritative digest.
#[derive(Debug)]
pub struct Download {
    metadata: ObjectMetadata,
    shared: Arc<Mutex<State>>,
    timer: Option<JoinHandle<()>>,
}

#[derive(Debug)]
struct State {
    end: Instant,
    remaining: u64,
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

#[derive(Debug)]
struct Resources {
    body: ByteStream,
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

/// Terminal custody is extracted while locked, then observed and destroyed
/// after unlocking. Only the winner can obtain these resources.
struct Completion {
    resources: Resources,
    outcome: Outcome,
    waker: Option<Waker>,
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

/// A provider poll can advance custody without yielding a caller payload.
/// Empty buffers cross the lock boundary before their destructors run.
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

    fn poll(&mut self, context: &mut Context<'_>) -> (ReadPoll, Option<Completion>) {
        let resources = match &mut self.status {
            DownloadState::Open(resources) => resources,
            DownloadState::Succeeded(last) => {
                return (Poll::Ready(Ok(ReadStep::Chunk(last.take()))), None);
            }
            DownloadState::Failed(error) => return (Poll::Ready(Err(*error)), None),
        };
        if Instant::now() >= self.end {
            return self.fail(ObjectStorageError::Unavailable, "timeout");
        }
        self.waker = Some(context.waker().clone());
        let Poll::Ready(coop) = tokio::task::coop::poll_proceed(context) else {
            return (Poll::Pending, None);
        };
        let polled = Pin::new(&mut resources.body).poll_next(context);
        if polled.is_ready() {
            coop.made_progress();
        }
        // A provider may have spent the remainder of the budget in its poll.
        if Instant::now() >= self.end {
            let (result, mut completion) = self.fail(ObjectStorageError::Unavailable, "timeout");
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
            Poll::Ready(Some(Ok(chunk))) => match self.remaining.checked_sub(chunk.len() as u64) {
                Some(0) => {
                    self.remaining = 0;
                    resources.last = Some(chunk);
                    (Poll::Ready(Ok(ReadStep::Buffered)), None)
                }
                Some(remaining) => {
                    self.remaining = remaining;
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
            Poll::Ready(None) if self.remaining == 0 => {
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
}

/// Captured before spawn's first poll, including cancellation at runtime exit.
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
        let outcome = if Instant::now() >= state.end {
            Outcome::Failure(ObjectStorageError::Unavailable, "timeout")
        } else {
            outcome
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
        end: Instant,
    ) -> Self {
        let shared = Arc::new(Mutex::new(State {
            end,
            remaining: metadata.size,
            waker: None,
            status: DownloadState::Open(Resources {
                body,
                last: None,
                guard,
                _permit: permit,
            }),
        }));
        let exit = TimerExit(Arc::downgrade(&shared));
        let timer = tokio::spawn(async move {
            let exit = exit;
            tokio::time::sleep_until(end).await;
            if let Some(shared) = exit.0.upgrade() {
                terminate(
                    &shared,
                    Outcome::Failure(ObjectStorageError::Unavailable, "timeout"),
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

    /// The next chunk, or `None` at the confirmed end. The SDK validates a
    /// supported full-object checksum there when available (see [`Download`]).
    /// Cancelling this read preserves any withheld final chunk for the next read.
    ///
    /// # Errors
    ///
    /// `Integrity` for a checksum mismatch or a body that differs from its
    /// headers; `Unavailable` for transport, stall or original-deadline expiry.
    /// Every later call preserves the same terminal error or successful EOF.
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

    /// Consume the download and collect its remaining body. Chunks already
    /// returned by [`Download::next_chunk`] are not included.
    /// `max_object_bytes` bounds this collection's payload. Chunks are copied
    /// into its buffer; SDK buffers and allocation overhead add to it. The
    /// returned bytes outlive the admission slot, so retained collections need
    /// a separate budget in the consuming HTTP/job path.
    ///
    /// # Errors
    ///
    /// As [`Download::next_chunk`].
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
                state.remaining
                    + resources
                        .last
                        .as_ref()
                        .map_or(0, |chunk| chunk.len() as u64)
            }
            DownloadState::Succeeded(last) => last.as_ref().map_or(0, |chunk| chunk.len() as u64),
            // Hyper may skip polling an exact-zero body. Keep the error visible.
            DownloadState::Failed(_) => return SizeHint::default(),
        };
        SizeHint::with_exact(remaining)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::observe::{Histograms, Operation};
    use http_body_util::BodyExt;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::{Semaphore, oneshot};

    struct Body {
        chunk: Option<Bytes>,
        eof: Arc<AtomicBool>,
        dropped: Option<oneshot::Sender<()>>,
        empty_polls: Option<Arc<AtomicUsize>>,
        panic: bool,
    }

    impl http_body::Body for Body {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            assert!(!self.panic, "provider poll panic");
            if let Some(polls) = &self.empty_polls {
                polls.fetch_add(1, Ordering::SeqCst);
                return Poll::Ready(Some(Ok(Frame::data(Bytes::new()))));
            }
            if let Some(chunk) = self.chunk.take() {
                return Poll::Ready(Some(Ok(Frame::data(chunk))));
            }
            if self.eof.load(Ordering::SeqCst) {
                Poll::Ready(None)
            } else {
                Poll::Pending
            }
        }
    }

    impl Drop for Body {
        fn drop(&mut self) {
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(());
            }
        }
    }

    fn open(body: Body, size: u64, end: Instant) -> (Download, Arc<Semaphore>) {
        let slots = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&slots).try_acquire_owned().unwrap();
        let guard = OperationGuard::start(Arc::new(Histograms::default()), Operation::Get, None);
        let metadata = ObjectMetadata {
            size,
            content_type: None,
            last_modified: None,
            e_tag: None,
        };
        (
            Download::open(
                metadata,
                ByteStream::from_body_1_x(body),
                guard,
                permit,
                end,
            ),
            slots,
        )
    }

    fn body(chunk: Option<Bytes>, eof: bool) -> (Body, Arc<AtomicBool>, oneshot::Receiver<()>) {
        let eof = Arc::new(AtomicBool::new(eof));
        let (dropped, destroyed) = oneshot::channel();
        (
            Body {
                chunk,
                eof: Arc::clone(&eof),
                dropped: Some(dropped),
                empty_polls: None,
                panic: false,
            },
            eof,
            destroyed,
        )
    }

    async fn joined(timer: JoinHandle<()>) {
        let result = tokio::time::timeout(Duration::from_secs(1), timer)
            .await
            .unwrap();
        assert!(result.is_ok() || result.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn cancelling_a_read_keeps_the_final_chunk_and_success_is_final() {
        let end = tokio::time::sleep(Duration::from_millis(100)).deadline();
        let (body, eof, destroyed) = body(Some(Bytes::from_static(b"last")), false);
        let (mut download, slots) = open(body, 4, end);
        {
            let mut read = std::pin::pin!(download.next_chunk());
            assert!(
                read.as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
        }
        assert_eq!(http_body::Body::size_hint(&download).exact(), Some(4));
        assert_eq!(slots.available_permits(), 0);
        eof.store(true, Ordering::SeqCst);
        assert_eq!(
            download.next_chunk().await.unwrap(),
            Some(Bytes::from_static(b"last"))
        );
        tokio::time::timeout(Duration::from_secs(1), destroyed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(slots.available_permits(), 1);
        joined(download.timer.take().unwrap()).await;
        // Explicit clock progression, after independently joining timer cleanup.
        tokio::time::sleep_until(end).await;
        assert_eq!(download.next_chunk().await.unwrap(), None);
        assert!(http_body::Body::is_end_stream(&download));
    }

    #[tokio::test]
    async fn expiry_destroys_the_unpolled_body_and_withheld_chunk() {
        struct Tracked(Arc<AtomicBool>);
        impl AsRef<[u8]> for Tracked {
            fn as_ref(&self) -> &[u8] {
                b"last"
            }
        }
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let freed = Arc::new(AtomicBool::new(false));
        let bytes = Bytes::from_owner(Tracked(Arc::clone(&freed)));
        let (body, _, destroyed) = body(Some(bytes), false);
        let end = tokio::time::sleep(Duration::from_millis(20)).deadline();
        let (mut download, slots) = open(body, 4, end);
        assert!(
            download
                .poll_chunk(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        joined(download.timer.take().unwrap()).await;
        tokio::time::timeout(Duration::from_secs(1), destroyed)
            .await
            .unwrap()
            .unwrap();
        assert!(freed.load(Ordering::SeqCst));
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(
            download.next_chunk().await,
            Err(ObjectStorageError::Unavailable)
        );
    }

    #[tokio::test]
    async fn empty_eof_and_pending_consumers_share_the_deadline() {
        for consumer in 0..3 {
            let (body, _, destroyed) = body(None, false);
            let end = tokio::time::sleep(Duration::from_millis(20)).deadline();
            let (mut download, slots) = open(body, 0, end);
            let timer = download.timer.take().unwrap();
            let result = match consumer {
                0 => download.next_chunk().await.map(|_| ()),
                1 => download.frame().await.unwrap().map(|_| ()),
                _ => download.bytes().await.map(|_| ()),
            };
            assert_eq!(result, Err(ObjectStorageError::Unavailable));
            joined(timer).await;
            tokio::time::timeout(Duration::from_secs(1), destroyed)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(slots.available_permits(), 1);
        }
    }

    #[tokio::test]
    async fn unexpected_timer_abort_before_first_poll_fails_closed() {
        let (body, _, destroyed) = body(None, false);
        let end = tokio::time::sleep(Duration::from_secs(60)).deadline();
        let (mut download, slots) = open(body, 4, end);
        let timer = download.timer.take().unwrap();
        // This task has not yielded since spawn: the exit guard must already exist.
        timer.abort();
        joined(timer).await;
        tokio::time::timeout(Duration::from_secs(1), destroyed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(
            download.next_chunk().await,
            Err(ObjectStorageError::Unavailable)
        );
    }

    #[tokio::test]
    async fn drop_and_provider_panic_release_custody_before_timer_runs() {
        for panic in [false, true] {
            let (mut body, _, mut destroyed) = body(None, false);
            body.panic = panic;
            let end = tokio::time::sleep(Duration::from_secs(60)).deadline();
            let (mut download, slots) = open(body, 4, end);
            let completion = download.timer.as_ref().unwrap().abort_handle();
            if panic {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    download.poll_chunk(&mut Context::from_waker(Waker::noop()))
                }));
                assert!(result.is_err());
                assert_eq!(
                    download.next_chunk().await,
                    Err(ObjectStorageError::Unavailable)
                );
                assert!(destroyed.try_recv().is_ok());
                assert_eq!(slots.available_permits(), 1);
                if let Some(timer) = download.timer.take() {
                    joined(timer).await;
                }
            } else {
                drop(download);
                assert!(destroyed.try_recv().is_ok());
                assert_eq!(slots.available_permits(), 1);
            }
            tokio::time::timeout(Duration::from_secs(1), async {
                while !completion.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn integrity_failure_stays_final_after_the_deadline() {
        let (body, _, destroyed) = body(Some(Bytes::from_static(b"too long")), true);
        let end = tokio::time::sleep(Duration::from_millis(20)).deadline();
        let (mut download, slots) = open(body, 1, end);
        assert_eq!(
            download.next_chunk().await,
            Err(ObjectStorageError::Integrity)
        );
        tokio::time::timeout(Duration::from_secs(1), destroyed)
            .await
            .unwrap()
            .unwrap();
        joined(download.timer.take().unwrap()).await;
        assert_eq!(slots.available_permits(), 1);
        tokio::time::sleep_until(end).await;
        assert_eq!(
            download.next_chunk().await,
            Err(ObjectStorageError::Integrity)
        );
    }

    #[tokio::test]
    async fn a_late_provider_payload_or_eof_cannot_escape() {
        struct Late {
            end: Instant,
            chunk: Option<Bytes>,
        }
        impl http_body::Body for Late {
            type Data = Bytes;
            type Error = std::io::Error;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
                // Model one bounded provider poll crossing the deadline.
                while Instant::now() < self.end {
                    std::hint::spin_loop();
                }
                Poll::Ready(self.chunk.take().map(|chunk| Ok(Frame::data(chunk))))
            }
        }
        for chunk in [None, Some(Bytes::from_static(b"late"))] {
            let size = if chunk.is_some() { 8 } else { 0 };
            let slots = Arc::new(Semaphore::new(1));
            let permit = Arc::clone(&slots).try_acquire_owned().unwrap();
            let guard =
                OperationGuard::start(Arc::new(Histograms::default()), Operation::Get, None);
            let metadata = ObjectMetadata {
                size,
                content_type: None,
                last_modified: None,
                e_tag: None,
            };
            let end = tokio::time::sleep(Duration::from_millis(10)).deadline();
            let body = ByteStream::from_body_1_x(Late { end, chunk });
            let mut download = Download::open(metadata, body, guard, permit, end);
            assert_eq!(
                download.next_chunk().await,
                Err(ObjectStorageError::Unavailable)
            );
            assert_eq!(slots.available_permits(), 1);
            joined(download.timer.take().unwrap()).await;
        }
    }

    #[tokio::test]
    async fn ready_empty_frames_cooperate_with_the_deadline_timer() {
        let (mut body, _, destroyed) = body(None, false);
        let polls = Arc::new(AtomicUsize::new(0));
        body.empty_polls = Some(Arc::clone(&polls));
        let end = tokio::time::sleep(Duration::from_millis(20)).deadline();
        let (mut download, slots) = open(body, 4, end);
        let timer = download.timer.take().unwrap();
        let progressed = Arc::new(AtomicBool::new(false));
        let progress = Arc::clone(&progressed);
        let other = tokio::spawn(async move {
            progress.store(true, Ordering::SeqCst);
        });
        assert_eq!(
            download.next_chunk().await,
            Err(ObjectStorageError::Unavailable)
        );
        assert!(
            progressed.load(Ordering::SeqCst),
            "ready frames must yield to other tasks"
        );
        tokio::time::timeout(Duration::from_secs(1), other)
            .await
            .unwrap()
            .unwrap();
        assert!(polls.load(Ordering::SeqCst) > 0);
        joined(timer).await;
        tokio::time::timeout(Duration::from_secs(1), destroyed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(slots.available_permits(), 1);
    }
}
