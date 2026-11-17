use std::time::Duration;

use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use shared_rules::grpc::{MAX_TIMEOUT_DIGITS, encode_timeout, parse_timeout};

pub(crate) const GRPC_TIMEOUT: HeaderName = HeaderName::from_static("grpc-timeout");
pub(crate) const GRPC_STATUS: HeaderName = HeaderName::from_static("grpc-status");
pub(crate) const GRPC_MESSAGE: HeaderName = HeaderName::from_static("grpc-message");
pub(crate) const GRPC_STATUS_DETAILS: HeaderName =
    HeaderName::from_static("grpc-status-details-bin");

#[must_use]
pub fn grpc_timeout(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(GRPC_TIMEOUT)
        .and_then(|value| parse_timeout(value.as_bytes()))
}

pub(crate) fn set_grpc_timeout(headers: &mut HeaderMap, remaining: Duration) {
    let mut wire = [0_u8; MAX_TIMEOUT_DIGITS + 1];
    if let Ok(value) = HeaderValue::from_bytes(encode_timeout(remaining, &mut wire)) {
        headers.insert(GRPC_TIMEOUT, value);
    }
}

pub(crate) fn http_status_code(status: StatusCode) -> Option<tonic::Code> {
    match status {
        StatusCode::OK => None,
        StatusCode::BAD_REQUEST => Some(tonic::Code::Internal),
        StatusCode::UNAUTHORIZED => Some(tonic::Code::Unauthenticated),
        StatusCode::FORBIDDEN => Some(tonic::Code::PermissionDenied),
        StatusCode::NOT_FOUND => Some(tonic::Code::Unimplemented),
        StatusCode::TOO_MANY_REQUESTS
        | StatusCode::BAD_GATEWAY
        | StatusCode::SERVICE_UNAVAILABLE
        | StatusCode::GATEWAY_TIMEOUT => Some(tonic::Code::Unavailable),
        _ => Some(tonic::Code::Unknown),
    }
}

pub(crate) fn decoded_header_status_code(headers: &HeaderMap) -> Option<tonic::Code> {
    if headers.contains_key(GRPC_MESSAGE) || headers.contains_key(GRPC_STATUS_DETAILS) {
        return tonic::Status::from_header_map(headers).map(|status| status.code());
    }
    header_status_code(headers)
}

pub(crate) fn header_status_code(headers: &HeaderMap) -> Option<tonic::Code> {
    headers
        .get(GRPC_STATUS)
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
    fn http_and_decoded_header_status_codes_follow_grpc_mappings() {
        assert_eq!(http_status_code(StatusCode::OK), None);
        assert_eq!(
            http_status_code(StatusCode::UNAUTHORIZED),
            Some(tonic::Code::Unauthenticated)
        );
        assert_eq!(
            http_status_code(StatusCode::SERVICE_UNAVAILABLE),
            Some(tonic::Code::Unavailable)
        );
        let mut headers = HeaderMap::new();
        headers.insert(GRPC_STATUS, HeaderValue::from_static("14"));
        assert_eq!(
            decoded_header_status_code(&headers),
            Some(tonic::Code::Unavailable)
        );
        headers.insert(GRPC_MESSAGE, HeaderValue::from_static("%FF"));
        assert_eq!(
            decoded_header_status_code(&headers),
            Some(tonic::Code::Unknown)
        );
    }
}
