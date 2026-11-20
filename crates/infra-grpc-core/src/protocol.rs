use std::time::Duration;

use http::{HeaderMap, HeaderName, HeaderValue};
use shared_rules::grpc::{MAX_TIMEOUT_DIGITS, TIMEOUT_HEADER, encode_timeout, parse_timeout};

pub const GRPC_TIMEOUT: HeaderName = HeaderName::from_static(TIMEOUT_HEADER);

pub const STATUS_HEADERS: [HeaderName; 3] = [
    tonic::Status::GRPC_STATUS,
    tonic::Status::GRPC_MESSAGE,
    tonic::Status::GRPC_STATUS_DETAILS,
];

#[must_use]
pub fn grpc_timeout(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(GRPC_TIMEOUT)
        .and_then(|value| parse_timeout(value.as_bytes()))
}

pub fn set_grpc_timeout(headers: &mut HeaderMap, remaining: Duration) {
    let mut wire = [0_u8; MAX_TIMEOUT_DIGITS + 1];
    if let Ok(value) = HeaderValue::from_bytes(encode_timeout(remaining, &mut wire)) {
        headers.insert(GRPC_TIMEOUT, value);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RpcName<'a> {
    pub service: &'a str,
    pub method: &'a str,
}

impl std::fmt::Display for RpcName<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.service, self.method)
    }
}

#[must_use]
pub fn split_method_path(path: &str) -> Option<RpcName<'_>> {
    let (service, method) = path.strip_prefix('/')?.split_once('/')?;
    let valid = !service.is_empty() && !method.is_empty() && !method.contains('/');
    valid.then_some(RpcName { service, method })
}

#[must_use]
pub fn header_status_code(headers: &HeaderMap) -> Option<tonic::Code> {
    headers
        .get(tonic::Status::GRPC_STATUS)
        .map(|value| tonic::Code::from_bytes(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_header_round_trips_and_rejects_non_grammar_values() {
        let mut headers = HeaderMap::new();
        assert_eq!(grpc_timeout(&headers), None);
        set_grpc_timeout(&mut headers, Duration::from_millis(250));
        assert_eq!(grpc_timeout(&headers), Some(Duration::from_millis(250)));
        set_grpc_timeout(&mut headers, Duration::MAX);
        assert_eq!(headers[GRPC_TIMEOUT], "99999999H");
        headers.insert(GRPC_TIMEOUT, HeaderValue::from_static("+5S"));
        assert_eq!(grpc_timeout(&headers), None);
    }

    #[test]
    fn split_method_path_requires_service_and_method_segments() {
        assert_eq!(
            split_method_path("/example.v1.EchoService/Unary"),
            Some(RpcName {
                service: "example.v1.EchoService",
                method: "Unary",
            })
        );
        for invalid in [
            "",
            "/",
            "/Service",
            "/Service/",
            "//Method",
            "/a/b/c",
            "Service/Method",
        ] {
            assert_eq!(split_method_path(invalid), None, "{invalid:?}");
        }
    }
}
