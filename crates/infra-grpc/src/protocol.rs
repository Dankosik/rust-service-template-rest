//! Wire rules for gRPC method paths, timeouts and response status codes.

use std::time::Duration;

use http::{HeaderMap, HeaderName, StatusCode};

pub(crate) const GRPC_TIMEOUT: HeaderName = HeaderName::from_static("grpc-timeout");
/// Most digits a `grpc-timeout` value carries before its unit.
const MAX_TIMEOUT_DIGITS: usize = 8;
/// The largest wire timeout is eight digits of hours. Tonic panics on longer durations.
const WIRE_TIMEOUT_MAX: Duration = Duration::from_hours(99_999_999);

/// Splits a request path `/{service}/{method}` whose parts are non-empty and
/// whose method holds no further `/`.
pub(crate) fn split_method_path(path: &str) -> Option<(&str, &str)> {
    let (service, method) = path.strip_prefix('/')?.split_once('/')?;
    let valid = !service.is_empty() && !method.is_empty() && !method.contains('/');
    valid.then_some((service, method))
}

pub(crate) fn method_path(service: &str, method: &str) -> String {
    format!("/{service}/{method}")
}

/// Parses `grpc-timeout`. A malformed value, including more than eight digits,
/// is absent. Tonic's own parser is private.
#[must_use]
pub fn grpc_timeout(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(GRPC_TIMEOUT)
        .and_then(|value| parse_grpc_timeout(value.as_bytes()))
}

/// [`grpc_timeout`] read from a tonic request's metadata, without converting
/// the whole map into headers.
#[must_use]
pub fn metadata_grpc_timeout(metadata: &tonic::metadata::MetadataMap) -> Option<Duration> {
    metadata
        .get(GRPC_TIMEOUT.as_str())
        .and_then(|value| parse_grpc_timeout(value.as_encoded_bytes()))
}

fn parse_grpc_timeout(value: &[u8]) -> Option<Duration> {
    if value.len() > MAX_TIMEOUT_DIGITS + 1 {
        return None;
    }
    let (&unit, digits) = value.split_last()?;
    // `u64::from_str` accepts a leading `+`; the gRPC grammar allows digits only.
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let amount = std::str::from_utf8(digits).ok()?.parse::<u64>().ok()?;
    match unit {
        b'H' => Some(Duration::from_hours(amount)),
        b'M' => Some(Duration::from_mins(amount)),
        b'S' => Some(Duration::from_secs(amount)),
        b'm' => Some(Duration::from_millis(amount)),
        b'u' => Some(Duration::from_micros(amount)),
        b'n' => Some(Duration::from_nanos(amount)),
        _ => None,
    }
}

pub(crate) fn set_grpc_timeout(headers: &mut HeaderMap, remaining: Duration) {
    // Tonic exposes its encoder through the request API.
    let mut request = tonic::Request::new(());
    request.set_timeout(remaining.min(WIRE_TIMEOUT_MAX));
    let mut encoded = request.into_parts().0.into_headers();
    if let Some(value) = encoded.remove(GRPC_TIMEOUT) {
        headers.insert(GRPC_TIMEOUT, value);
    }
}

/// Decodes the initial response headers using tonic's status rules. If they
/// omit `grpc-status`, the HTTP mapping is provisional until the stream ends.
/// For generated clients, metrics reflect the RPC outcome after decoding the
/// response.
pub fn client_response_code<B>(response: &http::Response<B>) -> Option<tonic::Code> {
    decoded_header_status_code(response.headers()).or_else(|| http_status_code(response.status()))
}

pub(crate) fn decoded_header_status_code(headers: &HeaderMap) -> Option<tonic::Code> {
    if headers.contains_key("grpc-message") || headers.contains_key("grpc-status-details-bin") {
        return tonic::Status::from_header_map(headers).map(|status| status.code());
    }
    header_status_code(headers)
}

pub(crate) fn header_status_code(headers: &HeaderMap) -> Option<tonic::Code> {
    headers
        .get("grpc-status")
        .map(|value| tonic::Code::from_bytes(value.as_bytes()))
}

/// The code a gRPC client infers from an HTTP answer without `grpc-status`,
/// per gRPC's HTTP-to-gRPC status mapping, which tonic's client applies too.
/// `None` for `200`, whose status the trailers still carry.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_timeout_follows_the_protocol_grammar() {
        let cases = [
            ("1H", Some(Duration::from_secs(3600))),
            ("2M", Some(Duration::from_secs(120))),
            ("5S", Some(Duration::from_secs(5))),
            ("250m", Some(Duration::from_millis(250))),
            ("7u", Some(Duration::from_micros(7))),
            ("99999999n", Some(Duration::from_nanos(99_999_999))),
            ("0n", Some(Duration::ZERO)),
            ("123456789S", None),
            ("+5S", None),
            ("-5S", None),
            (" 5S", None),
            ("S", None),
            ("1x", None),
            ("", None),
        ];
        for (value, expected) in cases {
            let mut headers = http::HeaderMap::new();
            headers.insert(GRPC_TIMEOUT, value.parse().expect("ASCII value"));
            assert_eq!(grpc_timeout(&headers), expected, "{value:?}");
        }
        assert_eq!(grpc_timeout(&http::HeaderMap::new()), None);
    }

    #[test]
    fn metadata_timeout_reads_the_grpc_timeout_entry() {
        let mut metadata = tonic::metadata::MetadataMap::new();
        assert_eq!(metadata_grpc_timeout(&metadata), None);
        metadata.insert("grpc-timeout", "250m".parse().expect("ASCII value"));
        assert_eq!(
            metadata_grpc_timeout(&metadata),
            Some(Duration::from_millis(250))
        );
    }
}
