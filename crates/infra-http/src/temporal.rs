use infra_temporal::{
    MutationError, MutationFailure, ReadError, StartFailure, StartWorkflowError, UnknownOutcome,
    UpdateFailure, UpdateWorkflowError, WorkflowResultError,
};

use crate::problem::{body_too_large, sanitized_problem};
use crate::{Code, Problem};

const UNCONFIRMED_OPERATION_DETAIL: &str =
    "the workflow service could not confirm whether the operation took effect";
const PAYLOAD_DECODE_DETAIL: &str = "workflow payload could not be decoded";

fn unknown_outcome_problem(reason: UnknownOutcome) -> Problem {
    let problem = match reason {
        UnknownOutcome::Stopped(stopped) => Problem::from(stopped),
        UnknownOutcome::Unavailable => Problem::new(Code::ServiceUnavailable),
    };
    problem.detail_static(UNCONFIRMED_OPERATION_DETAIL)
}

#[must_use]
pub fn read_problem(error: ReadError, not_found: impl FnOnce() -> Problem) -> Problem {
    match error {
        ReadError::NotFound => not_found(),
        ReadError::Conflict | ReadError::QueryRejected => Problem::new(Code::Conflict),
        ReadError::InvalidInput => Problem::new(Code::BadRequest),
        ReadError::PayloadDecode => {
            Problem::new(Code::UnprocessableContent).detail_static(PAYLOAD_DECODE_DETAIL)
        }
        ReadError::Unauthenticated
        | ReadError::PermissionDenied
        | ReadError::Busy
        | ReadError::Unavailable => Problem::new(Code::ServiceUnavailable),
        ReadError::Stopped(stopped) => Problem::from(stopped),
        ReadError::Internal => sanitized_problem(),
    }
}

#[must_use]
pub fn start_problem(
    error: StartWorkflowError,
    already_exists: impl FnOnce() -> Problem,
    conflict: impl FnOnce() -> Problem,
) -> Problem {
    match error {
        StartWorkflowError::NotApplied(reason) => match reason {
            StartFailure::AlreadyStarted => already_exists(),
            StartFailure::Conflict => conflict(),
            StartFailure::InvalidInput => Problem::new(Code::BadRequest),
            StartFailure::TooLarge => body_too_large(),
            StartFailure::Unauthenticated
            | StartFailure::PermissionDenied
            | StartFailure::Busy
            | StartFailure::Unavailable => Problem::new(Code::ServiceUnavailable),
            StartFailure::Stopped(stopped) => Problem::from(stopped),
            StartFailure::Internal => sanitized_problem(),
        },
        StartWorkflowError::OutcomeUnknown(reason) => unknown_outcome_problem(reason),
    }
}

#[must_use]
pub fn mutation_problem(error: MutationError, not_found: impl FnOnce() -> Problem) -> Problem {
    match error {
        MutationError::NotApplied(reason) => match reason {
            MutationFailure::NotFound => not_found(),
            MutationFailure::InvalidInput => Problem::new(Code::BadRequest),
            MutationFailure::TooLarge => body_too_large(),
            MutationFailure::Unauthenticated
            | MutationFailure::PermissionDenied
            | MutationFailure::Busy
            | MutationFailure::Unavailable => Problem::new(Code::ServiceUnavailable),
            MutationFailure::Stopped(stopped) => Problem::from(stopped),
            MutationFailure::Internal => sanitized_problem(),
        },
        MutationError::OutcomeUnknown(reason) => unknown_outcome_problem(reason),
    }
}

#[must_use]
pub fn update_problem(
    error: UpdateWorkflowError,
    not_found: impl FnOnce() -> Problem,
    rejected: impl FnOnce() -> Problem,
) -> Problem {
    match error {
        UpdateWorkflowError::NotApplied(reason) => match reason {
            UpdateFailure::AlreadyStarted => Problem::new(Code::AlreadyExists),
            UpdateFailure::Conflict => Problem::new(Code::Conflict),
            UpdateFailure::NotFound => not_found(),
            UpdateFailure::UpdateRejected => rejected(),
            UpdateFailure::InvalidInput => Problem::new(Code::BadRequest),
            UpdateFailure::TooLarge => body_too_large(),
            UpdateFailure::Unauthenticated
            | UpdateFailure::PermissionDenied
            | UpdateFailure::Busy
            | UpdateFailure::Unavailable => Problem::new(Code::ServiceUnavailable),
            UpdateFailure::Stopped(stopped) => Problem::from(stopped),
            UpdateFailure::Internal => sanitized_problem(),
        },
        UpdateWorkflowError::OutcomeUnknown(reason) => unknown_outcome_problem(reason),
    }
}

#[must_use]
pub fn result_problem(
    error: WorkflowResultError,
    not_found: impl FnOnce() -> Problem,
    failed: impl FnOnce() -> Problem,
) -> Problem {
    match error {
        WorkflowResultError::NotFound => not_found(),
        WorkflowResultError::WorkflowFailed => failed(),
        WorkflowResultError::Cancelled
        | WorkflowResultError::Terminated
        | WorkflowResultError::TimedOut
        | WorkflowResultError::ContinuedAsNew => Problem::new(Code::Conflict),
        WorkflowResultError::InvalidInput => Problem::new(Code::BadRequest),
        WorkflowResultError::PayloadDecode => {
            Problem::new(Code::UnprocessableContent).detail_static(PAYLOAD_DECODE_DETAIL)
        }
        WorkflowResultError::Unauthenticated
        | WorkflowResultError::PermissionDenied
        | WorkflowResultError::Busy
        | WorkflowResultError::Unavailable => Problem::new(Code::ServiceUnavailable),
        WorkflowResultError::Stopped(stopped) => Problem::from(stopped),
        WorkflowResultError::Internal => sanitized_problem(),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};
    use axum::response::IntoResponse as _;
    use operation_context::Stopped;

    use super::*;
    use crate::test_support::body_json;

    async fn assert_problem(
        problem: Problem,
        expected_status: u16,
        expected_code: &str,
        expected_detail: Option<&str>,
    ) {
        let response = problem.into_response();
        assert_eq!(response.status().as_u16(), expected_status);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
        assert!(!response.headers().contains_key(RETRY_AFTER));
        let json = body_json(response).await;
        assert_eq!(json["code"], expected_code);
        assert_eq!(json["status"], expected_status);
        assert_eq!(json["type"], "about:blank");
        assert_eq!(json.get("detail").and_then(|v| v.as_str()), expected_detail);
    }

    #[tokio::test]
    async fn read_problem_maps_every_variant_and_preserves_custom_not_found() {
        let map = |error| read_problem(error, || Problem::new(Code::NotFound).detail("missing"));
        assert_problem(map(ReadError::NotFound), 404, "not_found", Some("missing")).await;
        assert_problem(map(ReadError::Conflict), 409, "conflict", None).await;
        assert_problem(map(ReadError::QueryRejected), 409, "conflict", None).await;
        assert_problem(map(ReadError::InvalidInput), 400, "bad_request", None).await;
        assert_problem(
            map(ReadError::PayloadDecode),
            422,
            "unprocessable_content",
            Some(PAYLOAD_DECODE_DETAIL),
        )
        .await;
        assert_problem(
            map(ReadError::Unauthenticated),
            503,
            "service_unavailable",
            None,
        )
        .await;
        assert_problem(
            map(ReadError::PermissionDenied),
            503,
            "service_unavailable",
            None,
        )
        .await;
        assert_problem(map(ReadError::Busy), 503, "service_unavailable", None).await;
        assert_problem(
            map(ReadError::Unavailable),
            503,
            "service_unavailable",
            None,
        )
        .await;
        assert_problem(
            map(ReadError::Stopped(Stopped::Deadline)),
            504,
            "gateway_timeout",
            Some("request budget expired before a response could be committed"),
        )
        .await;
        assert_problem(
            map(ReadError::Stopped(Stopped::Cancelled)),
            503,
            "service_unavailable",
            None,
        )
        .await;
        assert_problem(
            map(ReadError::Internal),
            500,
            "internal_error",
            Some(service_failure::SANITIZED_DETAIL),
        )
        .await;
    }

    #[tokio::test]
    async fn start_problem_separates_not_applied_from_outcome_unknown() {
        let map = |error| {
            start_problem(
                error,
                || Problem::new(Code::AlreadyExists).detail("duplicate"),
                || Problem::new(Code::Conflict).detail("running"),
            )
        };
        assert_problem(
            map(StartWorkflowError::NotApplied(StartFailure::AlreadyStarted)),
            409,
            "already_exists",
            Some("duplicate"),
        )
        .await;
        assert_problem(
            map(StartWorkflowError::NotApplied(StartFailure::Conflict)),
            409,
            "conflict",
            Some("running"),
        )
        .await;
        assert_problem(
            map(StartWorkflowError::NotApplied(StartFailure::InvalidInput)),
            400,
            "bad_request",
            None,
        )
        .await;
        assert_problem(
            map(StartWorkflowError::NotApplied(StartFailure::TooLarge)),
            413,
            "request_entity_too_large",
            Some("request body exceeds the configured limit"),
        )
        .await;
        for reason in [
            StartFailure::Unauthenticated,
            StartFailure::PermissionDenied,
            StartFailure::Busy,
            StartFailure::Unavailable,
            StartFailure::Stopped(Stopped::Cancelled),
        ] {
            assert_problem(
                map(StartWorkflowError::NotApplied(reason)),
                503,
                "service_unavailable",
                None,
            )
            .await;
        }
        assert_problem(
            map(StartWorkflowError::NotApplied(StartFailure::Stopped(
                Stopped::Deadline,
            ))),
            504,
            "gateway_timeout",
            Some("request budget expired before a response could be committed"),
        )
        .await;
        assert_problem(
            map(StartWorkflowError::NotApplied(StartFailure::Internal)),
            500,
            "internal_error",
            Some(service_failure::SANITIZED_DETAIL),
        )
        .await;
        assert_problem(
            map(StartWorkflowError::OutcomeUnknown(UnknownOutcome::Stopped(
                Stopped::Deadline,
            ))),
            504,
            "gateway_timeout",
            Some(UNCONFIRMED_OPERATION_DETAIL),
        )
        .await;
        assert_problem(
            map(StartWorkflowError::OutcomeUnknown(UnknownOutcome::Stopped(
                Stopped::Cancelled,
            ))),
            503,
            "service_unavailable",
            Some(UNCONFIRMED_OPERATION_DETAIL),
        )
        .await;
        assert_problem(
            map(StartWorkflowError::OutcomeUnknown(
                UnknownOutcome::Unavailable,
            )),
            503,
            "service_unavailable",
            Some(UNCONFIRMED_OPERATION_DETAIL),
        )
        .await;
    }

    #[tokio::test]
    async fn mutation_problem_separates_not_applied_from_outcome_unknown() {
        let map =
            |error| mutation_problem(error, || Problem::new(Code::NotFound).detail("no workflow"));
        assert_problem(
            map(MutationError::NotApplied(MutationFailure::NotFound)),
            404,
            "not_found",
            Some("no workflow"),
        )
        .await;
        assert_problem(
            map(MutationError::NotApplied(MutationFailure::InvalidInput)),
            400,
            "bad_request",
            None,
        )
        .await;
        assert_problem(
            map(MutationError::NotApplied(MutationFailure::TooLarge)),
            413,
            "request_entity_too_large",
            Some("request body exceeds the configured limit"),
        )
        .await;
        for reason in [
            MutationFailure::Unauthenticated,
            MutationFailure::PermissionDenied,
            MutationFailure::Busy,
            MutationFailure::Unavailable,
            MutationFailure::Stopped(Stopped::Cancelled),
        ] {
            assert_problem(
                map(MutationError::NotApplied(reason)),
                503,
                "service_unavailable",
                None,
            )
            .await;
        }
        assert_problem(
            map(MutationError::NotApplied(MutationFailure::Stopped(
                Stopped::Deadline,
            ))),
            504,
            "gateway_timeout",
            Some("request budget expired before a response could be committed"),
        )
        .await;
        assert_problem(
            map(MutationError::NotApplied(MutationFailure::Internal)),
            500,
            "internal_error",
            Some(service_failure::SANITIZED_DETAIL),
        )
        .await;
        assert_problem(
            map(MutationError::OutcomeUnknown(UnknownOutcome::Stopped(
                Stopped::Deadline,
            ))),
            504,
            "gateway_timeout",
            Some(UNCONFIRMED_OPERATION_DETAIL),
        )
        .await;
        assert_problem(
            map(MutationError::OutcomeUnknown(UnknownOutcome::Stopped(
                Stopped::Cancelled,
            ))),
            503,
            "service_unavailable",
            Some(UNCONFIRMED_OPERATION_DETAIL),
        )
        .await;
        assert_problem(
            map(MutationError::OutcomeUnknown(UnknownOutcome::Unavailable)),
            503,
            "service_unavailable",
            Some(UNCONFIRMED_OPERATION_DETAIL),
        )
        .await;
    }

    #[tokio::test]
    async fn update_problem_maps_not_found_rejected_conflicts_and_unknown_outcomes() {
        let map = |error| {
            update_problem(
                error,
                || Problem::new(Code::NotFound).detail("missing"),
                || Problem::new(Code::UnprocessableContent).detail("rejected"),
            )
        };
        let cases = [
            (
                UpdateWorkflowError::NotApplied(UpdateFailure::AlreadyStarted),
                409,
                "already_exists",
                None,
            ),
            (
                UpdateWorkflowError::NotApplied(UpdateFailure::Conflict),
                409,
                "conflict",
                None,
            ),
            (
                UpdateWorkflowError::NotApplied(UpdateFailure::NotFound),
                404,
                "not_found",
                Some("missing"),
            ),
            (
                UpdateWorkflowError::NotApplied(UpdateFailure::UpdateRejected),
                422,
                "unprocessable_content",
                Some("rejected"),
            ),
            (
                UpdateWorkflowError::NotApplied(UpdateFailure::InvalidInput),
                400,
                "bad_request",
                None,
            ),
            (
                UpdateWorkflowError::NotApplied(UpdateFailure::TooLarge),
                413,
                "request_entity_too_large",
                Some("request body exceeds the configured limit"),
            ),
            (
                UpdateWorkflowError::NotApplied(UpdateFailure::Stopped(Stopped::Deadline)),
                504,
                "gateway_timeout",
                Some("request budget expired before a response could be committed"),
            ),
            (
                UpdateWorkflowError::NotApplied(UpdateFailure::Internal),
                500,
                "internal_error",
                Some(service_failure::SANITIZED_DETAIL),
            ),
            (
                UpdateWorkflowError::OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Deadline)),
                504,
                "gateway_timeout",
                Some(UNCONFIRMED_OPERATION_DETAIL),
            ),
            (
                UpdateWorkflowError::OutcomeUnknown(UnknownOutcome::Stopped(Stopped::Cancelled)),
                503,
                "service_unavailable",
                Some(UNCONFIRMED_OPERATION_DETAIL),
            ),
            (
                UpdateWorkflowError::OutcomeUnknown(UnknownOutcome::Unavailable),
                503,
                "service_unavailable",
                Some(UNCONFIRMED_OPERATION_DETAIL),
            ),
        ];
        for (error, status, code, detail) in cases {
            assert_problem(map(error), status, code, detail).await;
        }
        for reason in [
            UpdateFailure::Unauthenticated,
            UpdateFailure::PermissionDenied,
            UpdateFailure::Busy,
            UpdateFailure::Unavailable,
            UpdateFailure::Stopped(Stopped::Cancelled),
        ] {
            assert_problem(
                map(UpdateWorkflowError::NotApplied(reason)),
                503,
                "service_unavailable",
                None,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn result_problem_maps_terminal_outcomes_and_transport_errors() {
        let map = |error| {
            result_problem(
                error,
                || Problem::new(Code::NotFound).detail("missing"),
                || Problem::new(Code::UnprocessableContent).detail("workflow failed"),
            )
        };
        assert_problem(
            map(WorkflowResultError::NotFound),
            404,
            "not_found",
            Some("missing"),
        )
        .await;
        assert_problem(
            map(WorkflowResultError::WorkflowFailed),
            422,
            "unprocessable_content",
            Some("workflow failed"),
        )
        .await;
        for terminal in [
            WorkflowResultError::Cancelled,
            WorkflowResultError::Terminated,
            WorkflowResultError::TimedOut,
            WorkflowResultError::ContinuedAsNew,
        ] {
            assert_problem(map(terminal), 409, "conflict", None).await;
        }
        assert_problem(
            map(WorkflowResultError::InvalidInput),
            400,
            "bad_request",
            None,
        )
        .await;
        assert_problem(
            map(WorkflowResultError::PayloadDecode),
            422,
            "unprocessable_content",
            Some(PAYLOAD_DECODE_DETAIL),
        )
        .await;
        for reason in [
            WorkflowResultError::Unauthenticated,
            WorkflowResultError::PermissionDenied,
            WorkflowResultError::Busy,
            WorkflowResultError::Unavailable,
            WorkflowResultError::Stopped(Stopped::Cancelled),
        ] {
            assert_problem(map(reason), 503, "service_unavailable", None).await;
        }
        assert_problem(
            map(WorkflowResultError::Stopped(Stopped::Deadline)),
            504,
            "gateway_timeout",
            Some("request budget expired before a response could be committed"),
        )
        .await;
        assert_problem(
            map(WorkflowResultError::Internal),
            500,
            "internal_error",
            Some(service_failure::SANITIZED_DETAIL),
        )
        .await;
    }
}
