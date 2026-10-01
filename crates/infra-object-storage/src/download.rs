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
/// so it can be returned as a response body. The chunk that completes the
/// object is released only after the provider's body has ended and its
/// checksum, when one came back, has been validated: a reader that stops at
/// the declared length never holds a complete object that failed the check.
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
    /// the end: the SDK validates a returned full-object checksum there.
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
    /// `max_object_bytes` bounds the collection.
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
