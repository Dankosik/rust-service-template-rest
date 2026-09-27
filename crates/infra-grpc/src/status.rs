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
    Status::with_error_details(
        code(failure.meaning()),
        safe_message(failure.meaning()),
        details,
    )
}

const fn code(meaning: Meaning) -> Code {
    match meaning {
        Meaning::BadRequest => Code::InvalidArgument,
        Meaning::Unauthenticated => Code::Unauthenticated,
        Meaning::PermissionDenied => Code::PermissionDenied,
        Meaning::NotFound => Code::NotFound,
        Meaning::AlreadyExists => Code::AlreadyExists,
        Meaning::Conflict => Code::Aborted,
        Meaning::Unimplemented => Code::Unimplemented,
        Meaning::ResourceExhausted => Code::ResourceExhausted,
        Meaning::Unavailable => Code::Unavailable,
        Meaning::DeadlineExceeded => Code::DeadlineExceeded,
        Meaning::Internal => Code::Internal,
    }
}

const fn safe_message(meaning: Meaning) -> &'static str {
    match meaning {
        Meaning::ResourceExhausted => service_failure::AT_CAPACITY_DETAIL,
        Meaning::DeadlineExceeded => "request deadline exceeded",
        Meaning::Unavailable => "service is unavailable",
        Meaning::Unauthenticated => "authentication failed",
        Meaning::PermissionDenied => "permission denied",
        Meaning::NotFound => "not found",
        Meaning::AlreadyExists => "already exists",
        Meaning::Conflict => "request conflict",
        Meaning::Unimplemented => "method is not implemented",
        Meaning::BadRequest | Meaning::Internal => SANITIZED_DETAIL,
    }
}
