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

use crate::error::{Failure, LocalFailure};
use crate::observe::OperationGuard;
use crate::{ObjectMetadata, ObjectStorageError, error};

/// An open download. Dropping it releases the admission slot and owned SDK
/// body; the operation is recorded as `cancelled` unless it already ended or
/// its operation context has stopped, in which case it records `unavailable`.
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
    timer: JoinHandle<()>,
}

#[derive(Debug)]
struct State {
    waker: Option<Waker>,
    status: Status,
}

#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "State already lives in one Arc allocation; inline custody avoids another allocation"
)]
enum Status {
    Open(Resources),
    Succeeded,
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
    final_chunk: Option<Bytes>,
    guard: OperationGuard,
    _permit: OwnedSemaphorePermit,
}

enum Outcome {
    Success,
    Failure(Failure),
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
        match &self.outcome {
            Outcome::Success => self.resources.guard.succeed(),
            Outcome::Failure(failure) => {
                self.resources.guard.fail(failure);
            }
            Outcome::Cancelled => self.resources.guard.cancel(),
        }
        drop(self.discarded);
        drop(self.resources);
        if let Some(waker) = self.waker {
            waker.wake();
        }
    }
}

fn lock(shared: &Mutex<State>) -> MutexGuard<'_, State> {
    // `Download::poll_chunk` catches a provider-body panic and then calls
    // `terminate` on this same cell; a retained `Download` must still return
    // its terminal error. Keep the cell usable instead of panicking again.
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

/// The `close*` methods only move a still-open state to its terminal one
/// under the lock; the returned [`Completion`] records the outcome and
/// releases the resources after the lock is gone.
impl State {
    fn close(&mut self, outcome: Outcome) -> Option<Completion> {
        if !matches!(self.status, Status::Open(_)) {
            return None;
        }
        let terminal = match &outcome {
            Outcome::Success => Status::Succeeded,
            Outcome::Failure(failure) => Status::Failed(failure.error),
            Outcome::Cancelled => Status::Failed(ObjectStorageError::Unavailable),
        };
        let Status::Open(resources) = std::mem::replace(&mut self.status, terminal) else {
            return None;
        };
        Some(Completion {
            resources,
            outcome,
            waker: self.waker.take(),
            discarded: None,
        })
    }

    fn close_failed(&mut self, failure: Failure) -> (ReadPoll, Option<Completion>) {
        (
            Poll::Ready(Err(failure.error)),
            self.close(Outcome::Failure(failure)),
        )
    }

    fn close_failed_with_item(
        &mut self,
        failure: Failure,
        item: Result<Bytes, ByteStreamError>,
    ) -> (ReadPoll, Option<Completion>) {
        let (result, mut completion) = self.close_failed(failure);
        if let Some(completion) = &mut completion {
            completion.discarded = Some(item);
        }
        (result, completion)
    }

    /// Bytes not yet returned to the reader, including a withheld final
    /// chunk; `None` once the download failed.
    fn unread(&self) -> Option<u64> {
        match &self.status {
            Status::Open(resources) => Some(
                resources.remaining
                    + resources
                        .final_chunk
                        .as_ref()
                        .map_or(0, |chunk| chunk.len() as u64),
            ),
            Status::Succeeded => Some(0),
            Status::Failed(_) => None,
        }
    }

    fn poll(&mut self, context: &mut Context<'_>) -> (ReadPoll, Option<Completion>) {
        let resources = match &mut self.status {
            Status::Open(resources) => resources,
            Status::Succeeded => return (Poll::Ready(Ok(ReadStep::Chunk(None))), None),
            Status::Failed(error) => return (Poll::Ready(Err(*error)), None),
        };
        if let Some(stopped) = resources.context.stopped() {
            return self.close_failed(Failure::stopped(stopped));
        }
        self.waker = Some(context.waker().clone());
        // `poll_chunk` loops over ready empty and buffered chunks without
        // returning, so it spends the task's cooperative budget by hand.
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
            let (result, mut completion) = self.close_failed(Failure::stopped(stopped));
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
            Poll::Ready(Some(Ok(chunk))) => {
                match resources.remaining.checked_sub(chunk.len() as u64) {
                    Some(0) => {
                        resources.remaining = 0;
                        resources.final_chunk = Some(chunk);
                        (Poll::Ready(Ok(ReadStep::Buffered)), None)
                    }
                    Some(remaining) => {
                        resources.remaining = remaining;
                        (Poll::Ready(Ok(ReadStep::Chunk(Some(chunk)))), None)
                    }
                    None => {
                        self.close_failed_with_item(LocalFailure::ContentLength.into(), Ok(chunk))
                    }
                }
            }
            Poll::Ready(Some(Err(error))) if error::is_checksum_mismatch(&error) => {
                self.close_failed_with_item(LocalFailure::ChecksumMismatch.into(), Err(error))
            }
            Poll::Ready(Some(Err(error))) => {
                self.close_failed_with_item(LocalFailure::DownloadBody.into(), Err(error))
            }
            Poll::Ready(None) if resources.remaining == 0 => {
                let chunk = resources.final_chunk.take();
                (
                    Poll::Ready(Ok(ReadStep::Chunk(chunk))),
                    self.close(Outcome::Success),
                )
            }
            Poll::Ready(None) => self.close_failed(LocalFailure::ContentLength.into()),
        }
    }
}

/// Captured before the spawned timer can first poll. An unexpected timer exit
/// still frees retained body custody instead of depending on another read.
struct TimerExit(Weak<Mutex<State>>);

impl Drop for TimerExit {
    fn drop(&mut self) {
        if let Some(shared) = self.0.upgrade() {
            terminate(&shared, Outcome::Failure(LocalFailure::DownloadBody.into()));
        }
    }
}

fn terminate(shared: &Mutex<State>, outcome: Outcome) {
    let completion = {
        let mut state = lock(shared);
        let stopped = match &state.status {
            Status::Open(resources) => resources.context.stopped(),
            Status::Succeeded | Status::Failed(_) => return,
        };
        let outcome = stopped.map_or(outcome, |stopped| {
            Outcome::Failure(Failure::stopped(stopped))
        });
        state.close(outcome)
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
            status: Status::Open(Resources {
                context: context.clone(),
                body,
                remaining: metadata.size,
                final_chunk: None,
                guard,
                _permit: permit,
            }),
        }));
        let exit = TimerExit(Arc::downgrade(&shared));
        let timer_context = context;
        let timer = tokio::spawn(async move {
            let stopped = timer_context.wait_stopped().await;
            if let Some(shared) = exit.0.upgrade() {
                terminate(&shared, Outcome::Failure(Failure::stopped(stopped)));
            }
        });
        Self {
            metadata,
            shared,
            timer,
        }
    }

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
        loop {
            // `Drop` would release custody too, but a caller that catches the
            // panic may keep this `Download`; the slot and the body must not
            // wait for it to be dropped.
            let polled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                lock(&self.shared).poll(context)
            }));
            let (result, completion) = match polled {
                Ok(result) => result,
                Err(panic) => {
                    terminate(&self.shared, Outcome::Cancelled);
                    self.timer.abort();
                    std::panic::resume_unwind(panic);
                }
            };
            if let Some(completion) = completion {
                completion.finish();
                self.timer.abort();
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

    /// Consume and collect unread bytes under the original context. Returned
    /// bytes outlive admission and need a budget at their consumer.
    ///
    /// # Errors
    ///
    /// As [`Self::next_chunk`].
    pub async fn into_bytes(mut self) -> Result<Bytes, ObjectStorageError> {
        let unread = lock(&self.shared).unread().unwrap_or(0);
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
        self.timer.abort();
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
        matches!(lock(&self.shared).status, Status::Succeeded)
    }

    fn size_hint(&self) -> SizeHint {
        // Hyper may skip an exact-zero body, so a failed download reports no
        // exact size and keeps its error visible.
        lock(&self.shared)
            .unread()
            .map_or_else(SizeHint::default, SizeHint::with_exact)
    }
}

#[cfg(test)]
mod tests;
