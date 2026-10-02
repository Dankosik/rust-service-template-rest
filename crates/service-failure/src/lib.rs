//! Closed catalog of the failure identities a client can match on.
//!
//! Each transport projects a [`Code`] into its own wire format: `infra-http`
//! into an RFC 9457 problem, `infra-grpc` into a status carrying
//! `google.rpc.ErrorInfo`. The catalog carries no status code, response
//! schema, or caller-controlled detail text.
//!
//! A code that says no more than its HTTP status is named after that status
//! (`not_found`, `gateway_timeout`); a code that says more names the failure
//! (`idempotency_key_mismatch`). A code joins the catalog with its first
//! producer or a reserved use by service handlers; a status only the
//! connection layer answers, such as hyper's 431, has no code.

use serde::ser::Serializer;

/// Declares the catalog once, so the variants, the list of them and their
/// wire spellings cannot drift apart and a profile removes one line per code.
macro_rules! codes {
    ($($variant:ident => $wire:literal,)+) => {
        /// Stable machine-readable failure code a client matches on.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Code {
            $($variant,)+
        }

        impl Code {
            /// Every published code, for transport coverage tests.
            pub const ALL: &'static [Code] = &[$(Self::$variant,)+];

            /// The wire spelling: lowercase `snake_case` of at most 63 bytes,
            /// so its uppercase form is a valid `google.rpc.ErrorInfo` reason.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }
        }
    };
}

codes! {
    BadRequest => "bad_request",
    Unauthorized => "unauthorized",
    // template:begin authn:http-authentication-codes
    AuthenticationRequired => "authentication_required",
    AuthenticationMalformed => "authentication_malformed",
    AuthenticationInvalid => "authentication_invalid",
    AuthenticationUnavailable => "authentication_unavailable",
    // template:end authn:http-authentication-codes
    // template:begin http-idempotency:http-idempotency-codes
    IdempotencyRequestInProgress => "idempotency_request_in_progress",
    IdempotencyKeyMismatch => "idempotency_key_mismatch",
    IdempotencyUnavailable => "idempotency_unavailable",
    // template:end http-idempotency:http-idempotency-codes
    // template:begin inbound-webhooks:http-webhook-codes
    WebhookRejected => "webhook_rejected",
    // template:end inbound-webhooks:http-webhook-codes
    Forbidden => "forbidden",
    NotFound => "not_found",
    MethodNotAllowed => "method_not_allowed",
    Conflict => "conflict",
    AlreadyExists => "already_exists",
    RequestEntityTooLarge => "request_entity_too_large",
    UnsupportedMediaType => "unsupported_media_type",
    UnprocessableContent => "unprocessable_content",
    TooManyRequests => "too_many_requests",
    InternalServerError => "internal_error",
    ServiceUnavailable => "service_unavailable",
    GatewayTimeout => "gateway_timeout",
}

/// Caller-visible text for failures a transport refuses to describe.
pub const SANITIZED_DETAIL: &str = "request failed";

/// Caller-visible text when admission control sheds a request.
pub const AT_CAPACITY_DETAIL: &str = "server is at capacity";

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
    fn wire_forms_are_unique_lowercase_snake_case_within_the_reason_limit() {
        let mut wires = std::collections::HashSet::new();
        for code in Code::ALL {
            let wire = code.as_str();
            assert!(wires.insert(wire), "{wire} is published twice");
            assert_eq!(code.to_string(), wire);
            assert!((3..=63).contains(&wire.len()), "{wire}");
            assert!(wire.starts_with(|first: char| first.is_ascii_lowercase()));
            assert!(!wire.ends_with('_'), "{wire}");
            assert!(
                wire.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
                "{wire}"
            );
        }
    }
}
