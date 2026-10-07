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
/// checksum validation. The SDK permits absent checksums and skips composite
/// (`-N`) or invalid-base64 checksums. Success does not attest that a checksum
/// was validated; a feature requiring end-to-end integrity checks its own
/// authoritative digest.
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
        let unread = {
            let state = lock(&self.shared);
            match &state.status {
                DownloadState::Open(resources) => {
                    resources.remaining
                        + resources
                            .last
                            .as_ref()
                            .map_or(0, |chunk| chunk.len() as u64)
                }
                DownloadState::Succeeded(last) => {
                    last.as_ref().map_or(0, |chunk| chunk.len() as u64)
                }
                DownloadState::Failed(_) => 0,
            }
        };
        let mut buffer = Vec::with_capacity(usize::try_from(unread).unwrap_or(0));
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::Waker;
    use std::time::Duration;

    use http_body::Body as _;
    use http_body_util::BodyExt as _;
    use tokio::sync::{Semaphore, oneshot};
    use tokio::time::Instant;

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

    fn download(timeout: Duration) -> (Download, Arc<Semaphore>, oneshot::Sender<()>) {
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
                OperationContext::with_timeout(timeout),
            ),
            admission,
            send_eof,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn collection_reserves_only_the_unread_tail() {
        for (read_chunks, expected) in [(0, &b"abcde"[..]), (1, &b"de"[..]), (2, &b""[..])] {
            let (mut download, admission, eof) = download(Duration::from_secs(60));
            eof.send(()).unwrap();
            for _ in 0..read_chunks {
                download.next_chunk().await.unwrap().unwrap();
            }
            if read_chunks == 2 {
                joined(download.timer.take().unwrap()).await;
                tokio::time::advance(Duration::from_secs(60)).await;
                assert_eq!(download.next_chunk().await.unwrap(), None);
                assert!(download.is_end_stream());
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
        let (mut download, admission, eof) = download(Duration::from_secs(60));
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

    struct ObservedBody {
        chunk: Option<Bytes>,
        error: Option<Box<dyn std::error::Error + Send + Sync>>,
        drops: Arc<AtomicUsize>,
    }

    impl http_body::Body for ObservedBody {
        type Data = Bytes;
        type Error = Box<dyn std::error::Error + Send + Sync>;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            Poll::Ready(if let Some(chunk) = self.chunk.take() {
                Some(Ok(Frame::data(chunk)))
            } else {
                self.error.take().map(Err)
            })
        }
    }

    impl Drop for ObservedBody {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn read(
        download: &mut Download,
        context: &mut Context<'_>,
        frames: bool,
    ) -> Poll<Result<Option<Bytes>, ObjectStorageError>> {
        if frames {
            std::pin::pin!(download.frame()).poll(context).map(|frame| {
                frame
                    .map(|result| result.map(|frame| frame.into_data().unwrap()))
                    .transpose()
            })
        } else {
            std::pin::pin!(download.next_chunk()).poll(context)
        }
    }

    fn assert_download_failure(
        chunk: &'static [u8],
        error: Option<Box<dyn std::error::Error + Send + Sync>>,
        expected: ObjectStorageError,
        partial: bool,
        frames: bool,
    ) -> (Download, Arc<AtomicUsize>, JoinHandle<()>) {
        let drops = Arc::new(AtomicUsize::new(0));
        let admission = Arc::new(Semaphore::new(1));
        let metadata = ObjectMetadata {
            size: 4,
            content_type: Some("text/plain".into()),
            last_modified: Some(std::time::SystemTime::UNIX_EPOCH),
            e_tag: Some("opaque-tag".into()),
        };
        let mut download = Download::open(
            metadata.clone(),
            ByteStream::from_body_1_x(ObservedBody {
                chunk: Some(Bytes::from_static(chunk)),
                error,
                drops: Arc::clone(&drops),
            }),
            OperationGuard::start(Arc::new(Histograms::default()), Operation::Get, None),
            Arc::clone(&admission).try_acquire_owned().unwrap(),
            OperationContext::with_timeout(Duration::from_secs(60)),
        );
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(admission.available_permits(), 0);
        if partial {
            assert_eq!(
                read(&mut download, &mut context, frames),
                Poll::Ready(Ok(Some(Bytes::from_static(b"ab"))))
            );
        }
        assert_eq!(
            read(&mut download, &mut context, frames),
            Poll::Ready(Err(expected)),
            "chunk={chunk:?}, frames={frames}"
        );
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "body must be dropped before returning the error"
        );
        assert_eq!(admission.available_permits(), 1);
        assert_eq!(download.metadata(), &metadata);
        assert_eq!(download.size_hint().lower(), 0);
        assert_eq!(download.size_hint().upper(), None);
        assert!(!download.is_end_stream());
        let timer = download.timer.take().unwrap();
        (download, drops, timer)
    }

    #[tokio::test(start_paused = true)]
    async fn retained_failed_download_releases_body_and_finishes_once() {
        type BodyError = Box<dyn std::error::Error + Send + Sync>;
        for frames in [false, true] {
            let cases: [(&[u8], Option<BodyError>, _, _); 4] = [
                (
                    b"data",
                    Some(std::io::Error::other("broken body").into()),
                    ObjectStorageError::Unavailable,
                    false,
                ),
                (
                    b"data",
                    Some(
                        aws_smithy_checksums::body::validate::Error::ChecksumMismatch {
                            expected: Bytes::from_static(b"expected"),
                            actual: Bytes::from_static(b"actual"),
                        }
                        .into(),
                    ),
                    ObjectStorageError::Integrity,
                    false,
                ),
                (b"ab", None, ObjectStorageError::Integrity, true),
                (b"extra", None, ObjectStorageError::Integrity, false),
            ];
            for (chunk, error, expected, partial) in cases {
                let recorder =
                    metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
                let handle = recorder.handle();
                let (mut download, drops, timer) = metrics::with_local_recorder(&recorder, || {
                    assert_download_failure(chunk, error, expected, partial, frames)
                });
                joined(timer).await;
                tokio::time::advance(Duration::from_secs(60)).await;
                let mut context = Context::from_waker(Waker::noop());
                assert_eq!(
                    read(&mut download, &mut context, frames),
                    Poll::Ready(Err(expected))
                );
                assert_eq!(
                    std::pin::pin!(download.bytes()).poll(&mut context),
                    Poll::Ready(Err(expected))
                );
                assert_eq!(drops.load(Ordering::SeqCst), 1);
                let rendered = handle.render();
                let outcome = expected.label();
                assert!(rendered.contains(&format!(
                    "object_storage_operation_duration_seconds_count{{operation=\"get\",outcome=\"{outcome}\"}} 1"
                )), "{rendered}");
                assert!(!rendered.contains("outcome=\"cancelled\""), "{rendered}");
                assert!(!rendered.contains("outcome=\"ok\""), "{rendered}");
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stopped_context_releases_unpolled_custody_and_observes_once() {
        for cancelled in [false, true] {
            let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
            let handle = recorder.handle();
            let _recorder = metrics::set_default_local_recorder(&recorder);
            let admission = Arc::new(Semaphore::new(1));
            let guard =
                OperationGuard::start(Arc::new(Histograms::default()), Operation::Get, None);
            let (body, body_dropped) = body(None);
            let context = OperationContext::with_timeout(Duration::from_secs(2));
            let mut download = Download::open(
                ObjectMetadata {
                    size: 1,
                    content_type: None,
                    last_modified: None,
                    e_tag: None,
                },
                ByteStream::from_body_1_x(body),
                guard,
                Arc::clone(&admission).try_acquire_owned().unwrap(),
                context.child_context(),
            );
            if cancelled {
                context.cancel();
            } else {
                tokio::time::advance(Duration::from_secs(2)).await;
            }
            joined(download.timer.take().unwrap()).await;
            tokio::time::timeout(Duration::from_secs(1), body_dropped)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(admission.available_permits(), 1);
            assert_eq!(
                download.next_chunk().await,
                Err(ObjectStorageError::Unavailable)
            );
            drop(download);
            let rendered = handle.render();
            assert!(rendered.contains(
                r#"object_storage_operation_duration_seconds_count{operation="get",outcome="unavailable"} 1"#
            ), "{rendered}");
            assert!(!rendered.contains("cancelled"), "{rendered}");
        }
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
    async fn a_provider_chunk_or_eof_at_the_original_cutoff_is_not_delivered() {
        for chunk in [None, Some(Bytes::from_static(b"x"))] {
            let size = u64::from(chunk.is_some());
            let context = OperationContext::with_timeout(Duration::from_millis(10));
            let cutoff = context
                .deadline()
                .and_then(operation_context::Deadline::instant)
                .unwrap();
            let admission = Arc::new(Semaphore::new(1));
            let mut download = Download::open(
                ObjectMetadata {
                    size,
                    content_type: None,
                    last_modified: None,
                    e_tag: None,
                },
                ByteStream::from_body_1_x(LateChunk { cutoff, chunk }),
                OperationGuard::start(Arc::new(Histograms::default()), Operation::Get, None),
                Arc::clone(&admission).try_acquire_owned().unwrap(),
                context,
            );
            assert_eq!(
                download.next_chunk().await,
                Err(ObjectStorageError::Unavailable)
            );
            assert_eq!(admission.available_permits(), 1);
            joined(download.timer.take().unwrap()).await;
        }
    }

    struct LifecycleBody {
        chunk: Option<Bytes>,
        dropped: Option<oneshot::Sender<()>>,
        empty_polls: Option<Arc<AtomicUsize>>,
        panic: bool,
    }

    impl http_body::Body for LifecycleBody {
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
            Poll::Pending
        }
    }

    impl Drop for LifecycleBody {
        fn drop(&mut self) {
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(());
            }
        }
    }

    fn open(body: LifecycleBody, size: u64, end: Instant) -> (Download, Arc<Semaphore>) {
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
                OperationContext::from_deadline(operation_context::Deadline::at(end)),
            ),
            slots,
        )
    }

    fn body(chunk: Option<Bytes>) -> (LifecycleBody, oneshot::Receiver<()>) {
        let (dropped, destroyed) = oneshot::channel();
        (
            LifecycleBody {
                chunk,
                dropped: Some(dropped),
                empty_polls: None,
                panic: false,
            },
            destroyed,
        )
    }

    async fn joined(timer: JoinHandle<()>) {
        let result = tokio::time::timeout(Duration::from_secs(1), timer)
            .await
            .unwrap();
        assert!(result.is_ok() || result.unwrap_err().is_cancelled());
    }

    #[tokio::test(start_paused = true)]
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
        let (body, destroyed) = body(Some(bytes));
        let end = tokio::time::sleep(Duration::from_millis(20)).deadline();
        let (mut download, slots) = open(body, 4, end);
        assert!(
            download
                .poll_chunk(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        tokio::time::advance(Duration::from_millis(20)).await;
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

    #[tokio::test(start_paused = true)]
    async fn empty_eof_and_pending_consumers_share_the_deadline() {
        for consumer in 0..3 {
            let (body, destroyed) = body(None);
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

    #[tokio::test(start_paused = true)]
    async fn unexpected_timer_abort_before_first_poll_fails_closed() {
        let (body, destroyed) = body(None);
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

    #[tokio::test(start_paused = true)]
    async fn drop_and_provider_panic_release_custody_before_timer_runs() {
        for panic in [false, true] {
            let (mut body, mut destroyed) = body(None);
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
    async fn ready_empty_frames_cooperate_with_the_deadline_timer() {
        let (mut body, destroyed) = body(None);
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
    #[tokio::test(start_paused = true)]
    async fn partial_reads_spend_the_original_budget() {
        let (mut download, admission, eof) = download(Duration::from_secs(3));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(download.next_chunk().await.unwrap().unwrap(), "abc");
        let timer = download.timer.take().unwrap();
        // A successful chunk must not start another three-second lifetime.
        tokio::time::advance(Duration::from_secs(2)).await;
        let _ = eof.send(());
        assert_eq!(
            download.next_chunk().await,
            Err(ObjectStorageError::Unavailable)
        );
        assert_eq!(admission.available_permits(), 1);
        joined(timer).await;
    }
}
