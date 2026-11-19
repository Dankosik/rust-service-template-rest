use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use http_body::{Body as HttpBody, Frame, SizeHint};
use infra_object_storage::{
    Download, MutationError, MutationFailure, ObjectKey, ObjectStorage, ObjectStorageError,
    PutBody, UnknownOutcome,
};

use crate::context::BodyReadTracker;
use crate::problem::{body_too_large, sanitized_problem};
use crate::{Code, Problem, RequestContext, ResponseContext};

struct TrackedBody {
    body: Body,
    tracker: BodyReadTracker,
}

impl HttpBody for TrackedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = &mut *self;
        this.tracker
            .poll_with(|| Pin::new(&mut this.body).poll_frame(cx))
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

impl RequestContext {
    #[must_use]
    pub fn upload_body(&self, length: u64, body: Body) -> PutBody {
        let tracked = TrackedBody {
            body,
            tracker: BodyReadTracker::new(Some(self)),
        };
        self.observe_put(PutBody::stream(length, tracked))
    }

    #[must_use]
    pub fn buffered_upload(&self, bytes: Bytes) -> PutBody {
        self.observe_put(PutBody::from(bytes))
    }
}

impl ResponseContext {
    pub async fn open_download(
        &self,
        storage: &ObjectStorage,
        key: &ObjectKey,
    ) -> Result<Download, ObjectStorageError> {
        storage.get(self.operation(), key).await
    }
}

#[must_use]
pub fn read_problem(error: ObjectStorageError, not_found: impl FnOnce() -> Problem) -> Problem {
    match error {
        ObjectStorageError::NotFound => not_found(),
        ObjectStorageError::Busy | ObjectStorageError::Unavailable => {
            Problem::new(Code::ServiceUnavailable)
        }
        ObjectStorageError::Stopped(stopped) => Problem::from(stopped),
        ObjectStorageError::TooLarge
        | ObjectStorageError::Rejected
        | ObjectStorageError::Integrity => sanitized_problem(),
    }
}

#[must_use]
pub fn mutation_problem(
    error: MutationError,
    already_exists: impl FnOnce() -> Problem,
    conflict: impl FnOnce() -> Problem,
) -> Problem {
    match error {
        MutationError::NotApplied(reason) => match reason {
            MutationFailure::AlreadyExists => already_exists(),
            MutationFailure::Conflict => conflict(),
            MutationFailure::TooLarge => body_too_large(),
            MutationFailure::Busy | MutationFailure::Unavailable => {
                Problem::new(Code::ServiceUnavailable)
            }
            MutationFailure::Stopped(stopped) => Problem::from(stopped),
            MutationFailure::Rejected => sanitized_problem(),
        },
        MutationError::OutcomeUnknown(reason) => {
            let problem = match reason {
                UnknownOutcome::Stopped(stopped) => Problem::from(stopped),
                UnknownOutcome::Unavailable => Problem::new(Code::ServiceUnavailable),
            };
            unconfirmed_write(problem)
        }
    }
}

pub(super) fn unconfirmed_write(problem: Problem) -> Problem {
    problem.detail_static("the storage provider could not confirm whether the write took effect")
}

#[cfg(test)]
mod tests;
