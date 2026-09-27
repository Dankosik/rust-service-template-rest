//! Closed, transport-neutral catalog of client-visible failure codes.
//!
//! Each transport owns its own projection of a [`Code`]: HTTP maps it to
//! RFC 9457 problem details in `infra_http::problem`, gRPC to a
//! `google.rpc.Status` in `infra_grpc`. The catalog carries no status code,
//! message text, or caller-controlled detail.

use serde::ser::Serializer;

/// Stable machine-readable failure code a client matches on.
///
/// The wire form is the `snake_case` variant name (see [`Code::as_str`]), so
/// renaming a variant changes the public contract.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    strum::Display,
    strum::IntoStaticStr,
    strum::VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum Code {
    BadRequest,
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
    // template:end inbound-webhooks:http-webhook-codes
    Forbidden,
    NotFound,
    MethodNotAllowed,
    Conflict,
    AlreadyExists,
    RequestEntityTooLarge,
    UnprocessableContent,
    TooManyRequests,
    #[strum(serialize = "internal_error")]
    InternalServerError,
    ServiceUnavailable,
    RequestTimeout,
}

impl Code {
    /// Every published code, for transport projections and coverage tests.
    pub const ALL: &'static [Self] = <Self as strum::VariantArray>::VARIANTS;

    /// The stable wire form, for example `bad_request`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
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
    fn wire_forms_are_unique_and_snake_case() {
        let mut wires = std::collections::HashSet::new();
        for code in Code::ALL {
            let wire = code.as_str();
            assert!(wires.insert(wire), "duplicate wire form {wire}");
            assert!(
                wire.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
                "{wire} is not snake_case"
            );
            assert_eq!(code.to_string(), wire);
        }
        assert_eq!(Code::InternalServerError.as_str(), "internal_error");
        assert_eq!(Code::BadRequest.as_str(), "bad_request");
    }
}
