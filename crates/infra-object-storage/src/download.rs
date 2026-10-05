//! An open download: the object's body, read to its confirmed end.

use std::future::Future as _;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

use aws_sdk_s3::primitives::ByteStream;
use bytes::Bytes;
use futures_util::FutureExt as _;
use http_body::{Frame, SizeHint};
use operation_context::OperationContext;
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::JoinHandle;

use crate::observe::OperationGuard;
use crate::{ObjectMetadata, ObjectStorageError, error};

/// An open download. Dropping it releases the admission slot and the
/// connection; the operation is recorded as `cancelled` unless the body
/// already ended.
///
/// It is an [`http_body::Body`] of exactly [`ObjectMetadata::size`] bytes,
/// so it can be returned as a response body. The slot is then held for as
/// long as the remaining operation budget permits: return it only to a reader that reads promptly,
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
    context: OperationContext,
    state: Arc<Mutex<State>>,
    timer: Option<JoinHandle<()>>,
}

#[derive(Debug)]
struct State {
    resources: Option<Resources>,
    failure: Option<ObjectStorageError>,
    waker: Option<Waker>,
}

/// All live custody leaves the cell in one transition. Destruction and
/// observation happen only after releasing its short synchronous lock.
#[derive(Debug)]
struct Resources {
    body: ByteStream,
    remaining: u64,
    last: Option<Bytes>,
    guard: OperationGuard,
    _permit: OwnedSemaphorePermit,
}

impl Resources {
    fn finish(mut self, failure: Option<(ObjectStorageError, &'static str)>) -> Option<Bytes> {
        if let Some((error, error_type)) = failure {
            self.guard.fail(error, error_type);
            None
        } else {
            self.guard.succeed();
            self.last.take()
        }
    }
}

enum Progress {
    Pending,
    Continue,
    Chunk(Bytes),
    Complete,
    Failed(ObjectStorageError, &'static str),
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn terminate(state: &Mutex<State>, error_type: &'static str) {
    let (resources, waker) = {
        let mut state = lock(state);
        let resources = state.resources.take();
        if resources.is_some() {
            state.failure = Some(ObjectStorageError::Unavailable);
        }
        (resources, state.waker.take())
    };
    if let Some(resources) = resources {
        resources.finish(Some((ObjectStorageError::Unavailable, error_type)));
    }
    if let Some(waker) = waker {
        waker.wake();
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
        let state = Arc::new(Mutex::new(State {
            resources: Some(Resources {
                remaining: metadata.size,
                body,
                last: None,
                guard,
                _permit: permit,
            }),
            failure: None,
            waker: None,
        }));
        let weak = Arc::downgrade(&state);
        let lifetime = context.clone();
        let timer = tokio::spawn(async move {
            // Catch inside the owner so even an unpolled retained Download
            // observes a timer panic and releases its resources.
            let result = AssertUnwindSafe(async {
                let stopped = lifetime.wait_stopped().await;
                if let Some(state) = weak.upgrade() {
                    terminate(&state, crate::stop_type(stopped));
                }
            })
            .catch_unwind()
            .await;
            if result.is_err()
                && let Some(state) = weak.upgrade()
            {
                terminate(&state, "body");
            }
        });
        Self {
            metadata,
            context,
            state,
            timer: Some(timer),
        }
    }

    /// Metadata from the response headers.
    #[must_use]
    pub fn metadata(&self) -> &ObjectMetadata {
        &self.metadata
    }

    fn stop_timer(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.abort();
        }
    }

    fn reap_timer(&mut self, cx: &mut Context<'_>) {
        if let Some(timer) = self.timer.as_mut()
            && let Poll::Ready(result) = Pin::new(timer).poll(cx)
        {
            self.timer = None;
            if result.is_err() {
                terminate(&self.state, "body");
            }
        }
    }

    /// The next chunk, or `None` at the confirmed end. The final chunk is
    /// withheld until length and any supported full-object checksum pass.
    ///
    /// # Errors
    ///
    /// `Integrity` for a checksum mismatch or length disagreement;
    /// `Unavailable` for transport failure, deadline or cancellation.
    /// Every later call repeats the failure; successful EOF returns `Ok(None)`.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, ObjectStorageError> {
        std::future::poll_fn(|context| self.poll_chunk(context)).await
    }

    fn poll_chunk(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Bytes>, ObjectStorageError>> {
        self.reap_timer(cx);
        loop {
            // Clone/drop user wakers outside the lock, including replacements.
            let waker = cx.waker().clone();
            let mut state = lock(&self.state);
            if state.resources.is_none() {
                let result = state.failure.map_or(Ok(None), Err);
                drop(state);
                self.stop_timer();
                return Poll::Ready(result);
            }
            if let Some(stopped) = self.context.stopped() {
                drop(state);
                terminate(&self.state, crate::stop_type(stopped));
                self.stop_timer();
                return Poll::Ready(Err(ObjectStorageError::Unavailable));
            }
            let old_waker = state.waker.replace(waker);
            let Some(resources) = state.resources.as_mut() else {
                drop(state);
                drop(old_waker);
                return Poll::Ready(Ok(None));
            };
            let polled = std::panic::catch_unwind(AssertUnwindSafe(|| {
                Pin::new(&mut resources.body).poll_next(cx)
            }));
            let mut previous = None;
            // Borrow poll output so its bytes/error/panic payload are destroyed
            // outside the lock, even when expiry overrides a ready result.
            let progress = if let Some(stopped) = self.context.stopped() {
                Progress::Failed(ObjectStorageError::Unavailable, crate::stop_type(stopped))
            } else {
                match &polled {
                    Ok(Poll::Pending) => Progress::Pending,
                    Ok(Poll::Ready(Some(Ok(chunk)))) if chunk.is_empty() => Progress::Continue,
                    Ok(Poll::Ready(Some(Ok(chunk)))) => match resources
                        .remaining
                        .checked_sub(chunk.len() as u64)
                    {
                        Some(0) => {
                            resources.remaining = 0;
                            previous = resources.last.replace(chunk.clone());
                            Progress::Continue
                        }
                        Some(remaining) => {
                            resources.remaining = remaining;
                            Progress::Chunk(chunk.clone())
                        }
                        None => Progress::Failed(ObjectStorageError::Integrity, "content_length"),
                    },
                    Ok(Poll::Ready(Some(Err(error)))) if error::is_checksum_mismatch(error) => {
                        Progress::Failed(ObjectStorageError::Integrity, "checksum")
                    }
                    Ok(Poll::Ready(Some(Err(_)))) | Err(_) => {
                        Progress::Failed(ObjectStorageError::Unavailable, "body")
                    }
                    Ok(Poll::Ready(None)) if resources.remaining == 0 => Progress::Complete,
                    Ok(Poll::Ready(None)) => {
                        Progress::Failed(ObjectStorageError::Integrity, "content_length")
                    }
                }
            };
            let failure = match &progress {
                Progress::Failed(error, error_type) => Some((*error, *error_type)),
                _ => None,
            };
            let terminal = matches!(progress, Progress::Complete | Progress::Failed(..));
            let resources = if terminal {
                state.resources.take()
            } else {
                None
            };
            if terminal {
                state.failure = failure.map(|(error, _)| error);
            }
            let waker = if terminal { state.waker.take() } else { None };
            drop(state);
            drop(old_waker);
            drop(waker);
            drop(polled);
            drop(previous);
            if terminal {
                self.stop_timer();
            }
            let last = resources.and_then(|resources| resources.finish(failure));
            match progress {
                Progress::Pending => return Poll::Pending,
                Progress::Continue => {}
                Progress::Chunk(chunk) => return Poll::Ready(Ok(Some(chunk))),
                Progress::Complete => return Poll::Ready(Ok(last)),
                Progress::Failed(error, _) => return Poll::Ready(Err(error)),
            }
        }
    }

    /// Consume and collect the remaining body under its original deadline.
    /// Returned bytes outlive admission and need a budget at their consumer.
    ///
    /// # Errors
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
        let (resources, waker) = {
            let mut state = lock(&self.state);
            (state.resources.take(), state.waker.take())
        };
        self.stop_timer();
        drop(resources);
        drop(waker);
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
        let state = lock(&self.state);
        state.resources.is_none() && state.failure.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        let state = lock(&self.state);
        state.resources.as_ref().map_or_else(
            || {
                if state.failure.is_none() {
                    SizeHint::with_exact(0)
                } else {
                    SizeHint::default()
                }
            },
            |resources| {
                SizeHint::with_exact(
                    resources.remaining
                        + resources
                            .last
                            .as_ref()
                            .map_or(0, |chunk| chunk.len() as u64),
                )
            },
        )
    }
}
