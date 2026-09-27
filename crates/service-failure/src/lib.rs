//! Transport-neutral, closed service failure classification.
//!
//! Transports project this catalog into their own wire formats. The catalog
//! intentionally carries no status code, response schema, or caller-controlled
//! detail text.

use std::time::Duration;

use serde::ser::Serializer;

/// Stable machine-readable failure code a client matches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    BadRequest,
    Unauthorized,
    // template:begin authn:http-authentication-codes
    AuthenticationRequired,
    AuthenticationMalformed,
    AuthenticationInvalid,
    AuthenticationUnavailable,
    // template:end authn:http-authentication-codes
    // template:begin http-idempotency:http-idempotency-codes
    IdempotencyRequestInProgress,
    IdempotencyKeyMismatch,
    IdempotencyUnavailable,
    // template:end http-idempotency:http-idempotency-codes
    // template:begin inbound-webhooks:http-webhook-codes
    WebhookRejected,
    WebhookConflict,
    // template:end inbound-webhooks:http-webhook-codes
    Forbidden,
    NotFound,
    MethodNotAllowed,
    Conflict,
    AlreadyExists,
    RequestEntityTooLarge,
    RequestHeaderFieldsTooLarge,
    UnprocessableContent,
    TooManyRequests,
    InternalServerError,
    ServiceUnavailable,
    RequestTimeout,
}

/// Transport-neutral failure meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Meaning {
    BadRequest,
    Unauthenticated,
    PermissionDenied,
    NotFound,
    AlreadyExists,
    Conflict,
    Unimplemented,
    ResourceExhausted,
    Unavailable,
    DeadlineExceeded,
    Internal,
}

/// Closed classified failure data rendered by a transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassifiedFailure {
    code: Code,
    retry_after: Option<Duration>,
}

impl ClassifiedFailure {
    #[must_use]
    pub const fn new(code: Code) -> Self {
        Self {
            code,
            retry_after: None,
        }
    }

    #[must_use]
    pub const fn code(&self) -> Code {
        self.code
    }

    #[must_use]
    pub const fn meaning(&self) -> Meaning {
        self.code.meaning()
    }

    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    #[must_use]
    pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = Some(retry_after);
        self
    }
}

/// Caller-visible text for failures a transport refuses to describe.
pub const SANITIZED_DETAIL: &str = "request failed";

/// Caller-visible text when admission control sheds a request.
pub const AT_CAPACITY_DETAIL: &str = "server is at capacity";

impl Code {
    /// Every published code, for transport projections and coverage tests.
    pub const ALL: &'static [Code] = &[
        Self::BadRequest,
        Self::Unauthorized,
        // template:begin authn:service-failure-authentication-code-all
        Self::AuthenticationRequired,
        Self::AuthenticationMalformed,
        Self::AuthenticationInvalid,
        Self::AuthenticationUnavailable,
        // template:end authn:service-failure-authentication-code-all
        // template:begin http-idempotency:service-failure-idempotency-code-all
        Self::IdempotencyRequestInProgress,
        Self::IdempotencyKeyMismatch,
        Self::IdempotencyUnavailable,
        // template:end http-idempotency:service-failure-idempotency-code-all
        // template:begin inbound-webhooks:service-failure-webhook-code-all
        Self::WebhookRejected,
        Self::WebhookConflict,
        // template:end inbound-webhooks:service-failure-webhook-code-all
        Self::Forbidden,
        Self::NotFound,
        Self::MethodNotAllowed,
        Self::Conflict,
        Self::AlreadyExists,
        Self::RequestEntityTooLarge,
        Self::RequestHeaderFieldsTooLarge,
        Self::UnprocessableContent,
        Self::TooManyRequests,
        Self::InternalServerError,
        Self::ServiceUnavailable,
        Self::RequestTimeout,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::Unauthorized => "unauthorized",
            // template:begin authn:service-failure-authentication-code-wire
            Self::AuthenticationRequired => "authentication_required",
            Self::AuthenticationMalformed => "authentication_malformed",
            Self::AuthenticationInvalid => "authentication_invalid",
            Self::AuthenticationUnavailable => "authentication_unavailable",
            // template:end authn:service-failure-authentication-code-wire
            // template:begin http-idempotency:service-failure-idempotency-code-wire
            Self::IdempotencyRequestInProgress => "idempotency_request_in_progress",
            Self::IdempotencyKeyMismatch => "idempotency_key_mismatch",
            Self::IdempotencyUnavailable => "idempotency_unavailable",
            // template:end http-idempotency:service-failure-idempotency-code-wire
            // template:begin inbound-webhooks:service-failure-webhook-code-wire
            Self::WebhookRejected => "webhook_rejected",
            Self::WebhookConflict => "webhook_conflict",
            // template:end inbound-webhooks:service-failure-webhook-code-wire
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::Conflict => "conflict",
            Self::AlreadyExists => "already_exists",
            Self::RequestEntityTooLarge => "request_entity_too_large",
            Self::RequestHeaderFieldsTooLarge => "request_header_fields_too_large",
            Self::UnprocessableContent => "unprocessable_content",
            Self::TooManyRequests => "too_many_requests",
            Self::InternalServerError => "internal_error",
            Self::ServiceUnavailable => "service_unavailable",
            Self::RequestTimeout => "request_timeout",
        }
    }

    #[must_use]
    #[allow(
        clippy::match_same_arms,
        reason = "Optional profiles remove complete match arms independently."
    )]
    pub const fn meaning(self) -> Meaning {
        match self {
            Self::BadRequest | Self::UnprocessableContent => Meaning::BadRequest,
            // template:begin authn:service-failure-authentication-code-meaning
            Self::AuthenticationMalformed => Meaning::BadRequest,
            // template:end authn:service-failure-authentication-code-meaning
            // template:begin http-idempotency:service-failure-idempotency-code-meaning
            Self::IdempotencyKeyMismatch => Meaning::BadRequest,
            // template:end http-idempotency:service-failure-idempotency-code-meaning
            // template:begin inbound-webhooks:service-failure-webhook-code-meaning
            Self::WebhookRejected => Meaning::BadRequest,
            // template:end inbound-webhooks:service-failure-webhook-code-meaning
            Self::Unauthorized => Meaning::Unauthenticated,
            // template:begin authn:service-failure-authentication-unauthenticated-meaning
            Self::AuthenticationRequired | Self::AuthenticationInvalid => Meaning::Unauthenticated,
            // template:end authn:service-failure-authentication-unauthenticated-meaning
            Self::Forbidden => Meaning::PermissionDenied,
            Self::NotFound => Meaning::NotFound,
            Self::AlreadyExists => Meaning::AlreadyExists,
            Self::Conflict => Meaning::Conflict,
            // template:begin http-idempotency:service-failure-idempotency-conflict-meaning
            Self::IdempotencyRequestInProgress => Meaning::Conflict,
            // template:end http-idempotency:service-failure-idempotency-conflict-meaning
            // template:begin inbound-webhooks:service-failure-webhook-conflict-meaning
            Self::WebhookConflict => Meaning::Conflict,
            // template:end inbound-webhooks:service-failure-webhook-conflict-meaning
            Self::MethodNotAllowed => Meaning::Unimplemented,
            Self::RequestEntityTooLarge
            | Self::RequestHeaderFieldsTooLarge
            | Self::TooManyRequests => Meaning::ResourceExhausted,
            Self::ServiceUnavailable => Meaning::Unavailable,
            // template:begin authn:service-failure-authentication-unavailable-meaning
            Self::AuthenticationUnavailable => Meaning::Unavailable,
            // template:end authn:service-failure-authentication-unavailable-meaning
            // template:begin http-idempotency:service-failure-idempotency-unavailable-meaning
            Self::IdempotencyUnavailable => Meaning::Unavailable,
            // template:end http-idempotency:service-failure-idempotency-unavailable-meaning
            Self::RequestTimeout => Meaning::DeadlineExceeded,
            Self::InternalServerError => Meaning::Internal,
        }
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl serde::Serialize for Code {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_unique_wire_forms_and_exhaustive_meanings() {
        let mut wires = std::collections::HashSet::new();
        for code in Code::ALL {
            assert!(wires.insert(code.as_str()));
            assert_eq!(code.to_string(), code.as_str());
            let _ = code.meaning();
        }
    }
}
