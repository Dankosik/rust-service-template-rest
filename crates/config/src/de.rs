//! Serde adapters for field forms serde does not express directly.

use std::net::SocketAddr;

use secrecy::SecretString;
use serde::Deserialize;
use serde::de::{Error as _, Unexpected};

/// Marks a decode message this crate wrote without the value it rejects, so
/// the loader shows it as written. An environment variable cannot contain
/// NUL, so no supplied value can forge the mark.
pub(crate) const VALUE_FREE: char = '\0';

/// What a listen address must be, for the decode message.
const LISTEN_ADDR: &str = "an IP address and port, or :port; hostnames are not resolved";

/// Missing, empty, or whitespace-only text is vacant (`None`); a present
/// value is stored trimmed, as text or as the path it names.
pub(crate) fn blank_as_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: From<String>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| T::from(trimmed.to_owned()))
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
    parse_listen_addr(&raw)
        .ok_or_else(|| D::Error::invalid_value(Unexpected::Str(&raw), &LISTEN_ADDR))
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
            .ok_or_else(|| D::Error::invalid_value(Unexpected::Str(&value), &LISTEN_ADDR)),
        _ => Ok(None),
    }
}

/// Parse an IP listen address: `ip:port`, or the Go-style `:port` meaning
/// IPv4 all-interfaces (`0.0.0.0`). Hostnames are refused; load does not
/// do DNS. Explicit IPv6 forms such as `[::1]:9000` are unchanged.
fn parse_listen_addr(value: &str) -> Option<SocketAddr> {
    let trimmed = value.trim();
    let candidate = match trimmed.strip_prefix(':') {
        Some(port) => format!("0.0.0.0:{port}"),
        None => trimmed.to_owned(),
    };
    candidate.parse().ok()
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
        #[derive(Debug, Deserialize)]
        struct Probe {
            #[serde(deserialize_with = "listen_addr")]
            #[allow(dead_code)]
            addr: SocketAddr,
        }

        assert_eq!(parse_listen_addr("localhost:8080"), None);
        let err = toml::from_str::<Probe>("addr = \"localhost:8080\"").unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("localhost:8080"), "{rendered}");
        assert!(rendered.contains(LISTEN_ADDR), "{rendered}");
    }
}
