//! Request correlation.
//!
//! `tower_http::request_id` sets and propagates `X-Request-ID`, but trusts
//! any inbound value verbatim. This module strips values outside the accepted
//! grammar before that layer runs, so an attacker cannot inject log or header
//! content, and reads the accepted value back for problem bodies and logs.

use axum::http::header::HeaderName;
use axum::http::{HeaderValue, Request};
use tower_http::request_id::{MakeRequestId, RequestId};
use uuid::Uuid;

/// The correlation header shared with outbound sanitizers.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

const MAX_LEN: usize = 128;

fn is_valid(value: &[u8]) -> bool {
    !value.is_empty()
        && value.len() <= MAX_LEN
        && value
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-'))
}

/// A random UUID v4 per request. tower-http's `MakeRequestUuid` formats a
/// `String` and copies it into the header value; this formats on the stack.
/// `uuid`'s `fast-rng` draws from a thread-local CSPRNG instead of a
/// `getrandom` system call per id.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MakeRequestUuid;

impl MakeRequestId for MakeRequestUuid {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let mut buffer = Uuid::encode_buffer();
        let id = Uuid::new_v4().hyphenated().encode_lower(&mut buffer);
        HeaderValue::from_str(id).ok().map(RequestId::new)
    }
}

/// Remove an inbound `X-Request-ID` that does not match
/// `^[A-Za-z0-9._~-]{1,128}$`, so the next layer generates one.
pub(crate) fn strip_invalid<B>(mut request: Request<B>) -> Request<B> {
    let keep = request
        .headers()
        .get(&REQUEST_ID_HEADER)
        .is_some_and(|value| is_valid(value.as_bytes()));
    if !keep {
        request.headers_mut().remove(&REQUEST_ID_HEADER);
    }
    request
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::HeaderValue;

    use super::*;

    fn with_header(value: &str) -> Request<Body> {
        let mut request = Request::new(Body::empty());
        request
            .headers_mut()
            .insert(&REQUEST_ID_HEADER, HeaderValue::from_str(value).unwrap());
        request
    }

    #[test]
    fn keeps_values_in_the_accepted_grammar() {
        for value in ["abc", "0123456789abcdef", "a.b_c~d-e", &"x".repeat(128)] {
            let request = strip_invalid(with_header(value));
            assert!(
                request.headers().contains_key(&REQUEST_ID_HEADER),
                "{value:?} should be kept"
            );
        }
    }

    #[test]
    fn strips_values_outside_the_grammar() {
        for value in [
            "",
            " ",
            "a b",
            "a/b",
            "a=b",
            "a:b",
            "\"quoted\"",
            &"x".repeat(129),
        ] {
            let request = strip_invalid(with_header(value));
            assert!(
                !request.headers().contains_key(&REQUEST_ID_HEADER),
                "{value:?} should be stripped"
            );
        }
    }
}
