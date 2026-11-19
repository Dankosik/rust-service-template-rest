use serde::ser::Serializer;

macro_rules! codes {
    ($($variant:ident => $wire:literal,)+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, enum_map::Enum)]
        pub enum Code {
            $(
                $variant,
            )+
        }

        impl Code {
            pub const ALL: &'static [Code] = &[$(Self::$variant,)+];

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
    RequestTimeout => "request_timeout",
    GatewayTimeout => "gateway_timeout",
}

pub const SANITIZED_DETAIL: &str = "request failed";

pub const AT_CAPACITY_DETAIL: &str = "server is at capacity";

pub const AT_CAPACITY_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

// template:begin authn:insufficient-scope-detail
pub const INSUFFICIENT_SCOPE_DETAIL: &str = "the verified principal lacks the required scope";
// template:end authn:insufficient-scope-detail

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
