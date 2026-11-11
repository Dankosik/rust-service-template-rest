//! The gRPC wire grammar the server and the client both read: the
//! `grpc-timeout` header and the `/{service}/{method}` request path.

use std::time::Duration;

pub(crate) const GRPC_TIMEOUT: http::HeaderName = http::HeaderName::from_static("grpc-timeout");
/// Most digits a `grpc-timeout` value carries before its unit.
pub(crate) const MAX_TIMEOUT_DIGITS: usize = 8;

/// Splits a request path `/{service}/{method}` whose parts are non-empty and
/// whose method holds no further `/`.
pub(crate) fn split_method_path(path: &str) -> Option<(&str, &str)> {
    let (service, method) = path.strip_prefix('/')?.split_once('/')?;
    let valid = !service.is_empty() && !method.is_empty() && !method.contains('/');
    valid.then_some((service, method))
}

/// The request path of `method` on `service`.
pub(crate) fn method_path(service: &str, method: &str) -> String {
    format!("/{service}/{method}")
}

/// Parses `grpc-timeout`. A malformed value, including more than eight digits,
/// is absent. Tonic's own parser is private.
#[must_use]
pub fn grpc_timeout(headers: &http::HeaderMap) -> Option<Duration> {
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
    // The unit follows the digits.
    if value.is_empty() || value.len() > MAX_TIMEOUT_DIGITS + 1 {
        return None;
    }
    let (&unit, digits) = value.split_last()?;
    // `u64::from_str` accepts a leading `+`; the gRPC grammar allows digits only.
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let amount = std::str::from_utf8(digits).ok()?.parse::<u64>().ok()?;
    match unit {
        b'H' => Some(Duration::from_secs(amount.checked_mul(60 * 60)?)),
        b'M' => Some(Duration::from_secs(amount.checked_mul(60)?)),
        b'S' => Some(Duration::from_secs(amount)),
        b'm' => Some(Duration::from_millis(amount)),
        b'u' => Some(Duration::from_micros(amount)),
        b'n' => Some(Duration::from_nanos(amount)),
        _ => None,
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
