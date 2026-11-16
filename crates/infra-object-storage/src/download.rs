use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

use aws_sdk_s3::primitives::{ByteStream, ByteStreamError};
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use operation_context::{OperationContext, Stopped};
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::JoinHandle;

use crate::error::{Failure, LocalFailure};
use crate::observe::OperationGuard;
use crate::{ObjectMetadata, ObjectStorageError, error};

/// An [`http_body::Body`] of exactly [`ObjectMetadata::size`] bytes. Dropping it
/// releases the slot and SDK body and records `cancelled`, or `unavailable` if
/// the operation context already stopped; neither claims socket closure or
/// completion of SDK-owned work. A terminal failure releases the body before the
/// error returns, even if the `Download` is retained. A weak timer releases
/// unpolled retained custody at the original cutoff; a presigned URL suits a
/// remote slow reader.
///
/// The final chunk is withheld until EOF is confirmed and the SDK finished any
/// supported full-object checksum validation. The SDK permits absent checksums
/// and skips composite (`-N`) or invalid-base64 ones, so success does not attest
/// validation; a feature needing end-to-end integrity checks its own digest.
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

/// Terminal code takes the whole cell under the short lock, then destroys and
/// observes it after unlocking.
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
    /// A ready SDK poll may have produced bytes or an error at the cutoff;
    /// discarded after the lock is released.
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
    // `Download::poll_chunk` catches a provider-body panic and calls `terminate` on
    // this cell; keep it usable so a retained `Download` still returns its error.
    shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A provider poll can advance custody without returning a caller payload.
enum ReadStep {
    Buffered,
    Empty(Bytes),
    Chunk(Option<Bytes>),
}

type ReadPoll = Poll<Result<ReadStep, ObjectStorageError>>;

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

    /// Includes a withheld final chunk; `None` once the download failed.
    fn unread_bytes(&self) -> Option<u64> {
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
        // `poll_chunk` loops over ready chunks without returning, so it spends the
        // cooperative budget by hand.
        let Poll::Ready(coop) = tokio::task::coop::poll_proceed(context) else {
            return (Poll::Pending, None);
        };
        let polled = Pin::new(&mut resources.body).poll_next(context);
        if polled.is_ready() {
            coop.made_progress();
        }
        // Strict GET finality: a poll that crossed the cutoff yields no chunk, error
        // mapping, or EOF.
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
            Poll::Ready(Some(Err(error))) => {
                self.close_failed_with_item(error::from_download_body(&error), Err(error))
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

enum Termination {
    Stopped(Stopped),
    Fallback(Outcome),
}

fn terminate(shared: &Mutex<State>, termination: Termination) {
    let completion = {
        let mut state = lock(shared);
        let Status::Open(resources) = &state.status else {
            return;
        };
        let outcome = match termination {
            Termination::Stopped(stopped) => Outcome::Failure(Failure::stopped(stopped)),
            Termination::Fallback(outcome) => {
                resources.context.stopped().map_or(outcome, |stopped| {
                    Outcome::Failure(Failure::stopped(stopped))
                })
            }
        };
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
        // Captured before the timer first polls so an unexpected exit frees custody.
        let exit = std::mem::DropGuard::new(Arc::downgrade(&shared), |weak| {
            if let Some(shared) = weak.upgrade() {
                terminate(
                    &shared,
                    Termination::Fallback(Outcome::Failure(
                        LocalFailure::DownloadTimerExited.into(),
                    )),
                );
            }
        });
        let timer_context = context;
        let timer = tokio::spawn(async move {
            let stopped = timer_context.wait_stopped().await;
            if let Some(shared) = exit.upgrade() {
                terminate(&shared, Termination::Stopped(stopped));
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

    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, ObjectStorageError> {
        std::future::poll_fn(|context| self.poll_chunk(context)).await
    }

    fn poll_chunk(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Option<Bytes>, ObjectStorageError>> {
        loop {
            // A caller that catches the panic may keep this `Download`; slot and body must
            // not wait for its drop.
            let polled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                lock(&self.shared).poll(context)
            }));
            let (result, completion) = match polled {
                Ok(result) => result,
                Err(panic) => {
                    terminate(&self.shared, Termination::Fallback(Outcome::Cancelled));
                    self.timer.abort();
                    std::panic::resume_unwind(panic);
                }
            };
            if let Some(completion) = completion {
                completion.finish();
                self.timer.abort();
            }
            match std::task::ready!(result)? {
                ReadStep::Buffered => {}
                ReadStep::Empty(chunk) => drop(chunk),
                ReadStep::Chunk(chunk) => return Poll::Ready(Ok(chunk)),
            }
        }
    }

    /// Returned bytes outlive admission and need a budget at their consumer.
    pub async fn into_bytes(mut self) -> Result<Bytes, ObjectStorageError> {
        let unread_bytes = lock(&self.shared).unread_bytes().unwrap_or(0);
        let mut buffer = Vec::with_capacity(usize::try_from(unread_bytes).unwrap_or(0));
        while let Some(chunk) = self.next_chunk().await? {
            buffer.extend_from_slice(&chunk);
        }
        Ok(Bytes::from(buffer))
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        terminate(&self.shared, Termination::Fallback(Outcome::Cancelled));
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
        // Hyper may skip an exact-zero body, so a failed download reports no exact size.
        lock(&self.shared)
            .unread_bytes()
            .map_or_else(SizeHint::default, SizeHint::with_exact)
    }
}

#[cfg(test)]
mod tests;
