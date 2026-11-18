use std::time::Duration;

use http::{HeaderMap, HeaderName};

pub(crate) const GRPC_TIMEOUT: HeaderName = HeaderName::from_static("grpc-timeout");

pub(crate) use shared_rules::grpc::split_method_path;

pub(crate) fn method_path(service: &str, method: &str) -> String {
    format!("/{service}/{method}")
}

/// gRPC over HTTP/2 §Requests and RFC 9110 §§5.6.2, 5.6.4, 5.6.6.
pub(crate) fn native_content_type(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(http::header::CONTENT_TYPE).iter();
    let Some(value) = values.next() else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }
    if *value == tonic::metadata::GRPC_CONTENT_TYPE {
        return true;
    }
    let native_media_type = regex::bytes::regex!(
        r#"(?ix-u)
        \A [\t\x20]* application/grpc (?: \+ [a-z0-9!\x23$%&'*+.^_`|~-]+ )?
        (?:
            [\t\x20]* ; [\t\x20]*
            (?: [a-z0-9!\x23$%&'*+.^_`|~-]+ =
                (?: [a-z0-9!\x23$%&'*+.^_`|~-]+
                  | " (?: [\t\x20-\x21\x23-\x5b\x5d-\x7e\x80-\xff] | \\ [\t\x20-\x7e\x80-\xff] )* "
                )
            )?
        )*
        [\t\x20]* \z"#
    );
    native_media_type.is_match(value.as_bytes())
}

#[must_use]
pub(crate) fn grpc_timeout(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(GRPC_TIMEOUT)
        .and_then(|value| shared_rules::grpc::parse_timeout(value.as_bytes()))
}

pub(crate) fn header_status_code(headers: &HeaderMap) -> Option<tonic::Code> {
    headers
        .get(tonic::Status::GRPC_STATUS)
        .map(|value| tonic::Code::from_bytes(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_timeout_extracts_valid_header_and_ignores_missing_or_malformed() {
        let mut headers = HeaderMap::new();
        assert_eq!(grpc_timeout(&headers), None);
        headers.insert(GRPC_TIMEOUT, http::HeaderValue::from_static("250m"));
        assert_eq!(grpc_timeout(&headers), Some(Duration::from_millis(250)));
        headers.insert(GRPC_TIMEOUT, http::HeaderValue::from_static("+5S"));
        assert_eq!(grpc_timeout(&headers), None);
    }
}
