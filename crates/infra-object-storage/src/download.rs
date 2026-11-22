use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use aws_sdk_s3::primitives::{ByteStream, ByteStreamError};
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use operation_context::{BoundedContext, BoundedStop};
use parking_lot::Mutex;
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::JoinHandle;

use crate::byte_progress::ByteProgress;
use crate::call::check_active;
use crate::error::{Failure, LocalFailure};
use crate::observe::OperationGuard;
use crate::{ObjectMetadata, ObjectStorageError, error};

#[derive(derive_more::Debug)]
#[debug("Download {{ size: {}, .. }}", metadata.size)]
pub struct Download {
    metadata: ObjectMetadata,
    shared: Arc<Mutex<State>>,
    timer: Option<JoinHandle<()>>,
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
    context: BoundedContext,
    body: ByteStream,
    progress: ByteProgress,
    guard: OperationGuard,
    permit: OwnedSemaphorePermit,
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
    fn finish(self) {
        let Self {
            resources,
            outcome,
            discarded,
        } = self;
        let Resources {
            waker,
            context,
            body,
            progress,
            guard,
            permit,
        } = resources;
        match outcome {
            Outcome::Success => guard.succeed(),
            Outcome::Failure(failure) => {
                guard.fail(&failure);
            }
            Outcome::Cancelled => drop(guard),
        }
        drop(discarded);
        drop(body);
        drop(progress);
        drop(context);
        drop(permit);
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
        check_active(&self.context)?;
        let Ok(unread) = usize::try_from(self.progress.remaining()) else {
            return Err(Failure::too_large());
        };
        if unread > limit.min(isize::MAX as usize) {
            return Err(Failure::too_large());
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
        let terminal = match &outcome {
            Outcome::Success => State::Succeeded,
            Outcome::Failure(failure) => State::Failed(failure.error),
            Outcome::Cancelled => State::Failed(ObjectStorageError::Unavailable),
        };
        match std::mem::replace(self, terminal) {
            State::Open(resources) => Some(Completion {
                resources,
                outcome,
                discarded,
            }),
            other => {
                *self = other;
                None
            }
        }
    }

    fn fail_poll(
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
            State::Open(resources) => Some(resources.progress.remaining()),
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
            return self.fail_poll(Failure::not_applied_stop(stopped), None);
        }
        match &mut resources.waker {
            Some(waker) => waker.clone_from(context.waker()),
            slot => *slot = Some(context.waker().clone()),
        }
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
            return self.fail_poll(Failure::not_applied_stop(stopped), discarded);
        }
        match polled {
            Poll::Pending => (Poll::Pending, None),
            Poll::Ready(Some(Ok(chunk))) if chunk.is_empty() => {
                (Poll::Ready(Ok(ReadStep::Empty(chunk))), None)
            }
            Poll::Ready(Some(Ok(chunk))) => match resources.progress.accept(chunk) {
                Ok(Some(chunk)) => (Poll::Ready(Ok(ReadStep::Chunk(Some(chunk)))), None),
                Ok(None) => (Poll::Ready(Ok(ReadStep::Buffered)), None),
                Err(chunk) => self.fail_poll(LocalFailure::ContentLength.into(), Some(Ok(chunk))),
            },
            Poll::Ready(Some(Err(error))) => {
                self.fail_poll(error::from_download_body(&error), Some(Err(error)))
            }
            Poll::Ready(None) => match resources.progress.finish() {
                Ok(chunk) => (
                    Poll::Ready(Ok(ReadStep::Chunk(chunk))),
                    self.close(Outcome::Success, None),
                ),
                Err(()) => self.fail_poll(LocalFailure::ContentLength.into(), None),
            },
        }
    }

    fn terminate(&mut self, termination: Termination) -> Option<Completion> {
        let State::Open(resources) = self else {
            return None;
        };
        let outcome = match termination {
            Termination::Stopped(stopped) => Outcome::Failure(Failure::not_applied_stop(stopped)),
            Termination::Fallback(outcome) => {
                resources.context.stopped().map_or(outcome, |stopped| {
                    Outcome::Failure(Failure::not_applied_stop(stopped))
                })
            }
        };
        self.close(outcome, None)
    }
}

enum Termination {
    Stopped(BoundedStop),
    Fallback(Outcome),
}

fn terminate_from_timer(shared: &Mutex<State>, termination: Termination) {
    let completion = shared.lock().terminate(termination);
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
        context: BoundedContext,
    ) -> Self {
        let shared = Arc::new(Mutex::new(State::Open(Resources {
            waker: None,
            context: context.clone(),
            body,
            progress: ByteProgress::new(metadata.size),
            guard,
            permit,
        })));
        let shared_on_exit = std::mem::DropGuard::new(Arc::downgrade(&shared), |weak| {
            if let Some(shared) = weak.upgrade() {
                terminate_from_timer(
                    &shared,
                    Termination::Fallback(Outcome::Failure(
                        LocalFailure::DownloadTimerExited.into(),
                    )),
                );
            }
        });
        let timer = tokio::spawn(async move {
            let stopped = context.wait_stopped().await;
            let shared = std::mem::DropGuard::dismiss(shared_on_exit).upgrade();
            if let Some(shared) = shared {
                terminate_from_timer(&shared, Termination::Stopped(stopped));
            }
        });
        Self {
            metadata,
            shared,
            timer: Some(timer),
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
                    self.cancel();
                    std::panic::resume_unwind(panic);
                }
            };
            self.finish_completion(completion);
            match std::task::ready!(result)? {
                ReadStep::Buffered => {}
                ReadStep::Empty(chunk) => drop(chunk),
                ReadStep::Chunk(chunk) => return Poll::Ready(Ok(chunk)),
            }
        }
    }

    pub async fn into_bytes(mut self, limit: usize) -> Result<Bytes, ObjectStorageError> {
        let (buffer, completion) = self.shared.lock().collection_buffer(limit);
        self.finish_completion(completion);
        let mut buffer = buffer?;
        while let Some(chunk) = self.next_chunk().await? {
            buffer.extend_from_slice(&chunk);
        }
        Ok(Bytes::from(buffer))
    }

    fn finish_completion(&mut self, completion: Option<Completion>) {
        if let Some(completion) = completion {
            if let Some(timer) = self.timer.take() {
                timer.abort();
            }
            completion.finish();
        }
    }

    fn cancel(&mut self) {
        if let Some(timer) = self.timer.take() {
            let completion = self
                .shared
                .lock()
                .terminate(Termination::Fallback(Outcome::Cancelled));
            timer.abort();
            if let Some(completion) = completion {
                completion.finish();
            }
        }
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        self.cancel();
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
