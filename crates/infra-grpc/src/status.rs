use std::collections::HashMap;

use service_failure::{ClassifiedFailure, Meaning, SANITIZED_DETAIL};
use tonic::{Code, Status};
use tonic_types::{ErrorDetails, StatusExt as _};

/// Builds a native status from the closed shared failure catalog.
#[must_use]
#[allow(
    clippy::needless_pass_by_value,
    reason = "callers hand off an owned classified failure; the public signature stays by value"
)]
pub fn classified_status(failure: ClassifiedFailure) -> Status {
    let mut details = ErrorDetails::new();
    details.set_error_info(failure.code().as_str(), "service", HashMap::new());
    if let Some(retry_after) = failure.retry_after() {
        details.set_retry_info(Some(retry_after));
    }
    let (code, message) = status_parts(failure.meaning());
    Status::with_error_details(code, message, details)
}

const fn status_parts(meaning: Meaning) -> (Code, &'static str) {
    match meaning {
        Meaning::BadRequest => (Code::InvalidArgument, SANITIZED_DETAIL),
        Meaning::Unauthenticated => (Code::Unauthenticated, "authentication failed"),
        Meaning::PermissionDenied => (Code::PermissionDenied, "permission denied"),
        Meaning::NotFound => (Code::NotFound, "not found"),
        Meaning::AlreadyExists => (Code::AlreadyExists, "already exists"),
        Meaning::Conflict => (Code::Aborted, "request conflict"),
        Meaning::Unimplemented => (Code::Unimplemented, "method is not implemented"),
        Meaning::ResourceExhausted => {
            (Code::ResourceExhausted, service_failure::AT_CAPACITY_DETAIL)
        }
        Meaning::Unavailable => (Code::Unavailable, "service is unavailable"),
        Meaning::DeadlineExceeded => (Code::DeadlineExceeded, "request deadline exceeded"),
        Meaning::Internal => (Code::Internal, SANITIZED_DETAIL),
    }
}
