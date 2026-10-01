//! The gRPC projection of the shared failure catalog.

use std::collections::HashMap;
use std::time::Duration;

use service_failure::{Code, SANITIZED_DETAIL};
use tonic::Status;
use tonic_types::{ErrorDetails, StatusExt as _};

/// `google.rpc.ErrorInfo.domain` of every catalog failure: the service name,
/// within which a reason is unique. The initializer writes the derived
/// service's name here.
pub const ERROR_DOMAIN: &str = "service";

/// A failure from the shared catalog, returned by a handler or the transport
/// as a status: `Err(Failure::new(Code::NotFound).into())`.
///
/// The status code and message are fixed by the catalog code.
/// `google.rpc.ErrorInfo` carries the code in upper case as the reason a
/// client matches on, in [`ERROR_DOMAIN`].
#[derive(Clone, Debug)]
pub struct Failure {
    code: Code,
    details: ErrorDetails,
}

impl Failure {
    #[must_use]
    pub fn new(code: Code) -> Self {
        Self {
            code,
            details: ErrorDetails::with_error_info(
                code.as_str().to_ascii_uppercase(),
                ERROR_DOMAIN,
                HashMap::new(),
            ),
        }
    }

    /// Adds `google.rpc.RetryInfo` with the delay before another attempt. It
    /// does not make a client retry.
    #[must_use]
    pub fn retry_after(mut self, after: Duration) -> Self {
        self.details.set_retry_info(Some(after));
        self
    }

    /// Adds a `google.rpc.BadRequest` field violation: the request field as a
    /// protobuf field path such as `message`, and the constraint that failed.
    /// Never pass the submitted value.
    #[must_use]
    pub fn field_violation(
        mut self,
        field: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.details.add_bad_request_violation(field, description);
        self
    }

    /// The status for a transport-owned answer whose code or message differs
    /// from the catalog projection.
    pub(crate) fn into_status_as(self, grpc: tonic::Code, message: &'static str) -> Status {
        Status::with_error_details(grpc, message, self.details)
    }
}

impl From<Failure> for Status {
    fn from(failure: Failure) -> Self {
        let (grpc, message) = projection(failure.code);
        failure.into_status_as(grpc, message)
    }
}

/// The status code and fixed safe message of each catalog code.
#[allow(
    clippy::match_same_arms,
    reason = "Optional profiles remove complete match arms independently."
)]
const fn projection(code: Code) -> (tonic::Code, &'static str) {
    const INVALID: (tonic::Code, &str) = (tonic::Code::InvalidArgument, "request is invalid");
    const UNAUTHENTICATED: (tonic::Code, &str) =
        (tonic::Code::Unauthenticated, "authentication failed");
    const CONFLICT: (tonic::Code, &str) = (tonic::Code::Aborted, "request conflict");
    const EXHAUSTED: (tonic::Code, &str) =
        (tonic::Code::ResourceExhausted, "resource limit exceeded");
    const UNAVAILABLE: (tonic::Code, &str) = (tonic::Code::Unavailable, "service is unavailable");
    match code {
        Code::BadRequest | Code::UnsupportedMediaType | Code::UnprocessableContent => INVALID,
        // template:begin http-idempotency:grpc-idempotency-invalid-status
        Code::IdempotencyKeyMismatch => INVALID,
        // template:end http-idempotency:grpc-idempotency-invalid-status
        // template:begin inbound-webhooks:grpc-webhook-status
        Code::WebhookRejected => INVALID,
        // template:end inbound-webhooks:grpc-webhook-status
        Code::Unauthorized => UNAUTHENTICATED,
        // template:begin authn:grpc-authentication-status
        // A bearer the server could not parse is still a credential failure
        // in gRPC, which has no 400 for it; the reason tells the cases apart.
        Code::AuthenticationRequired
        | Code::AuthenticationMalformed
        | Code::AuthenticationInvalid => UNAUTHENTICATED,
        Code::AuthenticationUnavailable => {
            (tonic::Code::Unavailable, "authentication is unavailable")
        }
        // template:end authn:grpc-authentication-status
        Code::Forbidden => (tonic::Code::PermissionDenied, "permission denied"),
        Code::NotFound => (tonic::Code::NotFound, "not found"),
        Code::AlreadyExists => (tonic::Code::AlreadyExists, "already exists"),
        Code::Conflict => CONFLICT,
        // template:begin http-idempotency:grpc-idempotency-conflict-status
        Code::IdempotencyRequestInProgress => CONFLICT,
        // template:end http-idempotency:grpc-idempotency-conflict-status
        Code::MethodNotAllowed => (tonic::Code::Unimplemented, "method is not implemented"),
        Code::RequestEntityTooLarge | Code::RequestHeaderFieldsTooLarge | Code::TooManyRequests => {
            EXHAUSTED
        }
        Code::ServiceUnavailable => UNAVAILABLE,
        // template:begin http-idempotency:grpc-idempotency-unavailable-status
        Code::IdempotencyUnavailable => UNAVAILABLE,
        // template:end http-idempotency:grpc-idempotency-unavailable-status
        Code::RequestTimeout => (tonic::Code::DeadlineExceeded, "request deadline exceeded"),
        Code::InternalServerError => (tonic::Code::Internal, SANITIZED_DETAIL),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_is_a_status_with_a_valid_error_info_reason() {
        for &code in Code::ALL {
            let status = Status::from(Failure::new(code));
            assert_ne!(status.code(), tonic::Code::Ok, "{code}");
            assert_ne!(status.code(), tonic::Code::Unknown, "{code}");
            assert!(!status.message().is_empty(), "{code}");
            let details = status.get_error_details();
            let info = details.error_info().expect("error info");
            assert_eq!(info.domain, ERROR_DOMAIN);
            assert!(info.metadata.is_empty());
            // `google.rpc.ErrorInfo.reason`: `[A-Z][A-Z0-9_]+[A-Z0-9]`, at
            // most 63 characters.
            let reason = info.reason.as_bytes();
            assert_eq!(info.reason, code.as_str().to_ascii_uppercase());
            assert!((3..=63).contains(&reason.len()), "{code}");
            assert!(reason[0].is_ascii_uppercase(), "{code}");
            assert!(reason[reason.len() - 1] != b'_', "{code}");
            assert!(
                reason.iter().all(|byte| byte.is_ascii_uppercase()
                    || byte.is_ascii_digit()
                    || *byte == b'_'),
                "{code}"
            );
            assert!(details.retry_info().is_none());
            assert!(details.bad_request().is_none());
        }
    }

    #[test]
    fn retry_delay_and_field_violations_travel_as_standard_details() {
        let status = Status::from(
            Failure::new(Code::BadRequest)
                .retry_after(Duration::from_secs(2))
                .field_violation("message", "must be 1 to 1024 bytes")
                .field_violation("items[0].name", "must not be empty"),
        );
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        let details = status.get_error_details();
        assert_eq!(
            details.retry_info().and_then(|info| info.retry_delay),
            Some(Duration::from_secs(2))
        );
        let violations = &details.bad_request().expect("bad request").field_violations;
        let violations: Vec<_> = violations
            .iter()
            .map(|violation| (violation.field.as_str(), violation.description.as_str()))
            .collect();
        assert_eq!(
            violations,
            [
                ("message", "must be 1 to 1024 bytes"),
                ("items[0].name", "must not be empty")
            ]
        );
    }
}
