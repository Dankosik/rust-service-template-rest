use std::future::Future as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use http_body::Body as _;
use http_body_util::{BodyExt as _, StreamBody};
use operation_context::{OperationContext, Stopped};
use tokio::sync::{Semaphore, oneshot};
use tokio::task::AbortHandle;

use super::*;
use crate::observe::{Histograms, Operation};
use crate::tests::{Records, in_tracing_child};

fn local_context(timeout: Duration) -> BoundedContext {
    OperationContext::unbounded().bounded(timeout)
}

fn download(timeout: Duration) -> (Download, Arc<Semaphore>, oneshot::Sender<()>) {
    let admission = Arc::new(Semaphore::new(1));
    let permit = Arc::clone(&admission).try_acquire_owned().unwrap();
    let (send_eof, eof) = oneshot::channel();
    let body = StreamBody::new(async_stream::stream! {
        yield Ok::<_, std::convert::Infallible>(Frame::data(Bytes::from_static(b"abc")));
        yield Ok(Frame::data(Bytes::from_static(b"de")));
        eof.await.unwrap();
    });
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
            local_context(timeout),
        ),
        admission,
        send_eof,
    )
}

#[tokio::test(start_paused = true)]
async fn collection_reserves_only_the_unread_tail() {
    for (read_chunks, expected) in [(0, &b"abcde"[..]), (1, &b"de"[..]), (2, &b""[..])] {
        let (mut download, admission, eof) = download(Duration::from_mins(1));
        let timer = download.timer.as_ref().unwrap().abort_handle();
        eof.send(()).unwrap();
        for _ in 0..read_chunks {
            download.next_chunk().await.unwrap().unwrap();
        }
        if read_chunks == 2 {
            wait_for_timer(&timer).await;
            tokio::time::advance(Duration::from_mins(1)).await;
            assert_eq!(download.next_chunk().await.unwrap(), None);
            assert!(download.is_end_stream());
        }
        assert_eq!(download.metadata().size, 5);
        let bytes =
            tokio::time::timeout(Duration::from_secs(1), download.into_bytes(expected.len()))
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
    let (mut download, admission, eof) = download(Duration::from_mins(1));
    assert_eq!(download.next_chunk().await.unwrap().unwrap(), "abc");
    {
        let mut next = std::pin::pin!(download.next_chunk());
        let mut context = Context::from_waker(Waker::noop());
        assert!(next.as_mut().poll(&mut context).is_pending());
    }
    assert_eq!(admission.available_permits(), 0);
    let mut collect = std::pin::pin!(download.into_bytes(2));
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

#[tokio::test(start_paused = true)]
async fn collection_rejects_an_oversized_unread_tail_before_eof() {
    for (hold_final_chunk, limit) in [(false, 4), (true, 1)] {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let (mut download, admission, eof) = download(Duration::from_mins(1));
        let mut context = Context::from_waker(Waker::noop());
        if hold_final_chunk {
            assert_eq!(download.next_chunk().await.unwrap().unwrap(), "abc");
            assert!(
                std::pin::pin!(download.next_chunk())
                    .poll(&mut context)
                    .is_pending()
            );
        }
        assert_eq!(
            std::pin::pin!(download.into_bytes(limit)).poll(&mut context),
            Poll::Ready(Err(ObjectStorageError::TooLarge))
        );
        assert!(eof.is_closed());
        assert_eq!(admission.available_permits(), 1);
        let rendered = handle.render();
        assert!(rendered.contains(
            r#"object_storage_operation_duration_seconds_count{operation="get",outcome="too_large"} 1"#
        ), "{rendered}");
        assert!(!rendered.contains("outcome=\"cancelled\""), "{rendered}");
        assert!(!rendered.contains("outcome=\"ok\""), "{rendered}");
    }
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
) -> (Download, Arc<AtomicUsize>, AbortHandle) {
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
        local_context(Duration::from_mins(1)),
    );
    let timer = download.timer.as_ref().unwrap().abort_handle();
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
    (download, drops, timer)
}

#[tokio::test(start_paused = true)]
async fn retained_failed_download_releases_body_and_finishes_once() {
    type BodyError = Box<dyn std::error::Error + Send + Sync>;
    for frames in [false, true] {
        let cases: [(&[u8], Option<BodyError>, _, _, _); 4] = [
            (
                b"data",
                Some(std::io::Error::other("broken body").into()),
                ObjectStorageError::Unavailable,
                "unavailable",
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
                "integrity",
                false,
            ),
            (
                b"ab",
                None,
                ObjectStorageError::Integrity,
                "integrity",
                true,
            ),
            (
                b"extra",
                None,
                ObjectStorageError::Integrity,
                "integrity",
                false,
            ),
        ];
        for (chunk, error, expected, outcome, partial) in cases {
            let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
            let handle = recorder.handle();
            let (mut download, drops, timer) = metrics::with_local_recorder(&recorder, || {
                assert_download_failure(chunk, error, expected, partial, frames)
            });
            wait_for_timer(&timer).await;
            tokio::time::advance(Duration::from_mins(1)).await;
            let mut context = Context::from_waker(Waker::noop());
            assert_eq!(
                read(&mut download, &mut context, frames),
                Poll::Ready(Err(expected))
            );
            assert_eq!(
                std::pin::pin!(download.into_bytes(0)).poll(&mut context),
                Poll::Ready(Err(expected))
            );
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            let rendered = handle.render();
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
    if !in_tracing_child(
        "download::tests::stopped_context_releases_unpolled_custody_and_observes_once",
    ) {
        return;
    }
    let records = Records::default();
    let _subscriber = records.install_default();
    let cases = [
        (
            Some(Duration::from_secs(2)),
            Duration::from_mins(1),
            ObjectStorageError::Stopped(Stopped::Deadline),
            Some("parent_deadline"),
        ),
        (
            None,
            Duration::from_secs(2),
            ObjectStorageError::Unavailable,
            Some("storage_timeout"),
        ),
        (
            None,
            Duration::from_mins(1),
            ObjectStorageError::Stopped(Stopped::Cancelled),
            None,
        ),
    ];
    for (index, (parent_timeout, storage_timeout, expected, timeout_source)) in
        cases.into_iter().enumerate()
    {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let admission = Arc::new(Semaphore::new(1));
        let guard = OperationGuard::start(Arc::new(Histograms::default()), Operation::Get, None);
        let (body, body_dropped) = body(None);
        let context =
            parent_timeout.map_or_else(OperationContext::unbounded, OperationContext::with_timeout);
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
            context.bounded(storage_timeout),
        );
        if expected == ObjectStorageError::Stopped(Stopped::Cancelled) {
            context.cancel();
        } else {
            tokio::time::advance(Duration::from_secs(2)).await;
        }
        joined(download.timer.as_mut().unwrap()).await;
        tokio::time::timeout(Duration::from_secs(1), body_dropped)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(admission.available_permits(), 1);
        assert_eq!(download.next_chunk().await, Err(expected));
        drop(download);
        let rendered = handle.render();
        assert!(rendered.contains(
            r#"object_storage_operation_duration_seconds_count{operation="get",outcome="unavailable"} 1"#
        ), "{rendered}");
        assert!(!rendered.contains("cancelled"), "{rendered}");

        let failures = records.matching("event", "object_storage_operation_failed");
        assert_eq!(failures.len(), index + 1);
        let event = &failures[index];
        if let Some(source) = timeout_source {
            let field = format!(" object_storage.timeout_source={source}");
            assert!(event.contains(&field), "{event}");
            assert!(event.contains(" error.type=timeout"), "{event}");
            assert_eq!(records.matching("span", &field).len(), 1);
        } else {
            assert!(event.contains(" error.type=cancelled"), "{event}");
            assert!(
                !event.contains(" object_storage.timeout_source="),
                "{event}"
            );
        }
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
            context.bounded(Duration::from_mins(1)),
        );
        let timer = download.timer.as_ref().unwrap().abort_handle();
        assert_eq!(
            download.next_chunk().await,
            Err(ObjectStorageError::Stopped(Stopped::Deadline))
        );
        assert_eq!(admission.available_permits(), 1);
        wait_for_timer(&timer).await;
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

fn open(body: LifecycleBody, size: u64, timeout: Duration) -> (Download, Arc<Semaphore>) {
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
            local_context(timeout),
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

#[tokio::test]
async fn download_debug_hides_metadata_and_held_payload() {
    let payload = Bytes::from_static(b"private-download-data");
    let size = payload.len() as u64;
    let (body, _destroyed) = body(Some(payload));
    let (mut download, _slots) = open(body, size, Duration::from_mins(1));
    download.metadata.e_tag = Some("private-object-tag".into());
    let mut context = Context::from_waker(Waker::noop());
    assert!(
        std::pin::pin!(download.next_chunk())
            .poll(&mut context)
            .is_pending()
    );
    let rendered = format!("{download:?}");
    assert!(!rendered.contains("private-download-data"), "{rendered}");
    assert!(!rendered.contains("private-object-tag"), "{rendered}");
}

async fn joined(timer: &mut JoinHandle<()>) {
    let result = tokio::time::timeout(Duration::from_secs(1), timer)
        .await
        .unwrap();
    assert!(result.is_ok() || result.unwrap_err().is_cancelled());
}

async fn wait_for_timer(completion: &AbortHandle) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while !completion.is_finished() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
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
    struct ConsumerWake(AtomicBool);
    impl std::task::Wake for ConsumerWake {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let freed = Arc::new(AtomicBool::new(false));
    let bytes = Bytes::from_owner(Tracked(Arc::clone(&freed)));
    let (body, destroyed) = body(Some(bytes));
    let timeout = Duration::from_millis(20);
    let (mut download, slots) = open(body, 4, timeout);
    let original = Arc::new(ConsumerWake(AtomicBool::new(false)));
    let replacement = Arc::new(ConsumerWake(AtomicBool::new(false)));
    for consumer in [&original, &replacement] {
        let waker = Waker::from(Arc::clone(consumer));
        assert!(
            download
                .poll_chunk(&mut Context::from_waker(&waker))
                .is_pending()
        );
    }
    tokio::time::advance(timeout).await;
    joined(download.timer.as_mut().unwrap()).await;
    assert!(!original.0.load(Ordering::SeqCst));
    assert!(replacement.0.load(Ordering::SeqCst));
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
        let (mut download, slots) = open(body, 0, Duration::from_millis(20));
        let completion = download.timer.as_ref().unwrap().abort_handle();
        let result = match consumer {
            0 => download.next_chunk().await.map(|_| ()),
            1 => download.frame().await.unwrap().map(|_| ()),
            _ => download.into_bytes(0).await.map(|_| ()),
        };
        assert_eq!(result, Err(ObjectStorageError::Unavailable));
        wait_for_timer(&completion).await;
        tokio::time::timeout(Duration::from_secs(1), destroyed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(slots.available_permits(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn unexpected_timer_abort_before_first_poll_fails_closed() {
    if !in_tracing_child("download::tests::unexpected_timer_abort_before_first_poll_fails_closed") {
        return;
    }
    let records = Records::default();
    let _subscriber = records.install_default();
    let (body, destroyed) = body(None);
    let (mut download, slots) = open(body, 4, Duration::from_mins(1));
    let timer = download.timer.as_mut().unwrap();
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
    let failures = records.matching("event", "object_storage_operation_failed");
    let [event] = failures.as_slice() else {
        panic!("one failure event: {failures:?}");
    };
    assert!(
        event.contains(" error.type=download_timer_exited"),
        "{event}"
    );
}

#[tokio::test(start_paused = true)]
async fn drop_and_provider_panic_release_custody_before_timer_runs() {
    for panic in [false, true] {
        let (mut body, mut destroyed) = body(None);
        body.panic = panic;
        let (mut download, slots) = open(body, 4, Duration::from_mins(1));
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
        } else {
            drop(download);
            assert!(destroyed.try_recv().is_ok());
            assert_eq!(slots.available_permits(), 1);
        }
        wait_for_timer(&completion).await;
    }
}

#[tokio::test]
async fn ready_empty_frames_cooperate_with_the_deadline_timer() {
    let (mut body, destroyed) = body(None);
    let polls = Arc::new(AtomicUsize::new(0));
    body.empty_polls = Some(Arc::clone(&polls));
    let (mut download, slots) = open(body, 4, Duration::from_millis(20));
    let timer = download.timer.as_ref().unwrap().abort_handle();
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
    wait_for_timer(&timer).await;
    tokio::time::timeout(Duration::from_secs(1), destroyed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(slots.available_permits(), 1);
}
#[tokio::test(start_paused = true)]
async fn partial_reads_spend_the_original_budget() {
    let (mut download, admission, eof) = download(Duration::from_secs(3));
    let timer = download.timer.as_ref().unwrap().abort_handle();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(download.next_chunk().await.unwrap().unwrap(), "abc");
    tokio::time::advance(Duration::from_secs(2)).await;
    let _ = eof.send(());
    assert_eq!(
        download.next_chunk().await,
        Err(ObjectStorageError::Unavailable)
    );
    assert_eq!(admission.available_permits(), 1);
    wait_for_timer(&timer).await;
}
