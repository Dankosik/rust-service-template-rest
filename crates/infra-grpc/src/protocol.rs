use std::time::Duration;

use http::{HeaderMap, HeaderName, StatusCode};

pub(crate) const GRPC_TIMEOUT: HeaderName = HeaderName::from_static("grpc-timeout");
const MAX_TIMEOUT_DIGITS: usize = 8;
const WIRE_TIMEOUT_MAX: Duration = Duration::from_hours(99_999_999);

pub(crate) fn split_method_path(path: &str) -> Option<(&str, &str)> {
    let (service, method) = path.strip_prefix('/')?.split_once('/')?;
    let valid = !service.is_empty() && !method.is_empty() && !method.contains('/');
    valid.then_some((service, method))
}

pub(crate) fn method_path(service: &str, method: &str) -> String {
    format!("/{service}/{method}")
}

#[must_use]
pub fn grpc_timeout(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(GRPC_TIMEOUT)
        .and_then(|value| parse_grpc_timeout(value.as_bytes()))
}

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
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let amount: u64 = std::str::from_utf8(digits).ok()?.parse().ok()?;
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
    const NANOS_PER_UNIT: [(u8, u128); 5] = [
        (b'n', 1),
        (b'u', 1_000),
        (b'm', 1_000_000),
        (b'S', 1_000_000_000),
        (b'M', 60_000_000_000),
    ];
    const MAX_VALUE: u128 = 99_999_999;
    let nanos = remaining.min(WIRE_TIMEOUT_MAX).as_nanos();
    let (unit, amount) = NANOS_PER_UNIT
        .into_iter()
        .map(|(unit, per_unit)| (unit, nanos / per_unit))
        .find(|&(_, amount)| amount <= MAX_VALUE)
        .unwrap_or((b'H', nanos / 3_600_000_000_000));
    let mut buffer = std::fmt::NumBuffer::new();
    let digits = amount.format_into(&mut buffer);
    let mut wire = [0_u8; MAX_TIMEOUT_DIGITS + 1];
    let len = digits.len();
    wire[..len].copy_from_slice(digits.as_bytes());
    wire[len] = unit;
    if let Ok(value) = http::HeaderValue::from_bytes(&wire[..=len]) {
        headers.insert(GRPC_TIMEOUT, value);
    }
}

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
            ("1H", Some(Duration::from_hours(1))),
            ("2M", Some(Duration::from_mins(2))),
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

    #[test]
    fn set_grpc_timeout_round_trips_and_caps_at_wire_maximum() {
        for duration in [
            Duration::ZERO,
            Duration::from_nanos(1),
            Duration::from_nanos(99_999_999),
            Duration::from_millis(100),
            Duration::from_micros(99_999_999),
            Duration::from_millis(250),
            Duration::from_millis(99_999_999),
            Duration::from_secs(5),
            Duration::from_secs(99_999_999),
            Duration::from_mins(99_999_999),
            Duration::from_secs(99_999_999 * 60 + 59),
            Duration::from_mins(100_000_000),
            Duration::from_hours(2),
            WIRE_TIMEOUT_MAX,
            Duration::MAX,
        ] {
            let mut actual = http::HeaderMap::new();
            set_grpc_timeout(&mut actual, duration);
            let mut request = tonic::Request::new(());
            request.set_timeout(duration.min(WIRE_TIMEOUT_MAX));
            let expected = request.into_parts().0.into_headers();
            assert_eq!(
                grpc_timeout(&actual),
                grpc_timeout(&expected),
                "decoded mismatch for {duration:?}: actual={:?} expected={:?}",
                actual.get(GRPC_TIMEOUT),
                expected.get(GRPC_TIMEOUT),
            );
        }
    }
}
