use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;
use serde::de::Unexpected;

pub(crate) const VALUE_FREE: char = '\0';

#[allow(
    dead_code,
    reason = "removing every profile with a value-free decoder leaves the constructor without a caller"
)]
pub(crate) fn value_free<E: serde::de::Error>(message: impl std::fmt::Display) -> E {
    E::custom(format_args!("{VALUE_FREE}{message}"))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Scalar<T> {
    Typed(T),
    Text(String),
}

#[allow(
    dead_code,
    reason = "removing OIDC introspection and outbound authentication leaves no caller"
)]
pub(crate) fn deserialize_scalar<'de, D, T>(deserializer: D, message: &str) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + FromStr,
{
    match Scalar::<T>::deserialize(deserializer).map_err(|_| value_free(message))? {
        Scalar::Typed(value) => Ok(value),
        Scalar::Text(value) => value.parse().map_err(|_| value_free(message)),
    }
}

impl<T: FromStr> Scalar<T> {
    fn into_count<E: serde::de::Error>(self) -> Result<T, E> {
        match self {
            Self::Typed(value) => Ok(value),
            Self::Text(value) => value.parse().map_err(|_| invalid_count()),
        }
    }
}

fn invalid_count<E: serde::de::Error>() -> E {
    E::invalid_value(
        Unexpected::Other("a malformed or out-of-range count"),
        &"an integer",
    )
}

pub(crate) fn unsigned_count<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + FromStr,
{
    Scalar::deserialize(deserializer)
        .map_err(|_| invalid_count::<D::Error>())?
        .into_count()
}

pub(crate) fn optional_unsigned_count<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + FromStr,
{
    Option::<Scalar<T>>::deserialize(deserializer)
        .map_err(|_| invalid_count::<D::Error>())?
        .map(Scalar::into_count)
        .transpose()
}

fn invalid_ratio<E: serde::de::Error>() -> E {
    E::invalid_value(Unexpected::Other("a malformed number"), &"a number")
}

pub(crate) fn ratio<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Scalar::<f64>::deserialize(deserializer).map_err(|_| invalid_ratio::<D::Error>())? {
        Scalar::Typed(value) => Ok(value),
        Scalar::Text(value) => value.trim().parse().map_err(|_| invalid_ratio()),
    }
}

const LISTEN_ADDR_FORM: &str = "an IP address and port, or :port; hostnames are not resolved";

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

pub(crate) fn blank_secret_as_none<'de, D>(
    deserializer: D,
) -> Result<Option<SecretString>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| (!value.trim().is_empty()).then(|| SecretString::from(value))))
}

pub(crate) fn listen_addr<'de, D>(deserializer: D) -> Result<SocketAddr, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    parse_listen_addr(&raw).ok_or_else(|| invalid_listen_addr(&raw))
}

pub(crate) fn optional_listen_addr<'de, D>(deserializer: D) -> Result<Option<SocketAddr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    blank_as_none::<_, String>(deserializer)?
        .map(|value| parse_listen_addr(&value).ok_or_else(|| invalid_listen_addr(&value)))
        .transpose()
}

fn invalid_listen_addr<E: serde::de::Error>(value: &str) -> E {
    E::invalid_value(Unexpected::Str(value), &LISTEN_ADDR_FORM)
}

fn parse_listen_addr(value: &str) -> Option<SocketAddr> {
    let trimmed = value.trim();
    match trimmed.strip_prefix(':') {
        Some(port) => format!("0.0.0.0:{port}").parse(),
        None => trimmed.parse(),
    }
    .ok()
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
            #[expect(dead_code, reason = "the probe only checks that deserialization fails")]
            addr: SocketAddr,
        }

        assert_eq!(parse_listen_addr("localhost:8080"), None);
        let err = toml::from_str::<Probe>("addr = \"localhost:8080\"").unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("localhost:8080"), "{rendered}");
        assert!(rendered.contains(LISTEN_ADDR_FORM), "{rendered}");
    }
}

pub(crate) fn duration<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let text = String::deserialize(deserializer)?;
    parse_duration(&text)
        .ok_or_else(|| serde::de::Error::invalid_value(Unexpected::Str(&text), &"a duration"))
}

pub(crate) fn parse_duration(text: &str) -> Option<Duration> {
    let span: jiff::Span = text.trim().parse().ok()?;
    let signed = span
        .to_duration(jiff::SpanRelativeTo::days_are_24_hours())
        .ok()?;
    Duration::try_from(signed).ok()
}

pub(crate) fn display_duration(duration: Duration) -> String {
    jiff::fmt::friendly::SpanPrinter::new().unsigned_duration_to_string(&duration)
}
