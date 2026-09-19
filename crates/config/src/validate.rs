//! Validation error and the range helpers every section shares.

use std::time::Duration;

/// One violated configuration rule, named by the key an operator would set.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{key}: {message}")]
pub struct ValidationError {
    /// Dotted key, for example `http.request_timeout`.
    pub key: String,
    /// What the value had to satisfy.
    pub message: String,
}

impl ValidationError {
    pub(crate) fn new(key: &str, message: impl Into<String>) -> Self {
        Self {
            key: key.to_owned(),
            message: message.into(),
        }
    }
}

pub(crate) fn duration_range(
    key: &str,
    value: Duration,
    min: Duration,
    max: Duration,
) -> Result<(), ValidationError> {
    if value < min || value > max {
        return Err(ValidationError::new(
            key,
            format!(
                "must be between {} and {}, got {}",
                humantime::format_duration(min),
                humantime::format_duration(max),
                humantime::format_duration(value)
            ),
        ));
    }
    Ok(())
}

pub(crate) fn int_range(key: &str, value: u64, min: u64, max: u64) -> Result<(), ValidationError> {
    if value < min || value > max {
        return Err(ValidationError::new(
            key,
            format!("must be between {min} and {max}, got {value}"),
        ));
    }
    Ok(())
}

pub(crate) fn non_empty(key: &str, value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new(key, "cannot be empty"));
    }
    Ok(())
}

/// Parse `host:port`, or the Go-style `:port` meaning IPv4 all-interfaces
/// (`0.0.0.0`). Explicit IPv6 forms such as `[::1]:9000` are unchanged.
pub(crate) fn socket_addr(key: &str, value: &str) -> Result<std::net::SocketAddr, ValidationError> {
    let trimmed = value.trim();
    let candidate = match trimmed.strip_prefix(':') {
        Some(port) => format!("0.0.0.0:{port}"),
        None => trimmed.to_owned(),
    };
    candidate.parse().map_err(|_| {
        ValidationError::new(
            key,
            format!("{value:?} is not a socket address (expected host:port or :port)"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::port_only(":8080", "0.0.0.0:8080")]
    #[case::ipv6("[::1]:9000", "[::1]:9000")]
    #[case::ipv4("127.0.0.1:8080", "127.0.0.1:8080")]
    #[case::trimmed("  :8080  ", "0.0.0.0:8080")]
    fn socket_addr_accepts_supported_forms(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(
            socket_addr("k", input).unwrap(),
            expected.parse::<std::net::SocketAddr>().unwrap()
        );
    }

    #[rstest::rstest]
    #[case::below_minimum(0, false)]
    #[case::minimum(1, true)]
    #[case::maximum(10, true)]
    #[case::above_maximum(11, false)]
    fn int_range_is_inclusive(#[case] value: u64, #[case] valid: bool) {
        assert_eq!(int_range("limit", value, 1, 10).is_ok(), valid);
    }

    #[rstest::rstest]
    #[case::below_minimum(99, false)]
    #[case::minimum(100, true)]
    #[case::maximum(600_000, true)]
    #[case::above_maximum(600_001, false)]
    fn duration_range_is_inclusive(#[case] milliseconds: u64, #[case] valid: bool) {
        let result = duration_range(
            "http.request_timeout",
            Duration::from_millis(milliseconds),
            Duration::from_millis(100),
            Duration::from_secs(600),
        );
        assert_eq!(result.is_ok(), valid);
    }

    #[test]
    fn socket_addr_rejects_hostnames() {
        let err = socket_addr("http.addr", "localhost:8080").unwrap_err();
        assert_eq!(err.key, "http.addr");
        assert!(err.message.contains("not a socket address"));
    }

    #[test]
    fn duration_range_names_bounds() {
        let err = duration_range(
            "http.request_timeout",
            Duration::from_millis(10),
            Duration::from_millis(100),
            Duration::from_secs(600),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "http.request_timeout: must be between 100ms and 10m, got 10ms"
        );
    }
}
