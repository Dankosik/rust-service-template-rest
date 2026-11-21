use axum::http::header::HeaderName;
use axum::http::{HeaderValue, Request};
use itertools::Itertools as _;
use tower_http::request_id::RequestId;
use uuid::Uuid;

pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

const MAX_REQUEST_ID_BYTES: usize = 128;

fn is_valid(value: &[u8]) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
        && value
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-'))
}

fn generate() -> Option<HeaderValue> {
    let mut buffer = Uuid::encode_buffer();
    let id = Uuid::new_v4().hyphenated().encode_lower(&mut buffer);
    HeaderValue::from_str(id).ok()
}

pub(crate) fn ensure<B>(request: &mut Request<B>) -> Option<RequestId> {
    let valid = match request
        .headers()
        .get_all(&REQUEST_ID_HEADER)
        .iter()
        .at_most_one()
    {
        Ok(Some(value)) if is_valid(value.as_bytes()) => Some(value.clone()),
        _ => None,
    };
    let value = if let Some(value) = valid {
        value
    } else {
        let Some(value) = generate() else {
            request.headers_mut().remove(&REQUEST_ID_HEADER);
            request.extensions_mut().remove::<RequestId>();
            return None;
        };
        request
            .headers_mut()
            .insert(&REQUEST_ID_HEADER, value.clone());
        value
    };
    let id = RequestId::new(value);
    request.extensions_mut().insert(id.clone());
    Some(id)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;

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
            let mut request = with_header(value);
            request
                .extensions_mut()
                .insert(RequestId::new(HeaderValue::from_static("stale-id")));
            let id = ensure(&mut request).unwrap();
            assert_eq!(id.header_value(), value, "{value:?} should be kept");
            assert_eq!(request.headers()[&REQUEST_ID_HEADER], value);
            assert_eq!(
                request
                    .extensions()
                    .get::<RequestId>()
                    .unwrap()
                    .header_value(),
                value
            );
        }
    }

    #[test]
    fn replaces_values_outside_the_grammar_with_uuid_v4() {
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
            let mut request = with_header(value);
            let id = ensure(&mut request).unwrap();
            let text = id.header_value().to_str().unwrap();
            let parsed = Uuid::parse_str(text).expect("invalid value replaced by UUID");
            assert_eq!(parsed.get_version_num(), 4, "{value:?}");
            assert_eq!(request.headers()[&REQUEST_ID_HEADER], text);
            assert_eq!(
                request
                    .extensions()
                    .get::<RequestId>()
                    .unwrap()
                    .header_value(),
                text
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
            let documented = regex::bytes::regex!("(?-u)^[A-Za-z0-9._~-]{1,128}$");
            proptest::prop_assert_eq!(is_valid(&value), documented.is_match(&value));
        }
    }
}
