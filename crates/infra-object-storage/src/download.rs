use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use aws_sdk_s3::primitives::{ByteStream, ByteStreamError};
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use parking_lot::Mutex;
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::JoinHandle;

use crate::call::{CallContext, CallStop};
use crate::error::{Failure, LocalFailure};
use crate::observe::OperationGuard;
use crate::{ObjectMetadata, ObjectStorageError, error};

#[derive(derive_more::Debug)]
#[debug("Download {{ size: {}, .. }}", metadata.size)]
pub struct Download {
    metadata: ObjectMetadata,
    shared: Arc<Mutex<State>>,
    timer: JoinHandle<()>,
}

#[expect(
    clippy::large_enum_variant,
    reason = "State already lives in one Arc allocation; inline custody avoids another allocation"
)]
enum State {
    Open(Resources),
    Succeeded,
    Failed(ObjectStorageError),
}

struct Resources {
    waker: Option<Waker>,
    context: CallContext,
    body: ByteStream,
    remaining: u64,
    final_chunk: Option<Bytes>,
    guard: OperationGuard,
    _permit: OwnedSemaphorePermit,
}

impl Resources {
    fn unread_bytes(&self) -> u64 {
        self.remaining
            + self
                .final_chunk
                .as_ref()
                .map_or(0, |chunk| chunk.len() as u64)
    }
}

enum Outcome {
    Success,
    Failure(Failure),
    Cancelled,
}

struct Completion {
    resources: Resources,
    outcome: Outcome,
    discarded: Option<Result<Bytes, ByteStreamError>>,
}

impl Completion {
    fn finish(mut self) {
        match self.outcome {
            Outcome::Success => self.resources.guard.succeed(),
            Outcome::Failure(failure) => {
                self.resources.guard.fail(failure);
            }
            Outcome::Cancelled => self.resources.guard.cancel(),
        }
        let waker = self.resources.waker.take();
        drop(self.discarded);
        drop(self.resources);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

enum ReadStep {
    Buffered,
    Empty(Bytes),
    Chunk(Option<Bytes>),
}

type ReadPoll = Poll<Result<ReadStep, ObjectStorageError>>;

impl Resources {
    fn collection_buffer(&self, limit: usize) -> Result<Vec<u8>, Failure> {
        if let Some(stopped) = self.context.stopped() {
            return Err(Failure::stopped(stopped));
        }
        let Ok(unread) = usize::try_from(self.unread_bytes()) else {
            return Err(LocalFailure::TooLarge.into());
        };
        if unread > limit.min(isize::MAX as usize) {
            return Err(LocalFailure::TooLarge.into());
        }
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(unread)
            .map_err(|_| LocalFailure::DownloadAllocation)?;
        Ok(buffer)
    }
}

impl State {
    fn close(
        &mut self,
        outcome: Outcome,
        discarded: Option<Result<Bytes, ByteStreamError>>,
    ) -> Option<Completion> {
        if !matches!(self, State::Open(_)) {
            return None;
        }
        let terminal = match &outcome {
            Outcome::Success => State::Succeeded,
            Outcome::Failure(failure) => State::Failed(failure.error),
            Outcome::Cancelled => State::Failed(ObjectStorageError::Unavailable),
        };
        let State::Open(resources) = std::mem::replace(self, terminal) else {
            return None;
        };
        Some(Completion {
            resources,
            outcome,
            discarded,
        })
    }

    fn close_failed(
        &mut self,
        failure: Failure,
        discarded: Option<Result<Bytes, ByteStreamError>>,
    ) -> (ReadPoll, Option<Completion>) {
        (
            Poll::Ready(Err(failure.error)),
            self.close(Outcome::Failure(failure), discarded),
        )
    }

    fn unread_bytes(&self) -> Option<u64> {
        match self {
            State::Open(resources) => Some(resources.unread_bytes()),
            State::Succeeded => Some(0),
            State::Failed(_) => None,
        }
    }

    fn collection_buffer(
        &mut self,
        limit: usize,
    ) -> (Result<Vec<u8>, ObjectStorageError>, Option<Completion>) {
        let resources = match self {
            State::Open(resources) => resources,
            State::Succeeded => return (Ok(Vec::new()), None),
            State::Failed(error) => return (Err(*error), None),
        };
        match resources.collection_buffer(limit) {
            Ok(buffer) => (Ok(buffer), None),
            Err(failure) => (
                Err(failure.error),
                self.close(Outcome::Failure(failure), None),
            ),
        }
    }

    fn poll(&mut self, context: &mut Context<'_>) -> (ReadPoll, Option<Completion>) {
        let resources = match self {
            State::Open(resources) => resources,
            State::Succeeded => return (Poll::Ready(Ok(ReadStep::Chunk(None))), None),
            State::Failed(error) => return (Poll::Ready(Err(*error)), None),
        };
        if let Some(stopped) = resources.context.stopped() {
            return self.close_failed(Failure::stopped(stopped), None);
        }
        resources.waker = Some(context.waker().clone());
        let Poll::Ready(coop) = tokio::task::coop::poll_proceed(context) else {
            return (Poll::Pending, None);
        };
        let polled = Pin::new(&mut resources.body).poll_next(context);
        if polled.is_ready() {
            coop.made_progress();
        }
        if let Some(stopped) = resources.context.stopped() {
            let discarded = match polled {
                Poll::Ready(item) => item,
                Poll::Pending => None,
            };
            return self.close_failed(Failure::stopped(stopped), discarded);
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
                    None => self.close_failed(LocalFailure::ContentLength.into(), Some(Ok(chunk))),
                }
            }
            Poll::Ready(Some(Err(error))) => {
                self.close_failed(error::from_download_body(&error), Some(Err(error)))
            }
            Poll::Ready(None) if resources.remaining == 0 => {
                let chunk = resources.final_chunk.take();
                (
                    Poll::Ready(Ok(ReadStep::Chunk(chunk))),
                    self.close(Outcome::Success, None),
                )
            }
            Poll::Ready(None) => self.close_failed(LocalFailure::ContentLength.into(), None),
        }
    }
}

enum Termination {
    Stopped(CallStop),
    Fallback(Outcome),
}

fn terminate(shared: &Mutex<State>, termination: Termination) {
    let completion = {
        let mut state = shared.lock();
        let State::Open(resources) = &*state else {
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
        state.close(outcome, None)
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
        context: CallContext,
    ) -> Self {
        let shared = Arc::new(Mutex::new(State::Open(Resources {
            waker: None,
            context: context.clone(),
            body,
            remaining: metadata.size,
            final_chunk: None,
            guard,
            _permit: permit,
        })));
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
        let timer = tokio::spawn(async move {
            let stopped = context.wait_stopped().await;
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
            let polled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.shared.lock().poll(context)
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

    pub async fn into_bytes(mut self, limit: usize) -> Result<Bytes, ObjectStorageError> {
        let (buffer, completion) = self.shared.lock().collection_buffer(limit);
        if let Some(completion) = completion {
            completion.finish();
            self.timer.abort();
        }
        let mut buffer = buffer?;
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
        matches!(*self.shared.lock(), State::Succeeded)
    }

    fn size_hint(&self) -> SizeHint {
        self.shared
            .lock()
            .unread_bytes()
            .map_or_else(SizeHint::default, SizeHint::with_exact)
    }
}

#[cfg(test)]
mod tests;
