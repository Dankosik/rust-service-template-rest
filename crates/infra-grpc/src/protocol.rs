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
    let Ok(value) = value.to_str() else {
        return false;
    };
    mediatype::MediaType::parse(value.trim_matches([' ', '\t'])).is_ok_and(|media| {
        let subtype = media.subty.as_str();
        let family = subtype
            .split_once('+')
            .map_or(subtype, |(family, _)| family);
        media.ty == "application"
            && family.eq_ignore_ascii_case("grpc")
            && media
                .params
                .iter()
                .all(|(_, value)| !value.as_str().is_empty())
    })
}

#[must_use]
pub(crate) fn grpc_timeout(headers: &HeaderMap) -> Option<Duration> {
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
