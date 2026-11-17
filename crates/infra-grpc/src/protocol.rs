use std::time::Duration;

use http::{HeaderMap, HeaderName};

pub(crate) const GRPC_TIMEOUT: HeaderName = HeaderName::from_static("grpc-timeout");
pub(crate) const GRPC_STATUS: HeaderName = HeaderName::from_static("grpc-status");
pub(crate) const GRPC_MESSAGE: HeaderName = HeaderName::from_static("grpc-message");
pub(crate) const GRPC_STATUS_DETAILS: HeaderName =
    HeaderName::from_static("grpc-status-details-bin");

pub(crate) use shared_rules::grpc::split_method_path;

pub(crate) fn method_path(service: &str, method: &str) -> String {
    format!("/{service}/{method}")
}

#[must_use]
pub fn grpc_timeout(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(GRPC_TIMEOUT)
        .and_then(|value| shared_rules::grpc::parse_timeout(value.as_bytes()))
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
    fn grpc_timeout_extracts_valid_header_and_ignores_missing_or_malformed() {
        let mut headers = HeaderMap::new();
        assert_eq!(grpc_timeout(&headers), None);
        headers.insert(GRPC_TIMEOUT, http::HeaderValue::from_static("250m"));
        assert_eq!(grpc_timeout(&headers), Some(Duration::from_millis(250)));
        headers.insert(GRPC_TIMEOUT, http::HeaderValue::from_static("+5S"));
        assert_eq!(grpc_timeout(&headers), None);
    }
}
