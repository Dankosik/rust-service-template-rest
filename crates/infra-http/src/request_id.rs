//! Request correlation.
//!
//! [`SetRequestIdLayer`](tower_http::request_id::SetRequestIdLayer) trusts the
//! inbound header and any existing [`RequestId`] extension. Sanitize both
//! before it establishes the ID that observation echoes in the response header
//! and uses for Problem bodies and logs.

use axum::http::header::HeaderName;
use axum::http::{HeaderValue, Request};
use tower_http::request_id::{MakeRequestId, RequestId};
use uuid::Uuid;

/// The correlation header this crate accepts, generates, and echoes.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

const MAX_REQUEST_ID_BYTES: usize = 128;

fn is_valid(value: &[u8]) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
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

/// Keep exactly one inbound `X-Request-ID` matching `^[A-Za-z0-9._~-]{1,128}$`.
pub(crate) fn sanitize<B>(mut request: Request<B>) -> Request<B> {
    let mut values = request.headers().get_all(&REQUEST_ID_HEADER).iter();
    let keep = values
        .next()
        .is_some_and(|value| is_valid(value.as_bytes()))
        && values.next().is_none();
    if !keep {
        request.headers_mut().remove(&REQUEST_ID_HEADER);
    }
    request.extensions_mut().remove::<RequestId>();
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
            let request = sanitize(with_header(value));
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
            let request = sanitize(with_header(value));
            assert!(
                !request.headers().contains_key(&REQUEST_ID_HEADER),
                "{value:?} should be stripped"
            );
        }
    }

    use proptest::strategy::Strategy as _;

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(128))]

        #[test]
        fn validity_is_the_documented_pattern(
            value in proptest::prop_oneof![
                proptest::collection::vec(proptest::prelude::any::<u8>(), 0..160),
                "[A-Za-z0-9._~-]{0,140}".prop_map(String::into_bytes),
            ],
        ) {
            let documented = regex::bytes::Regex::new(r"(?-u)^[A-Za-z0-9._~-]{1,128}$").unwrap();
            proptest::prop_assert_eq!(is_valid(&value), documented.is_match(&value));
        }
    }
}
