//! Serde adapters for field forms serde does not express directly.

use std::net::SocketAddr;

use secrecy::SecretString;
use serde::Deserialize;

/// Missing, empty, or whitespace-only text is vacant (`None`); a present
/// value is stored trimmed.
pub(crate) fn blank_as_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }))
}

/// Missing, empty, or whitespace-only secret is absent (`None`).
///
/// Trim decides vacancy only; a present secret keeps the deserialized
/// bytes, including padding. Downstream parsers (DSN, OTLP headers) trim
/// their own input.
pub(crate) fn blank_secret_as_none<'de, D>(
    deserializer: D,
) -> Result<Option<SecretString>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| (!value.trim().is_empty()).then(|| SecretString::from(value))))
}

/// Listen address: `ip:port`, or `:port` for IPv4 all-interfaces.
pub(crate) fn listen_addr<'de, D>(deserializer: D) -> Result<SocketAddr, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    parse_listen_addr(&raw).map_err(serde::de::Error::custom)
}

/// Listen address, or `None` when the value is missing, empty, or whitespace.
pub(crate) fn optional_listen_addr<'de, D>(deserializer: D) -> Result<Option<SocketAddr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    match raw {
        Some(value) if !value.trim().is_empty() => parse_listen_addr(&value)
            .map(Some)
            .map_err(serde::de::Error::custom),
        _ => Ok(None),
    }
}

/// Parse an IP listen address: `ip:port`, or the Go-style `:port` meaning
/// IPv4 all-interfaces (`0.0.0.0`). Hostnames are refused; load does not
/// do DNS. Explicit IPv6 forms such as `[::1]:9000` are unchanged.
fn parse_listen_addr(value: &str) -> Result<SocketAddr, String> {
    let trimmed = value.trim();
    let candidate = match trimmed.strip_prefix(':') {
        Some(port) => format!("0.0.0.0:{port}"),
        None => trimmed.to_owned(),
    };
    candidate.parse().map_err(|_| {
        format!(
            "{value:?} is not a socket address (expected an IP address and port, or :port; hostnames are not resolved)"
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
            parse_listen_addr(input).unwrap(),
            expected.parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn socket_addr_rejects_hostnames() {
        let err = parse_listen_addr("localhost:8080").unwrap_err();
        assert!(err.contains("not a socket address"), "{err}");
        assert!(err.contains("localhost:8080"), "{err}");
    }
}
