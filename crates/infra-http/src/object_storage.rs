use axum::body::{Body, Bytes};
use futures_util::stream;
use http_body_util::StreamBody;
use infra_object_storage::{
    Download, MutationError, MutationFailure, ObjectKey, ObjectStorage, ObjectStorageError, PutBody,
};

use crate::problem::{body_too_large, sanitized_problem};
use crate::{Code, Problem, RequestContext, ResponseContext};

impl RequestContext {
    #[must_use]
    pub fn upload_body(&self, length: u64, body: Body) -> PutBody {
        let frames = stream::unfold((self.clone(), body), |(context, mut body)| async move {
            let frame = context.read_body_frame(&mut body).await?;
            Some((frame, (context, body)))
        });
        self.observe_put(PutBody::stream(length, StreamBody::new(frames)))
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
                MutationFailure::Stopped(stopped) => Problem::from(stopped),
                _ => Problem::new(Code::ServiceUnavailable),
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
