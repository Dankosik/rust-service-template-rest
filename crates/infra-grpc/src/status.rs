use std::collections::HashMap;
use std::time::Duration;

use service_failure::Code as FailureCode;
use tonic::{Code, Status};
use tonic_types::{ErrorDetails, StatusExt as _};

/// `google.rpc.ErrorInfo.domain` for every catalog failure. The template
/// initializer replaces it with the service name, the same identity as the
/// OpenTelemetry `service.name`.
pub const ERROR_DOMAIN: &str = "service";

/// Message for an internal failure the transport refuses to describe.
pub(crate) const REQUEST_FAILED: &str = "request failed";

/// Builds a native status for a catalog failure.
///
/// `google.rpc.ErrorInfo` carries the code in `UPPER_SNAKE_CASE`, as AIP-193
/// requires, in [`ERROR_DOMAIN`]. The message is a fixed safe string.
#[must_use]
pub fn failure_status(code: FailureCode) -> Status {
    status_with_details(code, None)
}

/// [`failure_status`] plus `google.rpc.RetryInfo`. The delay is a hint to
/// the caller; it does not enable retries.
#[must_use]
pub fn failure_status_with_retry(code: FailureCode, retry_after: Duration) -> Status {
    status_with_details(code, Some(retry_after))
}

fn status_with_details(code: FailureCode, retry_after: Option<Duration>) -> Status {
    let mut details = ErrorDetails::new();
    details.set_error_info(error_reason(code), ERROR_DOMAIN, HashMap::new());
    if let Some(retry_after) = retry_after {
        details.set_retry_info(Some(retry_after));
    }
    let grpc_code = grpc_code(code);
    Status::with_error_details(grpc_code, safe_message(grpc_code), details)
}

fn error_reason(code: FailureCode) -> String {
    code.as_str().to_ascii_uppercase()
}

#[allow(
    clippy::match_same_arms,
    reason = "optional profiles remove their arms independently"
)]
const fn grpc_code(code: FailureCode) -> Code {
    match code {
        FailureCode::BadRequest | FailureCode::UnprocessableContent => Code::InvalidArgument,
        // template:begin authn:grpc-authentication-status-code
        FailureCode::AuthenticationMalformed => Code::InvalidArgument,
        FailureCode::AuthenticationRequired | FailureCode::AuthenticationInvalid => {
            Code::Unauthenticated
        }
        FailureCode::AuthenticationUnavailable => Code::Unavailable,
        // template:end authn:grpc-authentication-status-code
        // template:begin http-idempotency:grpc-idempotency-status-code
        FailureCode::IdempotencyKeyMismatch => Code::InvalidArgument,
        FailureCode::IdempotencyRequestInProgress => Code::Aborted,
        FailureCode::IdempotencyUnavailable => Code::Unavailable,
        // template:end http-idempotency:grpc-idempotency-status-code
        // template:begin inbound-webhooks:grpc-webhook-status-code
        FailureCode::WebhookRejected => Code::InvalidArgument,
        // template:end inbound-webhooks:grpc-webhook-status-code
        FailureCode::Forbidden => Code::PermissionDenied,
        FailureCode::NotFound => Code::NotFound,
        FailureCode::AlreadyExists => Code::AlreadyExists,
        FailureCode::Conflict => Code::Aborted,
        FailureCode::MethodNotAllowed => Code::Unimplemented,
        FailureCode::RequestEntityTooLarge | FailureCode::TooManyRequests => {
            Code::ResourceExhausted
        }
        FailureCode::ServiceUnavailable => Code::Unavailable,
        FailureCode::RequestTimeout => Code::DeadlineExceeded,
        FailureCode::InternalServerError => Code::Internal,
    }
}

/// Fixed caller-visible text per gRPC code; never caller-controlled input.
const fn safe_message(code: Code) -> &'static str {
    match code {
        Code::Unauthenticated => "authentication failed",
        Code::PermissionDenied => "permission denied",
        Code::NotFound => "not found",
        Code::AlreadyExists => "already exists",
        Code::Aborted => "request conflict",
        Code::Unimplemented => "method is not implemented",
        Code::ResourceExhausted => "request exceeds a resource limit",
        Code::Unavailable => "service is unavailable",
        Code::DeadlineExceeded => "request deadline exceeded",
        _ => REQUEST_FAILED,
    }
}

#[cfg(test)]
mod tests {
    use service_failure::Code as FailureCode;

    use super::{error_reason, failure_status};

    fn is_aip_193_reason(reason: &str) -> bool {
        let bytes = reason.as_bytes();
        let Some((first, rest)) = bytes.split_first() else {
            return false;
        };
        let Some((last, middle)) = rest.split_last() else {
            return false;
        };
        !middle.is_empty()
            && first.is_ascii_uppercase()
            && middle
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
            && (last.is_ascii_uppercase() || last.is_ascii_digit())
    }

    #[test]
    fn error_reasons_follow_aip_193() {
        for code in FailureCode::ALL {
            let reason = error_reason(*code);
            assert!(reason.len() <= 63, "{reason} exceeds 63 bytes");
            assert!(
                is_aip_193_reason(&reason),
                "{reason} is not an AIP-193 reason"
            );
        }
        assert_eq!(error_reason(FailureCode::BadRequest), "BAD_REQUEST");
    }

    #[test]
    fn resource_limits_do_not_claim_server_capacity() {
        let status = failure_status(FailureCode::RequestEntityTooLarge);
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(status.message(), "request exceeds a resource limit");
    }
}
