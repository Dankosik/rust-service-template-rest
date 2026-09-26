use std::collections::HashMap;
use std::error::Error as _;
use std::fmt;
use std::sync::Arc;

use service_failure::{ClassifiedFailure, Meaning, SANITIZED_DETAIL};
use tonic::{Code, Status};
use tonic_types::{ErrorDetails, StatusExt as _};

/// Builds a native status whose in-process source proves that it came from the
/// closed shared failure catalog.  The source is never sent on the wire.
#[must_use]
pub fn classified_status(failure: ClassifiedFailure) -> Status {
    let mut details = ErrorDetails::new();
    details.set_error_info(failure.code().as_str(), "service", HashMap::new());
    if let Some(retry_after) = failure.retry_after() {
        details.set_retry_info(Some(retry_after));
    }
    let mut status = Status::with_error_details(
        code(failure.meaning()),
        safe_message(failure.meaning()),
        details,
    );
    status.set_source(Arc::new(ClassifiedMarker {
        failure,
        validation: Vec::new(),
    }));
    status
}

/// Converts an error returned by a generated implementation.  Only the
/// private marker installed by [`classified_status`] is trusted; a raw Status
/// cannot impersonate a framework or classified failure.
#[must_use]
pub(crate) fn sanitize_handler_status(status: Status) -> Status {
    if classified_failure(&status).is_some() {
        status
    } else {
        Status::internal(SANITIZED_DETAIL)
    }
}

#[must_use]
pub(crate) fn classified_failure(status: &Status) -> Option<&ClassifiedFailure> {
    status
        .source()
        .and_then(|source| source.downcast_ref::<ClassifiedMarker>())
        .map(|marker| {
            let _ = marker.validation.len();
            &marker.failure
        })
}

pub(crate) fn classified_validation_status(
    failure: ClassifiedFailure,
    validation: Vec<ValidationDetail>,
) -> Status {
    let mut details = ErrorDetails::new();
    details.set_error_info(failure.code().as_str(), "service", HashMap::new());
    if let Some(retry_after) = failure.retry_after() {
        details.set_retry_info(Some(retry_after));
    }
    let mut remaining = 16 * 1024usize;
    for detail in &validation {
        // The wire envelope also carries protobuf tags/type URLs; reserve a
        // conservative fixed amount and truncate the list rather than slice a
        // schema-owned identifier into a different identifier.
        let cost = detail
            .field
            .len()
            .saturating_add(detail.rule.len())
            .saturating_add(128);
        if cost > remaining {
            break;
        }
        details.add_bad_request_violation(&detail.field, &detail.rule);
        remaining -= cost;
    }
    let mut status = Status::with_error_details(
        code(failure.meaning()),
        safe_message(failure.meaning()),
        details,
    );
    status.set_source(Arc::new(ClassifiedMarker {
        failure,
        validation,
    }));
    status
}

pub(crate) fn panic_status() -> Status {
    Status::internal(SANITIZED_DETAIL)
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

#[derive(Debug)]
pub(crate) struct ValidationDetail {
    pub(crate) field: String,
    pub(crate) rule: String,
}

impl ValidationDetail {
    pub(crate) fn new(field: String, rule: String) -> Self {
        Self { field, rule }
    }
}

#[derive(Debug)]
struct ClassifiedMarker {
    failure: ClassifiedFailure,
    validation: Vec<ValidationDetail>,
}

impl fmt::Display for ClassifiedMarker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("classified service failure")
    }
}

impl std::error::Error for ClassifiedMarker {}
