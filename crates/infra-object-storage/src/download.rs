//! An open download: the object's body, read to its confirmed end.

use std::pin::Pin;
use std::task::{Context, Poll, ready};

use aws_sdk_s3::primitives::ByteStream;
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use tokio::sync::OwnedSemaphorePermit;

use crate::observe::OperationGuard;
use crate::{ObjectMetadata, ObjectStorageError, error};

/// An open download. Dropping it releases the admission slot and the
/// connection; the operation is recorded as `cancelled` unless the body
/// already ended.
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
        let last = self.last.as_ref().map_or(0, |chunk| chunk.len() as u64);
        let mut buffer = Vec::with_capacity(usize::try_from(self.remaining + last).unwrap_or(0));
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
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::collections::VecDeque;
    use std::future::Future as _;
    use std::sync::Arc;
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
            ready!(Pin::new(&mut self.eof).poll(context)).unwrap();
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
            Download::open(metadata, ByteStream::from_body_1_x(body), guard, permit),
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
            // The uniquely owned Bytes returns its Vec backing without copying.
            // This observes reservation through the public collection result.
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
}
