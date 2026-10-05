//! An open download: the object's body, read to its confirmed end.

use std::pin::Pin;
use std::task::{Context, Poll, ready};

use aws_sdk_s3::primitives::ByteStream;
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use tokio::sync::OwnedSemaphorePermit;

use crate::observe::OperationGuard;
use crate::{ObjectMetadata, ObjectStorageError, error};

/// An open download. Dropping it releases the admission slot and owned SDK
/// body; the operation is recorded as `cancelled` unless it already ended.
/// Terminal failure also releases that body before returning the error, even
/// if the failed download is retained. Neither guarantees socket closure or
/// completion of SDK tasks.
///
/// It is an [`http_body::Body`] of exactly [`ObjectMetadata::size`] bytes,
/// so it can be returned as a response body. The slot is then held for as
/// long as the reader takes: return it only to a reader that reads promptly,
/// and give any other reader [`Download::bytes`] or a presigned URL. The
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
    remaining: u64,
    body: ByteStream,
    /// The chunk that completed the object, until the body confirms its end.
    last: Option<Bytes>,
    state: DownloadState,
}

#[derive(Debug)]
enum DownloadState {
    Open(End),
    Succeeded,
    /// The failure returned again on every later call.
    Failed(ObjectStorageError),
}

/// What a download releases when it ends.
#[derive(Debug)]
struct End {
    guard: OperationGuard,
    _permit: OwnedSemaphorePermit,
}

impl Download {
    pub(crate) fn open(
        metadata: ObjectMetadata,
        body: ByteStream,
        guard: OperationGuard,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            remaining: metadata.size,
            metadata,
            body,
            last: None,
            state: DownloadState::Open(End {
                guard,
                _permit: permit,
            }),
        }
    }

    /// Metadata from the response headers.
    #[must_use]
    pub fn metadata(&self) -> &ObjectMetadata {
        &self.metadata
    }

    /// The next chunk, or `None` at the end. The download succeeds only at
    /// the end: the SDK validates a supported full-object checksum there when
    /// one is available (see [`Download`]).
    ///
    /// # Errors
    ///
    /// `Integrity` for a checksum mismatch or a body that differs from its
    /// headers; `Unavailable` for a transport failure or a stalled body.
    /// After an error every call returns the same error; after the end,
    /// `Ok(None)`.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, ObjectStorageError> {
        std::future::poll_fn(|context| self.poll_chunk(context)).await
    }

    fn poll_chunk(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Option<Bytes>, ObjectStorageError>> {
        loop {
            let end = match &mut self.state {
                DownloadState::Open(end) => end,
                DownloadState::Succeeded => return Poll::Ready(Ok(self.last.take())),
                DownloadState::Failed(error) => return Poll::Ready(Err(*error)),
            };
            let failure = match ready!(Pin::new(&mut self.body).poll_next(context)) {
                Some(Ok(chunk)) if chunk.is_empty() => continue,
                Some(Ok(chunk)) => match self.remaining.checked_sub(chunk.len() as u64) {
                    Some(0) => {
                        self.remaining = 0;
                        self.last = Some(chunk);
                        continue;
                    }
                    Some(remaining) => {
                        self.remaining = remaining;
                        return Poll::Ready(Ok(Some(chunk)));
                    }
                    None => (ObjectStorageError::Integrity, "content_length"),
                },
                Some(Err(error)) if error::is_checksum_mismatch(&error) => {
                    (ObjectStorageError::Integrity, "checksum")
                }
                Some(Err(_)) => (ObjectStorageError::Unavailable, "body"),
                None if self.remaining == 0 => {
                    end.guard.succeed();
                    self.state = DownloadState::Succeeded;
                    continue;
                }
                None => (ObjectStorageError::Integrity, "content_length"),
            };
            let error = end.guard.fail(failure.0, failure.1);
            self.last = None;
            self.body = ByteStream::default();
            self.state = DownloadState::Failed(error);
            return Poll::Ready(Err(error));
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
        matches!(self.state, DownloadState::Succeeded) && self.last.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        let last = self.last.as_ref().map_or(0, |chunk| chunk.len() as u64);
        SizeHint::with_exact(self.remaining + last)
    }
}

#[cfg(test)]
mod tests {
    // The fixture is synchronous and every unexpected frame or admission is a test failure.
    #![allow(clippy::unwrap_used, clippy::panic)]

    use std::future::Future;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Waker;

    use http_body::Body;
    use http_body_util::BodyExt;
    use tokio::sync::Semaphore;

    use super::*;
    use crate::observe::{Histograms, Operation};

    struct ObservedBody {
        chunk: Option<Bytes>,
        error: Option<Box<dyn std::error::Error + Send + Sync>>,
        drops: Arc<AtomicUsize>,
    }

    impl Body for ObservedBody {
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

    #[test]
    fn retained_failed_download_releases_body_and_finishes_once() {
        type BodyError = Box<dyn std::error::Error + Send + Sync>;
        for frames in [false, true] {
            let cases: [(&[u8], Option<BodyError>, _, _); 4] = [
                (
                    b"data",
                    Some(std::io::Error::other("broken body").into()),
                    ObjectStorageError::Unavailable,
                    0,
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
                    0,
                ),
                (b"ab", None, ObjectStorageError::Integrity, 2),
                (b"extra", None, ObjectStorageError::Integrity, 4),
            ];
            for (chunk, error, expected, remaining) in cases {
                let recorder =
                    metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
                let handle = recorder.handle();
                metrics::with_local_recorder(&recorder, || {
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
                        OperationGuard::start(
                            Arc::new(Histograms::default()),
                            Operation::Get,
                            None,
                        ),
                        Arc::clone(&admission).try_acquire_owned().unwrap(),
                    );
                    let mut context = Context::from_waker(Waker::noop());
                    assert_eq!(drops.load(Ordering::SeqCst), 0);
                    assert_eq!(admission.available_permits(), 0);
                    if remaining == 2 {
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
                    assert_eq!(download.size_hint().exact(), Some(remaining));
                    assert!(!download.is_end_stream());
                    assert_eq!(
                        read(&mut download, &mut context, frames),
                        Poll::Ready(Err(expected))
                    );
                    assert_eq!(
                        std::pin::pin!(download.bytes()).poll(&mut context),
                        Poll::Ready(Err(expected))
                    );
                    assert_eq!(drops.load(Ordering::SeqCst), 1);
                });
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
}
